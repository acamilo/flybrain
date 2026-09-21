//! A persistent worker pool for the per-tick parallel phases.
//!
//! The kernel's parallel phases are short — tens of microseconds — and there are two of them per
//! 1-ms tick, so a thousand brain-milliseconds is two thousand fan-outs per second. A work-stealing
//! pool pays for flexibility this kernel does not want: the partition is fixed, every worker's
//! share is known before the phase starts, and there is nothing to steal. Rayon's per-tick joins
//! measured *slower* than the sequential kernel above two threads.
//!
//! So: one thread per worker, spawned once, parked on a generation counter. A dispatch bumps the
//! counter, the workers run the phase over their own index, and the dispatcher waits for the
//! count of runners to drain. Workers spin before parking, because the gap between two phases is
//! the length of a sequential phase — a few microseconds — and a futex round trip is a large
//! fraction of that.
//!
//! The pool decides *who* runs *which* index. It has nothing to do with what a phase computes, so
//! nothing here can affect a result; see [`WorkerPool::broadcast`].

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// `spin_loop` iterations before a worker starts yielding. Roughly a microsecond.
const SPINS: u32 = 4_096;
/// `yield_now` calls after that before a worker parks on the condvar.
const YIELDS: u32 = 256;

type Job = dyn Fn(usize) + Sync;

struct Shared {
    /// Bumped once per dispatch. A worker runs when this differs from what it last saw.
    generation: AtomicUsize,
    /// Workers still inside the current job, excluding the dispatching thread.
    running: AtomicUsize,
    /// Workers blocked on `signal`, so a dispatch takes the lock only when it has to.
    parked: AtomicUsize,
    /// Set when a job panicked, so the dispatcher can re-raise instead of reporting success.
    poisoned: AtomicBool,
    shutdown: AtomicBool,
    /// The current job. Valid from the generation bump until `running` reaches zero; published by
    /// the release on `generation` and read after the matching acquire.
    job: UnsafeCell<Option<*const Job>>,
    gate: Mutex<()>,
    signal: Condvar,
    workers: usize,
}

// SAFETY: `job` is written only by a dispatcher, between bumping `generation` and waiting for
// `running` to reach zero, and read only by a worker that has observed that bump. `broadcast` does
// not return until every reader is done, so no worker can observe a stale or freed pointer.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// A fixed set of worker threads that run one indexed closure at a time.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for WorkerPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerPool")
            .field("workers", &self.shared.workers)
            .finish()
    }
}

impl WorkerPool {
    /// Spawn `workers - 1` threads; worker 0 is always the dispatching thread, so a one-worker
    /// pool spawns nothing and every dispatch runs inline.
    pub fn new(workers: usize, name: &str) -> std::io::Result<Self> {
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            generation: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            poisoned: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            job: UnsafeCell::new(None),
            gate: Mutex::new(()),
            signal: Condvar::new(),
            workers,
        });
        let mut threads = Vec::with_capacity(workers - 1);
        for index in 1..workers {
            let shared = Arc::clone(&shared);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("{name}-{index}"))
                    .spawn(move || worker_loop(&shared, index))?,
            );
        }
        Ok(Self { shared, threads })
    }

    pub fn workers(&self) -> usize {
        self.shared.workers
    }

    /// Run `job(worker)` once for every `worker` in `0..workers()` and return when all have
    /// finished. Index 0 runs on the calling thread.
    ///
    /// The pool guarantees only that each index runs exactly once; it never chooses *what* an
    /// index does. A phase is therefore as deterministic as its partition, and the partitions this
    /// kernel uses depend on nothing but the dataset and the worker count.
    ///
    /// Panics from any worker are re-raised here, so a panicking kernel fails a test instead of
    /// hanging the pool.
    pub fn broadcast(&self, job: &(dyn Fn(usize) + Sync)) {
        if self.shared.workers == 1 {
            job(0);
            return;
        }
        let shared = &self.shared;
        // SAFETY: the erased pointer is read only while this call is on the stack — see the
        // `unsafe impl` above — so widening its lifetime cannot let a worker outlive `job`.
        let borrowed: *const (dyn Fn(usize) + Sync + '_) = job;
        let erased: *const Job = unsafe {
            std::mem::transmute::<*const (dyn Fn(usize) + Sync + '_), *const Job>(borrowed)
        };
        unsafe { *shared.job.get() = Some(erased) };
        shared.running.store(shared.workers - 1, Ordering::Relaxed);
        shared.generation.fetch_add(1, Ordering::SeqCst);
        if shared.parked.load(Ordering::SeqCst) > 0 {
            let _guard = shared
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            shared.signal.notify_all();
        }

        // Run index 0 here. A panic must not skip the drain: the workers are still holding the
        // pointer into this stack frame.
        let local = std::panic::catch_unwind(AssertUnwindSafe(|| job(0)));

        let mut spins = 0u32;
        while shared.running.load(Ordering::Acquire) != 0 {
            spins += 1;
            if spins <= SPINS {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        unsafe { *shared.job.get() = None };

        if let Err(payload) = local {
            std::panic::resume_unwind(payload);
        }
        if shared.poisoned.swap(false, Ordering::AcqRel) {
            panic!("a flysim worker panicked");
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        {
            let _guard = self
                .shared
                .gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.shared.signal.notify_all();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn worker_loop(shared: &Shared, index: usize) {
    let mut seen = 0usize;
    loop {
        let mut spins = 0u32;
        loop {
            let generation = shared.generation.load(Ordering::Acquire);
            if generation != seen {
                seen = generation;
                break;
            }
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            spins += 1;
            if spins <= SPINS {
                std::hint::spin_loop();
            } else if spins <= SPINS + YIELDS {
                std::thread::yield_now();
            } else {
                // Park. `parked` is incremented under the lock and the wait condition is
                // re-checked after acquiring it, so a dispatch that bumped the generation before
                // reading `parked` cannot be missed.
                let mut guard = shared
                    .gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                shared.parked.fetch_add(1, Ordering::SeqCst);
                while shared.generation.load(Ordering::Acquire) == seen
                    && !shared.shutdown.load(Ordering::Acquire)
                {
                    guard = shared
                        .signal
                        .wait(guard)
                        .unwrap_or_else(|error| error.into_inner());
                }
                shared.parked.fetch_sub(1, Ordering::SeqCst);
                drop(guard);
                spins = 0;
            }
        }
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        // SAFETY: the generation load above is an acquire paired with the dispatcher's release, so
        // the job pointer is published and stays valid until `running` is decremented below.
        let job = unsafe { (*shared.job.get()).expect("a dispatched job") };
        if std::panic::catch_unwind(AssertUnwindSafe(|| unsafe { (*job)(index) })).is_err() {
            shared.poisoned.store(true, Ordering::Release);
        }
        shared.running.fetch_sub(1, Ordering::Release);
    }
}

/// A mutable slice that workers may split between themselves.
///
/// The kernel's parallel phases write disjoint contiguous ranges of the same array. The borrow
/// checker can prove that with `split_at_mut` when the shards are handed out as values, but not
/// when each worker derives its own range from its index inside a shared `Fn`. This is the escape
/// hatch, and the obligation it moves onto the caller is exactly one sentence: no two live borrows
/// may overlap.
pub struct SharedSlice<'a, T> {
    ptr: *mut T,
    len: usize,
    marker: PhantomData<&'a mut [T]>,
}

// SAFETY: the type hands out `&mut [T]` only through an unsafe method whose contract is that the
// caller keeps the ranges disjoint, which makes it no more shareable than `&mut [T]` already is.
unsafe impl<T: Send> Send for SharedSlice<'_, T> {}
unsafe impl<T: Send> Sync for SharedSlice<'_, T> {}

impl<'a, T> SharedSlice<'a, T> {
    pub fn new(slice: &'a mut [T]) -> Self {
        Self {
            ptr: slice.as_mut_ptr(),
            len: slice.len(),
            marker: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The sub-slice `from..to`.
    ///
    /// # Safety
    ///
    /// `from <= to <= len()`, and no other live borrow taken from this `SharedSlice` may overlap
    /// `from..to`.
    #[inline]
    pub unsafe fn range(&self, from: usize, to: usize) -> &'a mut [T] {
        debug_assert!(from <= to && to <= self.len);
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(from), to - from) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_index_runs_exactly_once() {
        for workers in [1usize, 2, 3, 7] {
            let pool = WorkerPool::new(workers, "test").expect("a pool");
            assert_eq!(pool.workers(), workers);
            let seen: Vec<AtomicUsize> = (0..workers).map(|_| AtomicUsize::new(0)).collect();
            // Several dispatches, so the park/wake path is exercised as well as the spin path.
            for _ in 0..64 {
                pool.broadcast(&|worker| {
                    seen[worker].fetch_add(1, Ordering::Relaxed);
                });
            }
            for (worker, count) in seen.iter().enumerate() {
                assert_eq!(count.load(Ordering::Relaxed), 64, "worker {worker}");
            }
        }
    }

    #[test]
    fn disjoint_shards_write_the_whole_array() {
        let pool = WorkerPool::new(4, "test").expect("a pool");
        let mut values = vec![0u32; 1000];
        let bounds = [0usize, 10, 10, 999, 1000];
        {
            let shared = SharedSlice::new(&mut values);
            pool.broadcast(&|worker| {
                let (from, to) = (bounds[worker], bounds[worker + 1]);
                // SAFETY: `bounds` is ascending, so the ranges are disjoint.
                let shard = unsafe { shared.range(from, to) };
                for (index, slot) in shard.iter_mut().enumerate() {
                    *slot = (from + index) as u32;
                }
            });
        }
        assert!(values
            .iter()
            .enumerate()
            .all(|(index, v)| *v == index as u32));
    }

    #[test]
    fn a_worker_panic_reaches_the_dispatcher() {
        let pool = WorkerPool::new(3, "test").expect("a pool");
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            pool.broadcast(&|worker| {
                if worker == 2 {
                    panic!("worker 2 says no");
                }
            });
        }));
        assert!(result.is_err(), "the panic must not be swallowed");
        // The pool has to keep working afterwards.
        let ran = AtomicUsize::new(0);
        pool.broadcast(&|_| {
            ran.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(ran.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_parked_pool_wakes_up() {
        let pool = WorkerPool::new(4, "test").expect("a pool");
        let ran = AtomicUsize::new(0);
        pool.broadcast(&|_| {
            ran.fetch_add(1, Ordering::Relaxed);
        });
        // Long enough that every worker has given up spinning and parked.
        std::thread::sleep(std::time::Duration::from_millis(50));
        pool.broadcast(&|_| {
            ran.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(ran.load(Ordering::Relaxed), 8);
    }
}
