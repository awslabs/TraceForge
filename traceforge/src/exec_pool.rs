use crate::exec_graph::ExecutionGraph;
use crate::must::Must;
use crate::runtime::execution::Execution;
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use crate::{Config, Stats};
use log::{debug, trace};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::env;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

#[derive(PartialEq, Debug)]
enum ExecutionPoolWorkerState {
    // When the pool is first created, each worker is in Created until they
    // enter the worker_loop() function.
    Created,

    // If a worker is not executing and has not been shut down, it's Waiting.
    Waiting,

    // The worker is executing a task.
    Busy,

    // The pool has asked this worker to stop. **Diagnostic only**: a worker
    // writes its own state without re-reading it, so this can be overwritten
    // by the worker's next write, and it is `ShutdownFlag` that actually ends
    // the loop.
    Shutdown,
}

/// LockableWorkerState wraps the about ExecutionPoolWorkerState enum in a mutex
/// so that it can be mutated either by the worker_loop() or externally when the
/// Shutdown state is asserted by the pool.
type LockableWorkerState = Arc<Mutex<ExecutionPoolWorkerState>>;

/// SharedWorkerDeque is the backlog of ExecutionGraphs that are queued for
/// distribution to the workers. Queueing an Option::None tells the worker to
/// use the EG that is local to the worker's local TraceForge instance. In theory,
/// that should only happen once when the pool is created to start the first
/// worker.
type SharedWorkerDeque = Arc<Mutex<VecDeque<Option<ExecutionGraph>>>>;

/// CondBlocker is the condition variable that is used to signal sleeping
/// workers to pop the next job from the queue and process it.
type CondBlocker = Arc<Condvar>;

/// The pool-wide shutdown request.
///
/// It is separate from `ExecutionPoolWorkerState` on purpose. A worker writes
/// its own state (`Waiting`, `Busy`) without first re-reading it, so a
/// `Shutdown` written into that state by the pool could be overwritten by the
/// worker's next write. The worker would then never see it, and
/// `shutdown_now` would wait for it forever. Nothing but `shutdown_now` writes
/// this flag, so it cannot be lost that way.
type ShutdownFlag = Arc<AtomicBool>;

/// ExecutionPoolWorker is the struct that holds all of the processing context
/// information which is provided as arguments to the worker_loop() function.
///
struct ExecutionPoolWorker {
    thread_handle: Option<JoinHandle<()>>,
    worker_state: LockableWorkerState,
    thread_idx: usize,
    shared_queue: SharedWorkerDeque,
    loop_block_cond: CondBlocker,
    shutdown: ShutdownFlag,
    pool_can_drain: Arc<Mutex<bool>>,
    pool_exec_stats: Arc<Mutex<Stats>>,
    must_conf: Config,
    /// In order to make max_iterations work right with parallel exploration
    /// we have to count executions as they start, not as they are finished,
    /// otherwise we end up overshooting while draining the queue of
    /// revisits.
    exec_counter: Arc<Mutex<u64>>,
}

impl ExecutionPoolWorker {
    // There is a circular dependency here with being able to create the thread
    // and the arguments for the thread as a member of the type before the members
    // themselves are created, so thread_handle is initially set to None and then
    // explicitly instantiated via start().
    //
    pub fn new(
        thread_idx: usize,
        shared_queue: SharedWorkerDeque,
        loop_block_cond: CondBlocker,
        shutdown: ShutdownFlag,
        pool_can_drain: Arc<Mutex<bool>>,
        pool_exec_stats: Arc<Mutex<Stats>>,
        must_conf: &Config,
        exec_counter: Arc<Mutex<u64>>,
    ) -> Self {
        debug!("Created Worker [{}]", &thread_idx);

        Self {
            thread_handle: None,
            worker_state: Arc::new(Mutex::new(ExecutionPoolWorkerState::Created)),
            thread_idx,
            shared_queue,
            loop_block_cond,
            shutdown,
            pool_can_drain,
            pool_exec_stats,
            must_conf: must_conf.clone(),
            exec_counter,
        }
    }

    // Annoyance: RefCell<> doesn't implement Send so the compiler won't let it
    // be passed into a thread. Thus, we clone or create everything here and then
    // let it be moved into the worker_loop.
    //
    pub fn start<F>(&mut self, exec_func: &Arc<F>)
    where
        F: Fn() + Send + Sync + 'static,
    {
        let thread_idx = self.thread_idx;
        let worker_state = self.worker_state.clone();
        let shared_queue = self.shared_queue.clone();
        let loop_block_cond = self.loop_block_cond.clone();
        let shutdown = self.shutdown.clone();
        let exec_func = exec_func.clone();
        let pool_exec_can_drain = self.pool_can_drain.clone();
        let pool_exec_stats = self.pool_exec_stats.clone();
        let must_conf = self.must_conf.clone();
        let exec_counter = self.exec_counter.clone();

        let thread_handle = std::thread::Builder::new()
            .name(format!("exec-pool-{}", &self.thread_idx))
            .spawn(move || {
                worker_loop(
                    thread_idx,
                    worker_state,
                    shared_queue,
                    loop_block_cond,
                    shutdown,
                    pool_exec_can_drain,
                    pool_exec_stats,
                    exec_func,
                    must_conf,
                    exec_counter,
                )
            })
            .expect("Should spawn() ExecutionPool worker thread.");

        self.thread_handle = Some(thread_handle);

        trace!("Started worker thread {}", &self.thread_idx);
    }
}

// The worker_loop function is not a member function of the pool worker, though
// logically it should be; but because the lifetime of the thread may exceed the
// lifetime of the ExecutionPoolWorker itself, Rust won't allow that. I chatted
// with some clue-wielding folks on #rust and they convinced me that this was the
// cleanest approach.
//
#[allow(clippy::too_many_arguments)]
fn worker_loop<F>(
    thread_idx: usize,
    worker_state: LockableWorkerState,
    shared_queue: SharedWorkerDeque,
    loop_block_cond: CondBlocker,
    shutdown: ShutdownFlag,
    pool_exec_can_drain: Arc<Mutex<bool>>,
    pool_exec_stats: Arc<Mutex<Stats>>,
    exec_func: Arc<F>,
    must_conf: Config,
    exec_counter: Arc<Mutex<u64>>,
) where
    F: Fn() + Send + Sync + 'static,
{
    // Don't create n times what you can create once.
    let wait_timeout_ms = Duration::from_millis(250);
    let max_iterations = must_conf.max_iterations;

    // Create a new TraceForge instance for each worker.
    let mut exec_must = Must::new(must_conf, false);
    exec_must.set_parallel_queues((shared_queue.clone(), loop_block_cond.clone()));
    let must_wrap = Rc::new(RefCell::new(exec_must));

    let continuation_pool = ContinuationPool::new();

    // Until the Worker is signalled to Shutdown...
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        #[cfg(test)]
        test_hooks::pause_at(test_hooks::Point::AfterShutdownCheck);

        {
            // The shutdown flag is re-read under the queue lock, and
            // `shutdown_now` sets the flag and then notifies while holding that
            // same lock. So a worker either sees the flag here or is already
            // waiting when the notification arrives; it cannot go to sleep
            // having missed both.
            let queue = shared_queue.lock().expect("Lock shared_queue mutex");
            if queue.is_empty() && !shutdown.load(Ordering::SeqCst) {
                *worker_state.lock().expect("Lock worker_state mutex") =
                    ExecutionPoolWorkerState::Waiting;

                // Between the re-check and the wait, still holding the queue
                // lock. A test stopped here makes `shutdown_now` block on that
                // lock until this worker is actually waiting, which is what
                // makes the store-and-notify-under-the-lock rule observable.
                #[cfg(test)]
                test_hooks::pause_at(test_hooks::Point::BeforeWait);

                let _timed_out = loop_block_cond
                    .wait_timeout(queue, wait_timeout_ms)
                    .expect("wait_timeout() failed");
            }
        }

        if cfg!(debug_assertions) {
            let queue_depth = shared_queue
                .lock()
                .expect("locking shared queue mutex")
                .len();
            trace!(
                "[{}] Queue depth is {}, state is {:?}",
                thread_idx,
                queue_depth,
                *worker_state.lock().unwrap()
            );
        }

        // After the (potential) wait_timeout() above finishes, there still may
        // or may not be work queued. Attempt to pop the head of the queue.
        //
        // Taking an item and marking the worker `Busy` happen under one queue
        // lock. `drain_and_shutdown` reads the queue depth and the busy states
        // under that same lock, so it can never see an empty queue and no busy
        // worker while a worker holds an item it has not yet marked. Without
        // this, the pool could shut down with work in flight, and the work that
        // item would have queued would never be explored.
        let next_eg = {
            let mut queue = shared_queue.lock().expect("locking shared queue mutex");
            if shutdown.load(Ordering::SeqCst) {
                // **A worker released from its wait must not start new work.**
                // The flag is stored under this same lock, so a worker that
                // acquires the lock after `shutdown_now` sees it here, and no
                // graph is begun after the request. Without this check a
                // worker that was idle when the request arrived could take a
                // queued item and explore it in full before reaching the
                // check at the top of the loop. `None` sends it round the
                // loop, where the flag ends it.
                None
            } else {
                let item = queue.pop_front();
                if item.is_some() {
                    *worker_state.lock().expect("Couldn't lock state mutex") =
                        ExecutionPoolWorkerState::Busy;
                }
                item
            }
        };

        // If there's no work, loop around and try again.
        //
        if next_eg.is_none() {
            trace!("[{}] No work to do.", thread_idx);
            continue;
        }

        #[cfg(test)]
        test_hooks::pause_at(test_hooks::Point::AfterTakingWork);

        // The queued object may or not contain an actual graph. If so,
        // add it to this worker's TraceForge queue. If this queue node does NOT
        // contain an EG, this signals the start token and it should use the
        // EG that's already associated with the worker's TraceForge instance.
        //
        if let Some(eg) = next_eg.unwrap() {
            if cfg!(debug_assertions) {
                trace!("[{}] is working on a provided EG.", thread_idx);
            }
            must_wrap.borrow_mut().reset_execution_graph(eg);
        } else {
            trace!("[{}] is working on a new EG.", thread_idx);
        }

        // Loop until the graph is done. Once the first graph successfully
        // completes, mark can_drain as true.
        //
        CONTINUATION_POOL.set(&continuation_pool, || loop {
            if let Some(limit) = max_iterations {
                let exec_c = {
                    let mut exec_c = exec_counter.lock().expect("Couldn't unlock exec_counter");
                    *exec_c += 1;
                    *exec_c
                };
                if exec_c > limit {
                    break; // Reached max iterations.
                }
            }

            let this_func = exec_func.clone();
            let execution = Execution::new(must_wrap.clone());
            Must::begin_execution(&must_wrap);

            // Unless we're in debug mode, don't pay the cost for collecting
            // and outputting runtimes.
            //
            if cfg!(debug_assertions) {
                trace!("[{}] is executing.", thread_idx);
                let start_time = Instant::now();
                execution.run(move || this_func());
                let end_time = Instant::now();
                trace!(
                    "[{}] is done executing, ran from {:?} to {:?} for {:?}",
                    thread_idx,
                    start_time,
                    end_time,
                    end_time.duration_since(start_time)
                );
            } else {
                execution.run(move || this_func());
            }

            *pool_exec_can_drain
                .lock()
                .expect("expect_pool_can_drain mutex") = true;
            if Must::complete_execution(&must_wrap) {
                break;
            }
        }); // loop until graph processing complete.

        // The worker is done on this graph.
        trace!("[{}] is done working.", thread_idx);
    } // loop until shutdown

    // Loop has been exited; shutdown must have been set.
    debug!("[{}] worker is shutdown.", thread_idx);

    let must_stats = must_wrap.borrow().stats();
    pool_exec_stats
        .lock()
        .expect("Can't lock stats mutex")
        .add(&must_stats);
} // worker_loop()

/// ExecutionPool is the main object to be instantiated.
///
pub struct ExecutionPool {
    worker_vec: Vec<ExecutionPoolWorker>,
    work_deque: SharedWorkerDeque,
    loop_block_cond: CondBlocker,
    shutdown: ShutdownFlag,
    can_drain: Arc<Mutex<bool>>,
    exec_stats: Arc<Mutex<Stats>>,
    is_shutdown: bool,
}

impl ExecutionPool {
    /// No more than this many items will be enqueued on the queue which serves
    /// the workers. The main reason for doing this is to avoid having the queue
    /// grow without bounds. When the queue gets full, the existing TraceForge serial
    /// code (local queue of revisits) is used instead, so no revisits get lost
    /// and nothing blocks. This is a classic form of **backpressure** which is
    /// always needed whenever there is a possibility that work can arrive at
    /// the queue faster than it can be processed by the workers.
    ///
    /// When testing on a nontrivial customer model with 16 worker threads:
    /// - an unlimited queue yields a 2x improvement over serial
    /// - a limited queue yields a 4x improvement over serial
    ///
    /// This increases the parallel utilization factor from about 12.5% to 25%
    /// which is still not great.
    ///
    /// The value of this limit seems to be very insensitive; I got nearly identical
    /// results with a queue size of 2, 10, 100, or 1000. I believe that the real
    /// value of this limit is to prevent the parallel revisit queue from consuming
    /// all system memory.
    pub const MAX_QUEUE_SIZE: usize = 100;

    pub fn new(must_conf: &Config) -> Self {
        let work_deque = Arc::new(Mutex::new(VecDeque::new()));
        let loop_block_cond = Arc::new(Condvar::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let exec_stats = Arc::new(Mutex::new(Stats::default()));
        let can_drain = Arc::new(Mutex::new(false));
        let exec_counter = Arc::new(Mutex::new(0));

        let worker_count: usize = if let Some(rpw) = must_conf.parallel_workers {
            rpw
        } else if let Ok(rpw) = env::var("MUST_PARALLEL_WORKERS") {
            rpw.parse().unwrap()
        } else {
            num_cpus::get()
        };

        debug!("Using Execution Pool with {} workers.", worker_count);

        let worker_vec: Vec<ExecutionPoolWorker> = (0..worker_count)
            .map(|idx| {
                ExecutionPoolWorker::new(
                    idx,
                    work_deque.clone(),
                    loop_block_cond.clone(),
                    shutdown.clone(),
                    can_drain.clone(),
                    exec_stats.clone(),
                    must_conf,
                    exec_counter.clone(),
                )
            })
            .collect();

        Self {
            worker_vec,
            work_deque,
            loop_block_cond,
            shutdown,
            exec_stats,
            can_drain,
            is_shutdown: false,
        }
    }

    pub fn explore<F>(&mut self, exec_func: &Arc<F>) -> Stats
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.worker_vec.iter_mut().for_each(|w| w.start(exec_func));

        debug!("Enqueuing the start token...");
        self.enqueue(<Option<ExecutionGraph>>::None);

        debug!("Draining and Shutting Down...");
        self.drain_and_shutdown();

        debug!("Done. Returning Stats.");
        self.exec_stats
            .lock()
            .expect("can't lock pool mutex")
            .clone()
    }

    /// Adds an Option<ExecutionGraph> to the shared queue and then calls
    /// notify_one() on the shared condition variable to wake up one of the
    /// Workers to process a queued graph. If enqueue() is called with
    /// Option<None>, then the default ExecutionGraph that comes bundled with
    /// on the TraceForge object is used. (Generally this should only be invoked to
    /// start the processing.
    ///
    pub(crate) fn enqueue(&mut self, rv: Option<ExecutionGraph>) {
        let mut work_deque = self
            .work_deque
            .lock()
            .expect("Couldn't lock work deque mutex");

        if self.is_shutdown {
            panic!("Shouldn't enqueue() after shutdown() invoked.");
        }

        work_deque.push_back(rv);

        trace!("Pushed execution, queue size now {}", work_deque.len());

        self.loop_block_cond.notify_one();
    }

    /// This function blocks until all of the workers are not in the Busy state
    /// and until the shared_queue is empty; at which point shutdown_now() is
    /// called.
    ///
    pub fn drain_and_shutdown(&mut self) -> bool {
        loop {
            // Add the delay once here rather than at every branch/continue.
            sleep(Duration::from_millis(250));

            let can_drain = *self.can_drain.lock().expect("can_drain mutex lock");

            if !can_drain {
                debug!("can_drain not set yet ... ");
                continue;
            }

            // The depth and the busy states are read under one queue lock: a
            // worker takes an item and marks itself `Busy` under that lock
            // too, so the two readings are consistent with each other.
            let (depth, any_busy) = {
                let queue = self.work_deque.lock().expect("Couldn't lock deque mutex");
                let any_busy = self.worker_vec.iter().any(|w| {
                    *w.worker_state.lock().expect("worker vec mutex lock")
                        == ExecutionPoolWorkerState::Busy
                });

                // Between the two readings, still holding the queue lock. A
                // test stopped here cannot see them diverge while the lock is
                // held; that is the property, and reading them under separate
                // locks is what this point exists to expose.
                #[cfg(test)]
                test_hooks::pause_at(test_hooks::Point::DrainBetweenReads);

                (queue.len(), any_busy)
            };

            if depth > 0 {
                trace!("Draining ... deque depth still {depth}");
                continue;
            }

            if any_busy {
                debug!("Threads are still finishing ... ");
                continue;
            }

            // if depth is 0 and all the threads are waiting, we're done.
            debug!("Queue drained.");
            break;
        }

        self.shutdown_now()
    }

    /// This function sets the pool's `ShutdownFlag`, which is what breaks the
    /// workers out of worker_loop(), after which it join()s the completed
    /// threads. It returns whether all of the threads were joined (e.g. did
    /// any of them panic.)
    ///
    /// The per-worker `Shutdown` state is set as well, for diagnostics only:
    /// a worker **may** overwrite its own state on a later pass, which is the
    /// defect this flag exists to prevent. With the flag in place it does not,
    /// because a worker
    /// that reads the flag leaves the loop without writing its state again.
    ///
    pub fn shutdown_now(&mut self) -> bool {
        self.is_shutdown = true;

        let mut threads_joined = 0;

        debug!("Shutting threads down...");
        {
            // Set the flag and wake every worker while holding the queue lock,
            // so a worker between its flag check and its wait cannot miss both
            // (see `worker_loop`).
            let _queue = self.work_deque.lock().expect("Couldn't lock deque mutex");
            self.shutdown.store(true, Ordering::SeqCst);
            self.loop_block_cond.notify_all();
            // Last statement *inside* the critical section, so the lock is still
            // held here. See `test_hooks::Point::AfterShutdownStore`; the position
            // is the whole point and it was moved here on a measurement.
            #[cfg(test)]
            test_hooks::pause_at(test_hooks::Point::AfterShutdownStore);
        }
        // The per-worker state is still set, for diagnostics only; the flag
        // above is what ends the worker loops.
        self.worker_vec.iter_mut().for_each(|w| {
            *w.worker_state.lock().expect("worker vec mutex lock") =
                ExecutionPoolWorkerState::Shutdown
        });

        // Not all the threads may be complete yet so join() the ones that are
        // ready and loop until all of the threads in the Vec have been set to
        // Option::None via .take().
        //
        loop {
            self.worker_vec.iter_mut().for_each(|w| {
                if let Some(busy_th) = &w.thread_handle {
                    if busy_th.is_finished() {
                        trace!("[{}] Joining ... ", &w.thread_idx);
                        let th = w.thread_handle.take().unwrap();
                        th.join().expect("Didn't join worker thread");
                        trace!("[{}] Joined. ", &w.thread_idx);
                        threads_joined += 1;
                    } else {
                        debug!("[{}] Not finished yet.", &w.thread_idx);
                    }
                }
            });

            // If any of the threads haven't completed, loop again; otherwise,
            // break out of the loop
            if let Some(busy_worker) = self.worker_vec.iter().find(|&w| w.thread_handle.is_some()) {
                trace!("[{}] Still isn't done. Looping().", &busy_worker.thread_idx);
                // How long this waits is not ours to choose. A worker that
                // was idle when the request arrived leaves within one
                // `wait_timeout` of it, and usually at once, because it is
                // notified and then takes no new work. A worker that was
                // already exploring a graph checks the flag only once that
                // exploration finishes, which can take arbitrarily longer:
                // the flag is read at the top of the outer loop and not
                // inside the inner one, so that no graph is abandoned
                // half-explored. Either way the caller waits for something
                // else to happen, so polling faster than this buys nothing,
                // while an unslept loop burns a whole core for the duration.
                sleep(Duration::from_millis(1));
            } else {
                trace!("All workers have completed and join()ed.");
                break;
            }
        } // loop

        threads_joined == self.worker_vec.len()
    } // shutdown_now()
}

/// **Test-only pause points in `worker_loop`.**
///
/// The shutdown races fixed above open for a few instructions at a time and
/// are hit in well under 1% of runs, so a test cannot rely on hitting them.
/// A test arms one point; the next worker to reach it stops there until the
/// test releases it, which lets the test run a shutdown inside the window
/// every time. Compiled only under `cfg(test)`.
///
/// The state is process-wide, so tests that arm a point must not run
/// concurrently with each other or with any other test that uses the pool.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::{Condvar, Mutex};

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum Point {
        /// After the worker's shutdown check, before it takes the queue lock
        /// to decide whether to wait.
        AfterShutdownCheck,
        /// After the worker has taken an item from the queue.
        AfterTakingWork,
        /// Inside the queue lock, after the worker re-reads the shutdown flag
        /// and before it waits on the condition variable.
        BeforeWait,
        /// Inside the queue lock in `drain_and_shutdown`, between reading the
        /// busy states and reading the queue depth.
        DrainBetweenReads,
        /// In `shutdown_now`, the **last statement inside** the critical section
        /// that stores the shutdown flag and notifies — so reaching it means the
        /// request is published **and the queue lock is still held**.
        ///
        /// The fifth point exists because of what the four before it could not
        /// distinguish. §5.2's rule is that the store and the notify happen inside
        /// the queue lock. The minimal violation — keep the lock acquisition, move
        /// only the store and notify after it — **survived all eight patterns**;
        /// pattern 6 catches only the maximal form, and catches it through the
        /// blocking of the lock acquisition rather than through where the store
        /// sits, so the set pinned "`shutdown_now` must contend for the lock before
        /// storing", which is weaker than the rule the original bug turned on
        /// Measured: that mutation passed all eight of the tests that existed
        /// before this point was added.
        ///
        /// **The position was corrected on a measurement, and the first attempt is
        /// worth recording.** This marker was first placed immediately *after* the
        /// critical section. That killed the minimal violation when the mutation
        /// put the store after the marker line, and **missed the same program**
        /// when it put the store before it — two mutants differing only in which
        /// side of a `cfg(test)` line the store sat on, a line that does not exist
        /// in a non-test build. So the test pinned "the store precedes this source
        /// line", not "the store is inside the lock". From **inside** the block
        /// there is no such gap: any mutation that lifts the store out of the
        /// critical section necessarily lands after this marker, and both forms
        /// die. Measured both ways — with the marker outside, one spelling was
        /// killed and the other missed; with it inside, both are killed and the two
        /// spellings become the same program up to a comment.
        ///
        /// **What it costs.** A test parked here holds the queue lock, so a worker
        /// cannot acquire it and the *behavioural* witness — a woken worker takes
        /// nothing — becomes a post-release race. The discriminating witness is
        /// therefore the flag read. That is the right instrument for a rule about
        /// ordering, and the behavioural consequence is not lost from the set:
        /// pattern 8 establishes that queued work is not taken after a request.
        ///
        /// **The position is load-bearing in both directions, and the second one is
        /// a false red rather than a false green.** Below the marker, nothing can
        /// lift the store out of the critical section undetected — that is what it
        /// is for. But a *rule-preserving* reorder that puts the store below the
        /// marker while keeping it inside the lock (notify, marker, store) also
        /// fails the test, although the program is equivalent. Measured as E3
        /// while keeping it inside the lock, and that was measured.
        ///
        /// **So state what the test is and is not.** It is a *structural guard for
        /// this implementation*: it establishes that the store precedes this marker,
        /// which given the marker's position means the store is inside the lock
        /// **here**. It is not a behavioural proof that every implementation
        /// satisfying the rule would pass — an equivalent one that stores below the
        /// marker does not. Observing lock ownership without imposing
        /// store-before-marker ordering would need richer instrumentation or a
        /// state-machine test; that is the cost of the small hook, and it buys the
        /// guarantee that no mutation can move the store past this critical section
        /// undetected. It is the
        /// conservative direction for a test whose
        /// job is that no mutation lifts the store out of the lock, and the failure
        /// message names the benign possibility explicitly rather than asserting
        /// the defect. Telling "inside the lock but below the marker" from "outside
        /// the lock" would need a second observation point, and `test_hooks` can
        /// arm only one at a time.
        AfterShutdownStore,
    }

    struct Armed {
        point: Point,
        reached: bool,
        released: bool,
    }

    static STATE: Mutex<Option<Armed>> = Mutex::new(None);
    static CHANGED: Condvar = Condvar::new();

    /// Arm `point`. Only the first worker to reach it will stop.
    pub(crate) fn arm(point: Point) {
        *STATE.lock().unwrap() = Some(Armed {
            point,
            reached: false,
            released: false,
        });
    }

    /// Block until a worker has stopped at the armed point.
    pub(crate) fn wait_until_reached() {
        let mut state = STATE.lock().unwrap();
        while !state.as_ref().is_some_and(|a| a.reached) {
            state = CHANGED.wait(state).unwrap();
        }
    }

    /// Let the stopped worker continue, and disarm.
    pub(crate) fn release() {
        if let Some(a) = STATE.lock().unwrap().as_mut() {
            a.released = true;
        }
        CHANGED.notify_all();
    }

    /// Disarm without a worker having reached the point.
    pub(crate) fn disarm() {
        *STATE.lock().unwrap() = None;
        CHANGED.notify_all();
    }

    pub(super) fn pause_at(point: Point) {
        let mut state = STATE.lock().unwrap();
        let fire = matches!(state.as_ref(), Some(a) if a.point == point && !a.reached);
        if !fire {
            return;
        }
        if let Some(a) = state.as_mut() {
            a.reached = true;
        }
        CHANGED.notify_all();
        while !state.as_ref().map_or(true, |a| a.released) {
            state = CHANGED.wait(state).unwrap();
        }
    }
}

/// **Tests for the pool's shutdown protocol.**
///
/// The pool shuts down once its queue is empty and no worker is busy. Two
/// windows used to break that:
///
/// 1. a worker checked for shutdown, and then wrote its own state back to
///    `Waiting` without looking again, erasing a shutdown requested in
///    between. The worker then never exited, and `shutdown_now` spun forever
///    waiting for it;
/// 2. a worker took an item from the queue while still marked `Waiting`, and
///    only then marked itself `Busy`. In between, the drain check could see an
///    empty queue and no busy worker, and shut the pool down with work in
///    flight.
///
/// Both windows are a few instructions wide, so each test uses
/// [`test_hooks`](super::test_hooks) to stop a worker inside one and runs the
/// shutdown while it is stopped.
///
/// **Every scenario runs in a child process** (`#[ignore]`d `*_child` tests,
/// started by the test of the same pattern). A pool that never shuts down
/// leaves a thread spinning or blocked forever, which must not take the test
/// run with it: the parent kills the child after [`CHILD_LIMIT`] and fails.
/// The child process also gives each scenario the process-wide hook state to
/// itself.
#[cfg(test)]
mod tests {
    use super::test_hooks::{self, Point};
    use super::{ExecutionPool, ExecutionPoolWorkerState, LockableWorkerState};
    use crate::thread::{self, ThreadId};
    use crate::Config;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    /// How long a parent waits for its child before killing it. A child
    /// normally finishes in a few seconds.
    const CHILD_LIMIT: Duration = Duration::from_secs(60);

    /// How long a child waits for any one step. A step that takes longer is a
    /// pool that will never get there.
    const STEP_LIMIT: Duration = Duration::from_secs(20);

    /// Runs `name` alone in a fresh copy of this test binary and fails unless
    /// it passes within [`CHILD_LIMIT`]. A child still running then is killed.
    fn run_child(name: &str) {
        let exe = std::env::current_exe().expect("the test binary knows its own path");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                name,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("re-running this test binary for one ignored test");
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("polling the child") {
                break Some(status);
            }
            if start.elapsed() > CHILD_LIMIT {
                child.kill().expect("killing the child");
                child.wait().expect("reaping the child");
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        let _ = child.stdout.take().unwrap().read_to_string(&mut stdout);
        let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
        let output = format!("--- child stdout ---\n{stdout}\n--- child stderr ---\n{stderr}");
        let Some(status) = status else {
            panic!(
                "{name} did not finish within {CHILD_LIMIT:?} and was killed: the pool never \
                 shut down.\n{output}"
            );
        };
        // A filter that matches nothing also exits 0, which would be a silent
        // pass. The child must say it ran exactly one test.
        assert!(
            stdout.contains("1 passed") || stdout.contains("1 failed"),
            "the child did not run {name}; filter or name drift.\n{output}"
        );
        assert!(status.success(), "{name} failed.\n{output}");
    }

    /// Serializes the children if they are ever run together in one process
    /// (`--ignored` without `--exact`): the hook state is process-wide.
    static HOOKS: Mutex<()> = Mutex::new(());

    fn hooks_lock() -> MutexGuard<'static, ()> {
        HOOKS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Disarms the pause point on drop, so a failing child never leaves a
    /// worker stopped.
    struct Disarm;

    impl Drop for Disarm {
        fn drop(&mut self) {
            test_hooks::disarm();
        }
    }

    #[derive(Clone, PartialEq, Debug)]
    enum ToCoordinator {
        Yes,
        No,
    }

    #[derive(Clone, PartialEq, Debug)]
    enum ToParticipant {
        Prepare(ThreadId),
        Commit,
        Abort,
    }

    fn participant() {
        let coordinator = match crate::recv_msg_block::<ToParticipant>() {
            ToParticipant::Prepare(id) => id,
            other => panic!("expected Prepare, got {other:?}"),
        };
        let yes = crate::nondet();
        let vote = if yes {
            ToCoordinator::Yes
        } else {
            ToCoordinator::No
        };
        crate::send_msg(coordinator, vote);
        match crate::recv_msg_block::<ToParticipant>() {
            ToParticipant::Commit => assert!(yes),
            ToParticipant::Abort => {}
            other => panic!("expected a decision, got {other:?}"),
        }
    }

    /// Two-phase commit with `n` participants, each voting `nondet()`. The
    /// coordinator's receives race, so the exploration revisits.
    ///
    /// **It does not hand those revisits to other workers through the pool's
    /// queue**, which an earlier version of this comment claimed. Measured:
    /// `Must::backward_revisit` is entered **0 times in 4 352 executions** across
    /// five program shapes, serial and parallel, including this one — its sole
    /// call site never runs. So this program exercises the pool's *lifecycle* —
    /// start, explore, drain, shut down — and not its queue as a work-distribution
    /// channel. The patterns that need work in the queue stage it directly, which
    /// is why they do.
    fn two_phase_commit(n: u32) {
        let participants: Vec<ThreadId> = (0..n)
            .map(|_| thread::spawn(participant).thread().id())
            .collect();
        let _ = thread::spawn(move || {
            let me = thread::current().id();
            for p in &participants {
                crate::send_msg(*p, ToParticipant::Prepare(me));
            }
            let yes = (0..participants.len())
                .filter(|_| crate::recv_msg_block::<ToCoordinator>() == ToCoordinator::Yes)
                .count();
            let decision = if yes == participants.len() {
                ToParticipant::Commit
            } else {
                ToParticipant::Abort
            };
            for p in &participants {
                crate::send_msg(*p, decision.clone());
            }
        });
    }

    /// The number of executions of [`two_phase_commit`]: `2^n` vote vectors
    /// times `n!` orders in which the coordinator receives the votes.
    fn two_phase_commit_execs(n: u32) -> usize {
        2usize.pow(n) * (1..=n as usize).product::<usize>()
    }

    /// A pool of `workers`, and handles on each worker's state that stay
    /// readable after the pool is moved into another thread.
    fn pool(workers: usize) -> (ExecutionPool, Vec<LockableWorkerState>) {
        let conf = Config::builder()
            .with_parallel(true)
            .with_parallel_workers(workers)
            .build();
        let pool = ExecutionPool::new(&conf);
        let states = pool
            .worker_vec
            .iter()
            .map(|w| w.worker_state.clone())
            .collect();
        (pool, states)
    }

    fn state_is(state: &LockableWorkerState, expected: ExecutionPoolWorkerState) -> bool {
        *state.lock().unwrap() == expected
    }

    fn count_in(states: &[LockableWorkerState], expected: ExecutionPoolWorkerState) -> usize {
        states
            .iter()
            .filter(|s| *s.lock().unwrap() == expected)
            .count()
    }

    fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < STEP_LIMIT,
                "timed out after {STEP_LIMIT:?} waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// [`test_hooks::wait_until_reached`], bounded by [`STEP_LIMIT`].
    fn wait_until_paused() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            test_hooks::wait_until_reached();
            let _ = tx.send(());
        });
        rx.recv_timeout(STEP_LIMIT)
            .expect("no worker reached the armed pause point");
    }

    /// On-CPU time of the calling thread, from `/proc/thread-self/schedstat`.
    #[cfg(target_os = "linux")]
    fn thread_cpu_time() -> Duration {
        let schedstat = std::fs::read_to_string("/proc/thread-self/schedstat")
            .expect("reading /proc/thread-self/schedstat");
        let nanos = schedstat
            .split_whitespace()
            .next()
            .and_then(|field| field.parse().ok())
            .expect("the first schedstat field is the on-CPU time in nanoseconds");
        Duration::from_nanos(nanos)
    }

    struct Explored {
        execs: usize,
        /// Wall time of `explore`.
        wall: Duration,
        /// CPU time the calling thread spent in `explore`.
        #[cfg(target_os = "linux")]
        cpu: Duration,
    }

    /// Runs `pool.explore` on [`two_phase_commit`]`(n)` in a thread of its
    /// own, so the test can act while it runs.
    fn explore_in_background(mut pool: ExecutionPool, n: u32) -> mpsc::Receiver<Explored> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("pool-caller".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                let cpu = thread_cpu_time();
                let start = Instant::now();
                let stats = pool.explore(&Arc::new(move || two_phase_commit(n)));
                let _ = tx.send(Explored {
                    execs: stats.execs,
                    wall: start.elapsed(),
                    #[cfg(target_os = "linux")]
                    cpu: thread_cpu_time() - cpu,
                });
            })
            .expect("spawning the pool's caller");
        rx
    }

    fn finished(done: &mpsc::Receiver<Explored>, window: &str) -> Explored {
        done.recv_timeout(STEP_LIMIT).unwrap_or_else(|_| {
            panic!(
                "explore did not return within {STEP_LIMIT:?} of the stopped worker resuming: \
                 the pool never shut down ({window})"
            )
        })
    }

    /// Window 1, forced on a pool of `workers`: one worker stops just after
    /// its shutdown check; the others explore the whole program and the pool
    /// requests shutdown; after `hold`, the stopped worker resumes. It must
    /// then exit, without first writing `Waiting` over the request.
    fn shutdown_while_a_worker_is_past_its_check(
        workers: usize,
        n: u32,
        hold: Duration,
    ) -> Explored {
        let (pool, states) = pool(workers);
        // The first worker to start stops at its first check, before it has
        // taken anything, so the others do all the work.
        test_hooks::arm(Point::AfterShutdownCheck);
        let done = explore_in_background(pool, n);
        wait_until_paused();
        // `shutdown_now` writes `Shutdown` into every worker's state once it
        // has requested shutdown, and the stopped worker writes nothing.
        wait_for("the pool to request shutdown", || {
            count_in(&states, ExecutionPoolWorkerState::Shutdown) == workers
        });
        std::thread::sleep(hold);
        test_hooks::release();
        let explored = finished(
            &done,
            "idle-check window: the resumed worker missed the request",
        );

        assert_eq!(
            explored.execs,
            two_phase_commit_execs(n),
            "the exploration is incomplete"
        );
        for (i, state) in states.iter().enumerate() {
            assert!(
                state_is(state, ExecutionPoolWorkerState::Shutdown),
                "worker {i} wrote {:?} after shutdown was requested: it went back to waiting \
                 instead of exiting",
                *state.lock().unwrap()
            );
        }
        explored
    }

    // --- Pattern 1: window 1 ------------------------------------------------

    /// A shutdown requested between a worker's shutdown check and its decision
    /// to wait is not lost: the worker exits, and `explore` returns the
    /// complete count.
    ///
    /// **Before the fix** the worker overwrote the request with `Waiting` and
    /// looped forever; the child fails after [`STEP_LIMIT`] with "explore did
    /// not return", and this test reports it.
    #[test]
    fn a_shutdown_during_the_idle_check_is_not_lost() {
        run_child("exec_pool::tests::window_1_child");
    }

    #[test]
    #[ignore = "run in a child process by a_shutdown_during_the_idle_check_is_not_lost"]
    fn window_1_child() {
        let _serial = hooks_lock();
        let _disarm = Disarm;
        let explored = shutdown_while_a_worker_is_past_its_check(2, 2, Duration::ZERO);
        eprintln!("window 1: explore returned after {:?}", explored.wall);
    }

    // --- Pattern 2: window 2 ------------------------------------------------

    /// Work a worker has taken keeps the pool from shutting down until it is
    /// finished, and none of the work it leads to is lost.
    ///
    /// The pool is given a second start token. Each token makes the worker
    /// that takes it explore the whole program with its own engine, so the
    /// complete count is exactly twice the sequential count. One worker stops
    /// right after taking a token, for two seconds (eight drain polls), while
    /// the other explores the whole program and goes idle. The queue is then
    /// empty and one worker is idle, but the stopped worker still holds work.
    ///
    /// **Before the fix** the stopped worker was not yet `Busy`, so the pool
    /// requested shutdown while it held the token. This test fails with "the
    /// pool requested shutdown while a worker held work"; if it had let the
    /// worker go on, the worker would have overwritten the request with
    /// `Busy` and never exited.
    #[test]
    fn work_taken_before_the_drain_check_is_finished_before_shutdown() {
        run_child("exec_pool::tests::window_2_child");
    }

    #[test]
    #[ignore = "run in a child process by \
                work_taken_before_the_drain_check_is_finished_before_shutdown"]
    fn window_2_child() {
        const N: u32 = 3;
        const HOLD: Duration = Duration::from_secs(2);
        let _serial = hooks_lock();
        let _disarm = Disarm;

        let (mut pool, states) = pool(2);
        pool.enqueue(None);
        test_hooks::arm(Point::AfterTakingWork);
        let done = explore_in_background(pool, N);
        wait_until_paused();
        wait_for("the other worker to go idle", || {
            count_in(&states, ExecutionPoolWorkerState::Waiting) >= 1
        });
        let held = Instant::now();
        while held.elapsed() < HOLD {
            assert_eq!(
                count_in(&states, ExecutionPoolWorkerState::Shutdown),
                0,
                "the pool requested shutdown while a worker held work it had \
                 taken (taken-work window)"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        // The situation the drain check had to get right: one worker idle,
        // one holding work (and marked so), the queue empty.
        assert_eq!(count_in(&states, ExecutionPoolWorkerState::Busy), 1);
        assert_eq!(count_in(&states, ExecutionPoolWorkerState::Waiting), 1);
        test_hooks::release();
        let explored = finished(&done, "taken-work window");

        assert_eq!(
            explored.execs,
            2 * two_phase_commit_execs(N),
            "work was lost: two start tokens explore the program twice"
        );
    }

    // --- Pattern 3: no spin while a worker is slow to exit -------------------

    /// While a worker has not yet exited, `shutdown_now` waits for it without
    /// burning a core.
    ///
    /// A worker is held past its shutdown check for one second after shutdown
    /// is requested, so the caller spends that second in `shutdown_now`. Its
    /// CPU time over the whole `explore` must stay under a quarter of that
    /// second.
    ///
    /// **Before the fix** the held worker never exits (window 1), so the child
    /// fails as pattern 1 does; the caller meanwhile spins at 100% of a core.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_worker_slow_to_exit_does_not_make_shutdown_spin() {
        run_child("exec_pool::tests::slow_exit_child");
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "run in a child process by a_worker_slow_to_exit_does_not_make_shutdown_spin"]
    fn slow_exit_child() {
        const HOLD: Duration = Duration::from_secs(1);
        let _serial = hooks_lock();
        let _disarm = Disarm;
        let explored = shutdown_while_a_worker_is_past_its_check(2, 2, HOLD);
        eprintln!(
            "slow exit: explore took {:?} of wall time and {:?} of the caller's CPU time",
            explored.wall, explored.cpu
        );
        assert!(
            explored.cpu < HOLD / 4,
            "the caller used {:?} of CPU time in {:?}, {HOLD:?} of it waiting for one worker: \
             shutdown_now is spinning",
            explored.cpu,
            explored.wall
        );
    }

    // --- Pattern 4: an idle pool shuts down promptly ------------------------

    /// Shutting down a pool whose workers are all idle does not wait for the
    /// workers' 250 ms poll timeout: the request wakes them.
    ///
    /// Five rounds, 16 idle workers each, `shutdown_now` timed. The total must
    /// be under 500 ms.
    ///
    /// **Before the fix** nothing woke the workers, so each round lasted until
    /// the last of 16 workers timed out: about 235 ms on average, and more
    /// than 1 s over five rounds. The test fails with the measured total.
    #[test]
    fn an_idle_pool_shuts_down_without_waiting_out_its_poll() {
        run_child("exec_pool::tests::idle_pool_child");
    }

    #[test]
    #[ignore = "run in a child process by \
                an_idle_pool_shuts_down_without_waiting_out_its_poll"]
    fn idle_pool_child() {
        const WORKERS: usize = 16;
        const ROUNDS: u32 = 5;
        const BUDGET: Duration = Duration::from_millis(500);
        let _serial = hooks_lock();

        let mut total = Duration::ZERO;
        for round in 0..ROUNDS {
            let (mut pool, states) = pool(WORKERS);
            let program = Arc::new(|| {});
            pool.worker_vec.iter_mut().for_each(|w| w.start(&program));
            wait_for("every worker to go idle", || {
                count_in(&states, ExecutionPoolWorkerState::Waiting) == WORKERS
            });
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let start = Instant::now();
                let joined_all = pool.shutdown_now();
                let _ = tx.send((joined_all, start.elapsed()));
            });
            let (joined_all, took) = rx.recv_timeout(STEP_LIMIT).unwrap_or_else(|_| {
                panic!("round {round}: shutdown_now did not return within {STEP_LIMIT:?}")
            });
            assert!(joined_all, "round {round}: not every worker was joined");
            eprintln!("idle pool: round {round}: shutdown_now took {took:?}");
            total += took;
        }
        assert!(
            total < BUDGET,
            "{ROUNDS} shutdowns of an idle {WORKERS}-worker pool took {total:?}: the workers \
             were not woken and waited out their poll timeout"
        );
    }

    // --- Pattern 5: a large pool, repeated ----------------------------------

    /// The configuration that matters here: a pool much larger than the program needs,
    /// explored repeatedly, with one worker caught in window 1 each time. Every
    /// round must return, with the complete count.
    ///
    /// **Before the fix** the first round never returns, and the child fails
    /// as pattern 1 does.
    #[test]
    fn a_large_pool_with_a_worker_in_the_window_explores_everything() {
        run_child("exec_pool::tests::large_pool_child");
    }

    #[test]
    #[ignore = "run in a child process by \
                a_large_pool_with_a_worker_in_the_window_explores_everything"]
    fn large_pool_child() {
        const WORKERS: usize = 8;
        const N: u32 = 3;
        const ROUNDS: u32 = 3;
        let _serial = hooks_lock();
        let _disarm = Disarm;
        for round in 0..ROUNDS {
            let explored = shutdown_while_a_worker_is_past_its_check(WORKERS, N, Duration::ZERO);
            eprintln!(
                "large pool: round {round}: explore returned after {:?}",
                explored.wall
            );
        }
    }

    // --- Pattern 6: the request cannot be missed at the wait ----------------

    /// A worker stopped at the moment it is about to wait — flag re-read, queue
    /// lock held — still gets the shutdown request at once, rather than
    /// sleeping out its 250 ms poll.
    ///
    /// The stopped worker holds the queue lock, so `shutdown_now` blocks there
    /// until the worker is genuinely waiting on the condition variable, and its
    /// notification, sent under that same lock, cannot arrive too early to be
    /// seen. Three rounds, timed from the release, must total under 180 ms:
    /// 60 ms a round, against a measured 1.1-3 ms when the wake-up works and a
    /// full 250 ms poll when it is missed.
    ///
    /// **With the flag stored and the notification sent outside the lock**,
    /// `shutdown_now` does not block, so both happen while the worker is still
    /// stopped short of the wait. The worker then waits having missed the
    /// notification and sleeps out its poll; the test fails with the measured
    /// total (about 750 ms).
    #[test]
    fn a_shutdown_reaches_a_worker_that_is_about_to_wait() {
        run_child("exec_pool::tests::before_wait_child");
    }

    #[test]
    #[ignore = "run in a child process by a_shutdown_reaches_a_worker_that_is_about_to_wait"]
    fn before_wait_child() {
        const ROUNDS: u32 = 3;
        const BUDGET: Duration = Duration::from_millis(180);
        let _serial = hooks_lock();
        let _disarm = Disarm;

        let mut total = Duration::ZERO;
        for round in 0..ROUNDS {
            let (mut pool, states) = pool(2);
            let program = Arc::new(|| {});
            pool.worker_vec.iter_mut().for_each(|w| w.start(&program));
            test_hooks::arm(Point::BeforeWait);
            wait_until_paused();
            assert!(
                count_in(&states, ExecutionPoolWorkerState::Waiting) >= 1,
                "round {round}: the stopped worker marks itself Waiting before it waits"
            );
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let joined_all = pool.shutdown_now();
                let _ = tx.send(joined_all);
            });
            // Long enough for `shutdown_now` to reach the queue lock, which the
            // stopped worker is holding.
            std::thread::sleep(Duration::from_millis(100));
            let released = Instant::now();
            test_hooks::release();
            let joined_all = rx.recv_timeout(STEP_LIMIT).unwrap_or_else(|_| {
                panic!("round {round}: shutdown_now did not return within {STEP_LIMIT:?}")
            });
            let took = released.elapsed();
            assert!(joined_all, "round {round}: not every worker was joined");
            eprintln!("before wait: round {round}: shutdown_now returned {took:?} after release");
            total += took;
        }
        assert!(
            total < BUDGET,
            "{ROUNDS} shutdowns of a worker stopped just before its wait took {total:?}: the \
             request was stored and notified before the worker waited, so the worker missed it \
             and slept out its poll"
        );
    }

    // --- Pattern 7: the drain's two readings cannot diverge -----------------

    enum Staged {
        Valid {
            execs: usize,
            shutdown_after: Duration,
        },
        Void(String),
    }

    /// Sets up the one state the drain check has to get right — work queued
    /// and no worker busy — and stops the drain inside it.
    ///
    /// Two workers explore the program; because a revisit never reaches the
    /// shared queue (measured: over a million samples of six program shapes at
    /// one, two and four workers, the queue only ever held a start token), the
    /// item staged here is a start token, pushed **without notifying** so the
    /// idle workers stay asleep with work queued. The drain reaches its check
    /// in that state and stops between its two readings, held there past the
    /// workers' 250 ms poll.
    ///
    /// While it is stopped it holds the queue lock, so no worker can take the
    /// item and the depth it reads afterwards is still 1: it keeps draining,
    /// and the earliest it can request shutdown is its next poll, a further
    /// 250 ms away. Reading the two under separate locks lets a worker take
    /// the item during the hold, so the depth reads 0 against a busy reading
    /// taken before that, and shutdown follows within microseconds of the
    /// release, with the work in flight.
    ///
    /// Staging races the drain's next poll, so an attempt can come to nothing;
    /// that is reported as [`Staged::Void`] and retried, and only a staged
    /// attempt asserts.
    fn stage_one_drain_check(n: u32, hold: Duration, settle: Duration) -> Staged {
        let (pool, states) = pool(2);
        let queue = pool.work_deque.clone();
        let can_drain = pool.can_drain.clone();
        let done = explore_in_background(pool, n);

        // Stage the item once the program has been explored and both workers
        // are asleep. `can_drain` means an execution has completed, so the
        // pool's own start token is long gone.
        loop {
            if *can_drain.lock().unwrap() {
                let mut held = queue.lock().unwrap();
                if held.is_empty() && count_in(&states, ExecutionPoolWorkerState::Waiting) == 2 {
                    held.push_back(None);
                    drop(held);
                    test_hooks::arm(Point::DrainBetweenReads);
                    break;
                }
            }
            if let Ok(explored) = done.try_recv() {
                return Staged::Void(format!(
                    "the pool shut down ({} execs) before the item could be staged",
                    explored.execs
                ));
            }
            std::thread::yield_now();
        }

        wait_until_paused();
        // A worker's poll may have fired before the drain's check, in which
        // case it took the item and the check sees a busy worker: no window.
        if count_in(&states, ExecutionPoolWorkerState::Busy) > 0 {
            test_hooks::release();
            let explored = finished(&done, "drain check");
            return Staged::Void(format!(
                "a worker took the item before the drain's check ({} execs)",
                explored.execs
            ));
        }
        // Past the workers' poll: one of them wakes inside this hold and takes
        // the item, unless the drain is holding the queue lock.
        std::thread::sleep(hold);

        let released = Instant::now();
        test_hooks::release();
        // Correct, the drain reads depth 1 and keeps draining, so the earliest
        // shutdown is its next poll. Reading across the gap, it concludes
        // "drained" at once.
        let mut shutdown_after = None;
        while released.elapsed() < settle && shutdown_after.is_none() {
            if count_in(&states, ExecutionPoolWorkerState::Shutdown) > 0 {
                shutdown_after = Some(released.elapsed());
            }
            std::thread::yield_now();
        }
        let explored = finished(&done, "drain check");
        Staged::Valid {
            execs: explored.execs,
            shutdown_after: shutdown_after.unwrap_or(settle),
        }
    }

    /// Work queued while the drain is deciding is either counted or left for a
    /// worker — never dropped, and never shut down from under a worker that is
    /// taking it.
    ///
    /// **With the depth and the busy states read under separate locks** a
    /// worker takes the item between the two readings, so the drain sees an
    /// empty queue and a stale "nobody busy" and requests shutdown with that
    /// work in flight, microseconds after the release rather than 250 ms
    /// later. The test fails with the measured delay.
    #[test]
    fn work_queued_while_the_drain_decides_is_not_dropped() {
        run_child("exec_pool::tests::drain_between_reads_child");
    }

    #[test]
    #[ignore = "run in a child process by work_queued_while_the_drain_decides_is_not_dropped"]
    fn drain_between_reads_child() {
        const N: u32 = 3;
        /// Longer than the workers' 250 ms poll, so a worker does wake and try
        /// to take the staged item while the drain is stopped.
        const HOLD: Duration = Duration::from_millis(400);
        /// Correct, the drain cannot request shutdown for at least its next
        /// poll, 250 ms away; reading across the gap it does so at once. This
        /// splits 250 ms and the 10 microseconds measured for the mutation.
        const SETTLE: Duration = Duration::from_millis(100);
        const ATTEMPTS: u32 = 5;
        let _serial = hooks_lock();
        let _disarm = Disarm;

        for attempt in 0..ATTEMPTS {
            match stage_one_drain_check(N, HOLD, SETTLE) {
                Staged::Valid {
                    execs,
                    shutdown_after,
                } => {
                    eprintln!(
                        "drain between reads: staged on attempt {attempt}: {execs} execs, \
                         shutdown seen {shutdown_after:?} after the release"
                    );
                    assert!(
                        shutdown_after >= SETTLE,
                        "the pool requested shutdown {shutdown_after:?} after the release, while \
                         a worker was taking the item the drain had just counted: the depth and \
                         the busy states were read on either side of that"
                    );
                    assert!(
                        execs > two_phase_commit_execs(N),
                        "the staged work was dropped: {execs} executions is no more than the \
                         {} of the exploration that had already finished",
                        two_phase_commit_execs(N)
                    );
                    return;
                }
                Staged::Void(why) => {
                    eprintln!("drain between reads: attempt {attempt} came to nothing: {why}")
                }
            }
        }
        panic!("could not stage the drain's check in {ATTEMPTS} attempts");
    }

    // --- Pattern 8: no new work is taken after the request ------------------

    /// Counts the executions [`pre_take_check_child`]'s program begins.
    static EXECUTIONS_BEGUN: AtomicUsize = AtomicUsize::new(0);

    /// Work already queued is not taken after a shutdown request: the worker
    /// leaves it where it is and exits.
    ///
    /// One worker, idle in its wait, is stopped on its next pass at the point
    /// between the flag check and the queue lock. The test then queues a start
    /// token and calls `shutdown_now` from another thread; because the queue
    /// is no longer empty, the stopped worker cannot wait, so the next thing
    /// it does on release is the take itself. The flag is stored under the
    /// very lock that take uses, so the worker sees it and takes nothing.
    ///
    /// Three independent witnesses that the token was never begun: the
    /// program's own execution counter is 0, the token is still in the queue,
    /// and the pool's stats record no executions.
    ///
    /// **Without the pre-take flag check** the worker pops the token, marks
    /// itself `Busy` and explores it in full before it looks at the flag
    /// again. All three witnesses change: the counter reads 1, the queue is
    /// empty, and the stats record the execution.
    ///
    /// This is the one property that needs a direct `shutdown_now`: the
    /// production path, `drain_and_shutdown`, requests shutdown only after it
    /// has seen an empty queue and no busy worker under the queue lock, so it
    /// cannot leave work queued at the request.
    #[test]
    fn queued_work_is_not_taken_after_a_shutdown_request() {
        run_child("exec_pool::tests::pre_take_check_child");
    }

    #[test]
    #[ignore = "run in a child process by queued_work_is_not_taken_after_a_shutdown_request"]
    fn pre_take_check_child() {
        let _serial = hooks_lock();
        let _disarm = Disarm;
        EXECUTIONS_BEGUN.store(0, Ordering::SeqCst);

        let (mut pool, states) = pool(1);
        let queue = pool.work_deque.clone();
        let stats = pool.exec_stats.clone();
        let program = Arc::new(|| {
            EXECUTIONS_BEGUN.fetch_add(1, Ordering::SeqCst);
        });
        pool.worker_vec.iter_mut().for_each(|w| w.start(&program));

        // Let the worker settle into its wait, so that it is idle in the sense
        // the guarantee is about, and stop it on its next pass.
        wait_for("the worker to go idle", || {
            count_in(&states, ExecutionPoolWorkerState::Waiting) == 1
        });
        test_hooks::arm(Point::AfterShutdownCheck);
        wait_until_paused();
        assert_eq!(
            count_in(&states, ExecutionPoolWorkerState::Waiting),
            1,
            "the stopped worker should be idle, holding no work"
        );

        // Queue the work it would take, then request shutdown directly.
        pool.enqueue(None);
        assert_eq!(
            queue.lock().unwrap().len(),
            1,
            "the token is queued before shutdown is requested"
        );
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let joined_all = pool.shutdown_now();
            let _ = tx.send(joined_all);
        });
        // `shutdown_now` stores the flag and only then writes the per-worker
        // states, so seeing the state is seeing the flag already set.
        wait_for("the shutdown request", || {
            count_in(&states, ExecutionPoolWorkerState::Shutdown) == 1
        });
        test_hooks::release();
        let joined_all = rx
            .recv_timeout(STEP_LIMIT)
            .unwrap_or_else(|_| panic!("shutdown_now did not return within {STEP_LIMIT:?}"));
        assert!(joined_all, "the worker was not joined");

        let begun = EXECUTIONS_BEGUN.load(Ordering::SeqCst);
        assert_eq!(
            begun, 0,
            "the worker began {begun} execution(s) after shutdown had been requested: it took \
             the queued token instead of leaving it"
        );
        assert_eq!(
            queue.lock().unwrap().len(),
            1,
            "the queued token is gone: the worker took it after shutdown had been requested"
        );
        assert_eq!(
            stats.lock().unwrap().execs,
            0,
            "the pool recorded executions from work taken after shutdown had been requested"
        );
        eprintln!("pre-take check: the token stayed queued and no execution began");
    }
    // --- Pattern 9: a request mid-graph does not abandon the graph -----------

    /// Counts the executions [`mid_graph_child`]'s program begins.
    static MID_GRAPH_BEGUN: AtomicUsize = AtomicUsize::new(0);
    /// Holds that program's first execution until the test opens the gate.
    static MID_GRAPH_GATE: Mutex<bool> = Mutex::new(false);
    static MID_GRAPH_OPEN: Condvar = Condvar::new();

    /// A shutdown requested while a worker is **inside** a graph does not
    /// abandon that graph: the worker explores it to the end, and only then
    /// exits.
    ///
    /// This is the one guarantee the other eight patterns leave untested,
    /// because through the production route it cannot arise: a worker inside a
    /// graph is `Busy`, and `drain_and_shutdown` requests shutdown only when no
    /// worker is. It is reachable, and asserted here, through a direct
    /// `shutdown_now` — the route a future timeout, cancel or abort caller
    /// would use, and the one the flag being read at the top of the *outer*
    /// loop rather than the inner one exists to serve.
    ///
    /// No pause point is needed: the program a worker explores is the test's
    /// own closure, so the test stops the graph from inside it.
    ///
    /// **With the flag also read inside the inner loop** the worker abandons
    /// the graph at the next execution boundary, and the test fails with the
    /// count it reached — 1 of 8 — against the closed form `2^n · n!`.
    #[test]
    fn a_shutdown_inside_a_graph_does_not_abandon_it() {
        run_child("exec_pool::tests::mid_graph_child");
    }

    #[test]
    #[ignore = "run in a child process by a_shutdown_inside_a_graph_does_not_abandon_it"]
    fn mid_graph_child() {
        const N: u32 = 2;
        let _serial = hooks_lock();
        MID_GRAPH_BEGUN.store(0, Ordering::SeqCst);
        *MID_GRAPH_GATE.lock().unwrap() = false;

        let (mut pool, states) = pool(1);
        let queue = pool.work_deque.clone();
        let stats = pool.exec_stats.clone();
        let program = Arc::new(move || {
            if MID_GRAPH_BEGUN.fetch_add(1, Ordering::SeqCst) == 0 {
                // The graph's first execution stops here, so the request lands
                // while the worker is inside the inner loop.
                let mut open = MID_GRAPH_GATE.lock().unwrap();
                while !*open {
                    open = MID_GRAPH_OPEN.wait(open).unwrap();
                }
            }
            two_phase_commit(N);
        });
        pool.worker_vec.iter_mut().for_each(|w| w.start(&program));
        pool.enqueue(None);

        wait_for("the worker to begin the graph's first execution", || {
            MID_GRAPH_BEGUN.load(Ordering::SeqCst) >= 1
        });
        wait_for("the worker to be marked busy on that graph", || {
            count_in(&states, ExecutionPoolWorkerState::Busy) == 1
        });

        // Request shutdown with the worker stopped inside the graph.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let joined_all = pool.shutdown_now();
            let _ = tx.send(joined_all);
        });
        wait_for("the shutdown request", || {
            count_in(&states, ExecutionPoolWorkerState::Shutdown) == 1
        });

        // Let the graph run on.
        *MID_GRAPH_GATE.lock().unwrap() = true;
        MID_GRAPH_OPEN.notify_all();

        let joined_all = rx
            .recv_timeout(STEP_LIMIT)
            .unwrap_or_else(|_| panic!("shutdown_now did not return within {STEP_LIMIT:?}"));
        assert!(joined_all, "the worker was not joined");

        let expected = two_phase_commit_execs(N);
        let begun = MID_GRAPH_BEGUN.load(Ordering::SeqCst);
        assert_eq!(
            begun, expected,
            "the worker began {begun} of the graph's {expected} executions before exiting: a \
             shutdown requested mid-graph abandoned the rest of it"
        );
        assert_eq!(
            stats.lock().unwrap().execs,
            expected,
            "the pool's stats do not account for the whole graph"
        );
        assert!(
            queue.lock().unwrap().is_empty(),
            "the graph left work queued that nothing will ever take"
        );
        eprintln!("mid-graph shutdown: the graph ran to completion, {begun} executions");
    }
    // --- Pattern 10: the request is published when the lock is released -----

    /// Counts the executions [`after_store_child`]'s program begins.
    static AFTER_STORE_BEGUN: AtomicUsize = AtomicUsize::new(0);

    /// One attempt at the window pattern 10 needs.
    enum Published {
        /// The request was published while the worker was still asleep on
        /// queued work, so the witnesses were taken in the window.
        Valid,
        /// The worker's poll fired before the request was made and it took the
        /// staged token on its own: no window.
        Void(String),
    }

    /// Stages the state §5.2's rule is about — a worker asleep on work that is
    /// already queued — publishes a shutdown request into it, and holds
    /// `shutdown_now` **inside** the critical section that published it.
    ///
    /// The pause point is the last statement in that block, so in the window
    /// the queue lock is held by the stopped `shutdown_now` and nothing about
    /// the queue can move: no worker can take the staged token, and the test
    /// itself must not touch the queue — it would block on that lock until the
    /// release. That is what the position buys.
    ///
    /// **Witness 1, the rule itself.** The flag is already set while that lock
    /// is still held, so the store happened inside the critical section. A
    /// mutation that lifts the store out of the block lands after this marker
    /// and leaves the flag unset here, whichever side of the marker line it is
    /// written on.
    ///
    /// **Witness 2, the consequence.** Released and woken, the worker takes
    /// nothing. Under the rule that is forced — the store happened under the
    /// very lock the take needs, before any worker could acquire it — but
    /// against a mutation that publishes late it is a race, since such a
    /// mutation stores within nanoseconds of the release. So witness 2
    /// confirms; witness 1 discriminates. See [`Point::AfterShutdownStore`].
    ///
    /// Without the pause point neither witness exists: `shutdown_now` runs on
    /// into the per-worker writes and the join, and the interval the rule is
    /// about is a few instructions wide.
    ///
    /// Staging races the worker's 250 ms poll: if it wakes on its own and takes
    /// the token before the request is made there is no window, and the attempt
    /// is [`Published::Void`] and retried. That is decided by the worker's own
    /// state rather than by the queue, both because the queue is unreadable
    /// here and because `Busy` is written under the same lock as the take, so a
    /// worker that has taken the token is `Busy` before it lets go of it.
    fn stage_one_publication() -> Published {
        AFTER_STORE_BEGUN.store(0, Ordering::SeqCst);
        let (mut pool, states) = pool(1);
        let queue = pool.work_deque.clone();
        let wake = pool.loop_block_cond.clone();
        let requested = pool.shutdown.clone();
        let stats = pool.exec_stats.clone();
        let program = Arc::new(|| {
            AFTER_STORE_BEGUN.fetch_add(1, Ordering::SeqCst);
        });
        pool.worker_vec.iter_mut().for_each(|w| w.start(&program));

        // Let the worker settle into its wait, then queue a start token
        // **without notifying it**, so it stays asleep holding no lock with
        // work in the queue that it would take on its next pass.
        wait_for("the worker to go idle", || {
            count_in(&states, ExecutionPoolWorkerState::Waiting) == 1
        });
        queue.lock().unwrap().push_back(None);

        test_hooks::arm(Point::AfterShutdownStore);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let joined_all = pool.shutdown_now();
            let _ = tx.send(joined_all);
        });
        wait_until_paused();

        // Witness 1. The lock is held here, so this reads the flag as of a
        // moment strictly inside the critical section. Nothing in this window
        // may touch `queue`: it would block until the release.
        let published = requested.load(Ordering::SeqCst);
        let begun_at_request = AFTER_STORE_BEGUN.load(Ordering::SeqCst);
        let busy_at_request = count_in(&states, ExecutionPoolWorkerState::Busy);
        if begun_at_request > 0 || busy_at_request > 0 {
            // The worker's own poll beat the request to the token, so this
            // attempt says nothing about the rule. It cannot have taken it
            // *after* the request: the store, the notification and this marker
            // are one critical section, and the take needs that same lock.
            test_hooks::release();
            let _ = rx.recv_timeout(STEP_LIMIT);
            return Published::Void(format!(
                "the worker's poll fired first and it took the staged token \
                 (executions begun {begun_at_request}, workers busy {busy_at_request})"
            ));
        }

        // The window closes here: the block ends, and the queue lock is free.
        test_hooks::release();

        // Witness 2. The notification is the test's own, so this witness does
        // not depend on `shutdown_now`'s — that is pattern 6's property.
        wake.notify_all();
        wait_for(
            "the woken worker to take the token or leave its loop",
            || AFTER_STORE_BEGUN.load(Ordering::SeqCst) > 0 || Arc::strong_count(&program) == 1,
        );
        let took = AFTER_STORE_BEGUN.load(Ordering::SeqCst);
        // The worker's clone of the program is dropped when `worker_loop`
        // returns, so one reference left is the worker gone.
        let left_the_loop = Arc::strong_count(&program) == 1;

        let joined_all = rx
            .recv_timeout(STEP_LIMIT)
            .unwrap_or_else(|_| panic!("shutdown_now did not return within {STEP_LIMIT:?}"));

        assert!(
            published,
            "the shutdown flag was still unset at this marker, which is reached with the queue \
             lock still held: either the store is outside the critical section — so a worker \
             can hold that lock, read no request, and take work or go to sleep with the request \
             already made — or it is inside the section but below this marker, which keeps the \
             rule and which this witness cannot tell apart (see `Point::AfterShutdownStore`)"
        );
        assert_eq!(
            took, 0,
            "the woken worker began {took} execution(s) of work queued before the request was \
             made: its read of the flag under the queue lock did not observe the request"
        );
        assert_eq!(
            queue.lock().unwrap().len(),
            1,
            "the staged token is gone: the worker took it after the request was published"
        );
        assert_eq!(
            stats.lock().unwrap().execs,
            0,
            "the pool recorded executions from work taken after the request was published"
        );
        assert!(
            left_the_loop,
            "the woken worker neither took the item nor left its loop"
        );
        assert!(joined_all, "the worker was not joined");
        Published::Valid
    }

    /// The shutdown request is published inside the critical section that
    /// holds the queue lock: a worker that takes that lock next observes it.
    ///
    /// This is §5.2's rule as stated — *the store and the notify happen while
    /// holding the queue lock* — rather than the weaker property the other
    /// patterns pin between them, which is that `shutdown_now` contends for
    /// that lock before it stores. Pattern 6 catches only the form of the
    /// violation that does not take the lock at all, and catches it through the
    /// blocking of the acquisition; the form that keeps the acquisition and
    /// moves only the store after it survives all nine
    /// which passed every test that existed before this one.
    ///
    /// **With the store moved out of the critical section** the flag is still
    /// unset at [`Point::AfterShutdownStore`], which is reached with the lock
    /// still held, and the test fails on that witness. **Both** ways of writing
    /// that mutation die, because there is no gap between the store and the
    /// marker for one of them to sit in; with the marker one line lower, one of
    /// the two survived; from inside the critical section both are caught.
    ///
    /// It fails **without the pre-take flag check** as well, on its second
    /// witness: the worker released here wakes, takes the staged token and
    /// begins it. Pattern 8 establishes that by another route.
    #[test]
    fn the_request_is_published_inside_the_critical_section() {
        run_child("exec_pool::tests::after_store_child");
    }

    #[test]
    #[ignore = "run in a child process by the_request_is_published_inside_the_critical_section"]
    fn after_store_child() {
        const ATTEMPTS: u32 = 5;
        let _serial = hooks_lock();
        let _disarm = Disarm;

        for attempt in 0..ATTEMPTS {
            match stage_one_publication() {
                Published::Valid => {
                    eprintln!(
                        "publication: staged on attempt {attempt}: the request was already set \
                         with the queue lock still held, and the woken worker left the token \
                         queued"
                    );
                    return;
                }
                Published::Void(why) => {
                    eprintln!("publication: attempt {attempt} came to nothing: {why}")
                }
            }
        }
        panic!("could not stage a publication window in {ATTEMPTS} attempts");
    }
}
