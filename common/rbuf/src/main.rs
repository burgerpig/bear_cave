// main.rs
use shared_memory::{Shmem, ShmemConf};
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::mem::{self, MaybeUninit};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

// The header that lives at the start of the shared memory
#[repr(C)]
pub struct RingBufferHeader {
    head: AtomicUsize,
    tail: AtomicUsize,
    capacity: usize,
}

// A handle that gives safe access to the shared memory region
struct ShmemRingBuffer<T> {
    shmem: Shmem,
    header: *const RingBufferHeader,
    buffer: *mut UnsafeCell<MaybeUninit<T>>,
    _phantom: PhantomData<T>,
}

// This is unsafe because we are dealing with raw pointers and shared memory.
// The user of this module must ensure that access is properly synchronized.
unsafe impl<T: Send> Send for ShmemRingBuffer<T> {}
unsafe impl<T: Sync> Sync for ShmemRingBuffer<T> {}

impl<T> ShmemRingBuffer<T> {
    fn header(&self) -> &RingBufferHeader {
        unsafe { &*self.header }
    }

    fn buffer_ptr(&self, index: usize) -> *mut T {
        unsafe {
            let cell_ptr = self.buffer.add(index);
            (*cell_ptr).get() as *mut T
        }
    }
}

// --- Producer and Consumer handles ---

pub struct Producer<T> {
    rb: ShmemRingBuffer<T>,
}

pub struct Consumer<T> {
    rb: ShmemRingBuffer<T>,
}

// --- Producer Logic ---

impl<T> Producer<T> {
    pub fn open(name: &str) -> Result<Self, String> {
        let shmem = ShmemConf::new()
            .os_id(name)
            .open()
            .map_err(|e| e.to_string())?;
        Ok(Self::from_shmem(shmem))
    }
    
    fn from_shmem(shmem: Shmem) -> Self {
        let header = shmem.as_ptr() as *const RingBufferHeader;
        let buffer = unsafe { shmem.as_ptr().add(mem::size_of::<RingBufferHeader>()) }
            as *mut UnsafeCell<MaybeUninit<T>>;
        
        Self {
            rb: ShmemRingBuffer { shmem, header, buffer, _phantom: PhantomData }
        }
    }

    pub fn push(&self, item: T) -> Result<(), T> {
        let header = self.rb.header();
        let head = header.head.load(Ordering::Relaxed);
        let tail = header.tail.load(Ordering::Acquire);
        let next_tail = (tail + 1) % header.capacity;

        if next_tail == head {
            return Err(item); // Buffer is full
        }

        unsafe {
            // Write the data into the buffer slot
            self.rb.buffer_ptr(tail).write(item);
        }

        // Publish the write
        header.tail.store(next_tail, Ordering::Release);
        Ok(())
    }
}

// --- Consumer Logic ---

impl<T> Consumer<T> {
    pub fn create(name: &str, capacity: usize) -> Result<Self, String> {
        // We add 1 to capacity for the empty/full check
        let real_capacity = capacity + 1;
        let shmem_size =
            mem::size_of::<RingBufferHeader>() + real_capacity * mem::size_of::<T>();

        let shmem = ShmemConf::new()
            .size(shmem_size)
            .os_id(name)
            .create()
            .map_err(|e| e.to_string())?;
            
        // Initialize the header in the shared memory
        unsafe {
            let header_ptr = shmem.as_ptr() as *mut RingBufferHeader;
            (*header_ptr).head = AtomicUsize::new(0);
            (*header_ptr).tail = AtomicUsize::new(0);
            (*header_ptr).capacity = real_capacity;
        }

        Ok(Self::from_shmem(shmem))
    }
    
    fn from_shmem(shmem: Shmem) -> Self {
        let header = shmem.as_ptr() as *const RingBufferHeader;
        let buffer = unsafe { shmem.as_ptr().add(mem::size_of::<RingBufferHeader>()) }
            as *mut UnsafeCell<MaybeUninit<T>>;
        
        Self {
            rb: ShmemRingBuffer { shmem, header, buffer, _phantom: PhantomData }
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        let header = self.rb.header();
        let head = header.head.load(Ordering::Relaxed);
        let tail = header.tail.load(Ordering::Acquire);

        if head == tail {
            return None; // Buffer is empty
        }

        let item = unsafe {
            // Read the data from the buffer slot
            self.rb.buffer_ptr(head).read()
        };

        // Publish the read by advancing the head
        header.head.store((head + 1) % header.capacity, Ordering::Release);
        Some(item)
    }
}

// --- Main execution logic ---

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: program <creator|producer>");
        return;
    }

    const SHMEM_ID: &str = "my_mpsc_ring_buffer";

    match args[1].as_str() {
        "creator" => {
            println!("[Creator/Consumer] Starting...");
            let mut consumer = Consumer::<u32>::create(SHMEM_ID, 10).expect("Failed to create consumer");
            println!("[Creator/Consumer] Shared memory created. Waiting for producers.");
            
            let mut received_count = 0;
            loop {
                if let Some(val) = consumer.pop() {
                    println!("[Consumer] Popped: {}", val);
                    received_count += 1;
                    if received_count == 20 { // Exit after 20 messages
                        break;
                    }
                } else {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            println!("[Creator/Consumer] Done.");
        }
        "producer" => {
            println!("[Producer] Starting...");
            // Wait a moment for the creator to set up
            thread::sleep(Duration::from_millis(500));
            
            let producer = Producer::<u32>::open(SHMEM_ID).expect("Failed to open producer");
            println!("[Producer] Attached to shared memory.");
            
            for i in 0..10 {
                println!("[Producer] Pushing {}", i);
                while producer.push(i).is_err() {
                    println!("[Producer] Buffer full, retrying...");
                    thread::sleep(Duration::from_millis(50));
                }
                thread::sleep(Duration::from_millis(200));
            }
            println!("[Producer] Done.");
        }
        _ => {
            println!("Invalid argument. Use 'creator' or 'producer'.");
        }
    }
}