// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Worker pool for parallel contour search. Workers spin between jobs, yield
//! after prolonged idling, and stop when the pool is dropped.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// A job: `run(i)` for each task i < count.
struct Job {
    /// The function of the caller of `Pool::run`, which borrows its data: it's
    /// only called for the tasks claimed before they're all finished, and
    /// `Pool::run` waits for this (so the data outlives the calls).
    run: *const (dyn Fn(usize) + Sync),
    count: usize,
    /// The next task to claim, and the number of tasks not finished.
    next: AtomicUsize,
    pending: AtomicUsize,
    panicked: AtomicBool,
}

// The function is `Sync`, and only called while it's alive (see `run`).
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    /// Runs tasks until there are none left to claim.
    fn work(&self) {
        loop {
            let i = self.next.fetch_add(1, Ordering::Relaxed);
            if i >= self.count {
                return;
            }
            // SAFETY: the task i < count is claimed, so it's not finished and
            // `Pool::run` is still waiting: the function is alive.
            let run = unsafe { &*self.run };
            if panic::catch_unwind(AssertUnwindSafe(|| run(i))).is_err() {
                self.panicked.store(true, Ordering::Relaxed);
            }
            self.pending.fetch_sub(1, Ordering::Release);
        }
    }
}

struct Shared {
    /// Incremented for each job.
    epoch: AtomicU64,
    job: Mutex<Option<Arc<Job>>>,
    stop: AtomicBool,
}

/// A pool of spinning threads (they stop when it's dropped).
pub(crate) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    /// A pool running the jobs on `threads` threads (including the caller of
    /// `run`).
    pub fn new(threads: usize) -> Self {
        let shared = Arc::new(Shared {
            epoch: AtomicU64::new(0),
            job: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let threads = (1..threads)
            .map(|_| {
                let shared = Arc::clone(&shared);
                thread::spawn(move || {
                    let mut seen = 0;
                    let mut idle = 0u32;
                    while !shared.stop.load(Ordering::Relaxed) {
                        let epoch = shared.epoch.load(Ordering::Acquire);
                        if epoch == seen {
                            // Let the other threads run if there are more
                            // threads than processors.
                            idle = idle.saturating_add(1);
                            if idle < 1024 {
                                std::hint::spin_loop();
                            } else {
                                thread::yield_now();
                            }
                            continue;
                        }
                        seen = epoch;
                        idle = 0;
                        let job = shared.job.lock().unwrap().clone();
                        if let Some(job) = job {
                            job.work();
                        }
                    }
                })
            })
            .collect();
        Pool { shared, threads }
    }

    /// Runs `run(i)` for i < count, on the threads of the pool.
    pub fn run(&self, count: usize, run: &(dyn Fn(usize) + Sync)) {
        // SAFETY: the lifetime of the function is extended for the job, but
        // it's only called until all the tasks are finished, which this
        // function waits for.
        let run: &(dyn Fn(usize) + Sync + 'static) = unsafe { std::mem::transmute(run) };
        let job = Arc::new(Job {
            run,
            count,
            next: AtomicUsize::new(0),
            pending: AtomicUsize::new(count),
            panicked: AtomicBool::new(false),
        });
        *self.shared.job.lock().unwrap() = Some(Arc::clone(&job));
        self.shared.epoch.fetch_add(1, Ordering::Release);
        job.work();
        while job.pending.load(Ordering::Acquire) != 0 {
            std::hint::spin_loop();
        }
        *self.shared.job.lock().unwrap() = None;
        if job.panicked.load(Ordering::Relaxed) {
            panic!("A task of the parallel search panicked.");
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Pool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn runs_all_the_tasks_once() {
        let pool = Pool::new(4);
        for count in [0, 1, 5, 1000] {
            let runs: Vec<AtomicUsize> = (0..count).map(|_| AtomicUsize::new(0)).collect();
            pool.run(count, &|i| {
                runs[i].fetch_add(1, Ordering::Relaxed);
            });
            assert!(runs.iter().all(|r| r.load(Ordering::Relaxed) == 1));
        }
    }

    #[test]
    #[should_panic(expected = "A task of the parallel search panicked.")]
    fn propagates_the_panics() {
        let pool = Pool::new(2);
        pool.run(10, &|i| assert!(i != 7));
    }
}
