use crate::event::Event;
use crate::must::Must;
use crate::runtime::failure::{init_panic_hook, persist_task_failure};
//experimental. Unfinished. use crate::runtime::storage::{StorageKey, StorageMap};
use crate::runtime::task::{Task, TaskId, DEFAULT_INLINE_TASKS};
use crate::runtime::thread::continuation::PooledContinuation;
use scoped_tls::scoped_thread_local;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::panic;
use std::rc::Rc;

// We use this scoped TLS to smuggle the ExecutionState, which is not 'static, across tasks that
// need access to it (to spawn new tasks, interrogate task status, etc).
scoped_thread_local! {
    static EXECUTION_STATE: RefCell<ExecutionState>
}

/// An `Execution` encapsulates a single run of a function under test against a chosen scheduler.
/// Its only useful method is `Execution::run`, which executes the function to completion.
///
/// The key thing that an `Execution` manages is the `ExecutionState`, which contains all the
/// mutable state a test's tasks might need access to during execution (to block/unblock tasks,
/// spawn new tasks, etc). The `Execution` makes this state available through the `EXECUTION_STATE`
/// static variable, but clients get access to it by calling `ExecutionState::with`.
pub(crate) struct Execution {
    must: Rc<RefCell<Must>>,
}

impl Execution {
    /// Construct a new execution that will use the given scheduler. The execution should then be
    /// invoked via its `run` method, which takes as input the closure for task 0.
    pub(crate) fn new(must: Rc<RefCell<Must>>) -> Self {
        Self { must }
    }
}

impl Execution {
    /// Run a function to be tested, taking control of scheduling it and any tasks it might spawn.
    /// This function runs until `f` and all tasks spawned by `f` have terminated, or until the
    /// scheduler returns `None`, indicating the execution should not be explored any further.
    pub(crate) fn run<F>(mut self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let state = RefCell::new(ExecutionState::new(Rc::clone(&self.must)));

        let _guard = init_panic_hook();

        EXECUTION_STATE.set(&state, move || {
            // Spawn `f` as the first task
            ExecutionState::spawn_thread(
                f,
                self.must.borrow().config().stack_size,
                Some(format!("main-thread-{:?}", std::thread::current().id())),
            );

            // Run the test to completion
            while self.step() {}

            // Cleanup the state before it goes out of `EXECUTION_STATE` scope
            ExecutionState::cleanup();
        });
    }

    /// Execute a single step of the scheduler. Returns true if the execution should continue.
    #[inline]
    fn step(&mut self) -> bool {
        enum NextStep {
            Task(Rc<RefCell<PooledContinuation>>),
            Failure(String),
            Finished,
        }

        let next_step = ExecutionState::with(|state| {
            if let Err(msg) = state.schedule() {
                return NextStep::Failure(msg);
            }
            state.advance_to_next_task();

            match state.current_task {
                ScheduledTask::Some(tid) => {
                    let task = state.get(tid);
                    NextStep::Task(Rc::clone(&task.continuation))
                }
                ScheduledTask::Finished => {
                    // The scheduler decided we're finished, so there are no runnable tasks.
                    //Therefore, it's a deadlock if there are unfinished attached tasks.
                    if state.tasks.iter().any(|t| !t.finished()) {
                        let blocked_tasks = state
                            .tasks
                            .iter()
                            .filter(|t| !t.finished())
                            .map(|t| {
                                format!(
                                    "{} (task {})",
                                    t.name().unwrap_or_else(|| "<unknown>".to_string()),
                                    t.id().0,
                                )
                            })
                            .collect::<Vec<_>>();
                        NextStep::Failure(
                            format!("deadlock! blocked tasks: [{}]", blocked_tasks.join(", ")), // ,
                                                                                                // state.current_schedule.clone(),
                        )
                    } else {
                        NextStep::Finished
                    }
                }
                ScheduledTask::Stopped => NextStep::Finished,
                ScheduledTask::None => NextStep::Failure(
                    "no task was scheduled".to_string(),
                    // state.current_schedule.clone(),
                ),
            }
        });

        // Run a single step of the chosen task.
        let ret = match next_step {
            NextStep::Task(continuation) => panic::catch_unwind(panic::AssertUnwindSafe(|| {
                continuation.borrow_mut().resume()
            })),
            NextStep::Failure(
                msg, // , schedule
            ) => {
                // // Because we're creating the panic here, we don't need `persist_failure` to print
                // // as the failure message will be part of the panic payload.
                // TODO: I think this might be completely dead code because if this ever happens,
                // a panic was already caught, and the counterexample was saved, and the panic
                // handler was disarmed already. But it's hard to tell...
                let pos = ExecutionState::failure_info().map(|(_, pos)| pos);
                let message = persist_task_failure(msg, pos);
                panic!("{}", message);
            }
            NextStep::Finished => return false,
        };

        match ret {
            // Task finished
            Ok(true) => {
                // Inform Must later so that we record the return value
                ExecutionState::with(|state| state.current_mut().finish());
            }
            // Task yielded
            Ok(false) => {}
            // Task failed
            Err(e) => {
                let (name, pos) = ExecutionState::failure_info().unwrap();
                let message = persist_task_failure(name, Some(pos));
                // Try to inject the schedule into the panic payload if we can
                let payload: Box<dyn Any + Send> = match e.downcast::<String>() {
                    Ok(panic_msg) => {
                        Box::new(format!("{}\noriginal panic: {}", message, panic_msg))
                    }
                    Err(panic) => panic,
                };
                panic::resume_unwind(payload);
            }
        }

        true
    }
}

/// `ExecutionState` contains the portion of a single execution's state that needs to be reachable
/// from within a task's execution. It tracks which tasks exist and their states, as well as which
/// tasks are pending spawn.
pub(crate) struct ExecutionState {
    pub(crate) tasks: SmallVec<[Task; DEFAULT_INLINE_TASKS]>,
    // invariant: if this transitions to Stopped or Finished, it can never change again
    current_task: ScheduledTask,
    // the task the scheduler has chosen to run next
    next_task: ScheduledTask,

    // static values for the current execution
    //storage: StorageMap,
    pub must: Rc<RefCell<Must>>,
    #[cfg(debug_assertions)]
    has_cleaned_up: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ScheduledTask {
    None,         // no task has ever been scheduled
    Some(TaskId), // this task is running
    Stopped,      // the scheduler asked us to stop running
    Finished,     // all tasks have finished running
}

impl ScheduledTask {
    fn id(&self) -> Option<TaskId> {
        match self {
            ScheduledTask::Some(tid) => Some(*tid),
            _ => None,
        }
    }

    fn take(&mut self) -> Self {
        std::mem::replace(self, ScheduledTask::None)
    }
}

impl ExecutionState {
    fn new(must: Rc<RefCell<Must>>) -> ExecutionState {
        Self {
            tasks: SmallVec::new(),
            current_task: ScheduledTask::None,
            next_task: ScheduledTask::None,
            //storage: StorageMap::new(),
            must,
            #[cfg(debug_assertions)]
            has_cleaned_up: false,
        }
    }

    /// Invoke a closure with access to the current execution state. Library code uses this to gain
    /// access to the state of the execution to influence scheduling (e.g. to register a task as
    /// blocked).
    #[inline]
    pub(crate) fn with<F, T>(f: F) -> T
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        Self::try_with(f).expect("The TraceForge API (spawn, recv_msg, etc.) should be used only from threads launched using traceforge::spawn and only inside a TraceForge test.")
    }

    /// Like `with`, but returns None instead of panicing if there is no current ExecutionState or
    /// if the current ExecutionState is already borrowed.
    #[inline]
    pub(crate) fn try_with<F, T>(f: F) -> Option<T>
    where
        F: FnOnce(&mut ExecutionState) -> T,
    {
        if EXECUTION_STATE.is_set() {
            EXECUTION_STATE.with(|cell| {
                if let Ok(mut state) = cell.try_borrow_mut() {
                    Some(f(&mut state))
                } else {
                    None
                }
            })
        } else {
            None
        }
    }

    pub(crate) fn spawn_thread<F>(
        f: F,
        stack_size: usize,
        name: Option<String>,
        // mut initial_clock: Option<VectorClock>,
    ) -> TaskId
    where
        F: FnOnce() + Send + 'static,
    {
        Self::with(|state| {
            let task_id = TaskId(state.tasks.len());
            let task = Task::from_closure(f, stack_size, task_id, name);
            state.tasks.push(task);
            task_id
        })
    }

    /// Prepare this ExecutionState to be dropped. Call this before dropping so that the tasks have
    /// a chance to run their drop handlers while `EXECUTION_STATE` is still in scope.
    fn cleanup() {
        // A slightly delicate dance here: we need to drop the tasks from outside of `Self::with`,
        // because a task's Drop impl might want to call back into `ExecutionState` (to check
        // `should_stop()`). So we pull the tasks out of the `ExecutionState`, leaving it in an
        // invalid state, but no one should still be accessing the tasks anyway.
        let (mut tasks, final_state) = Self::with(|state| {
            assert!(
                state.current_task == ScheduledTask::Stopped
                    || state.current_task == ScheduledTask::Finished
            );
            (
                std::mem::replace(&mut state.tasks, SmallVec::new()),
                state.current_task,
            )
        });

        for task in tasks.drain(..) {
            assert!(
                final_state == ScheduledTask::Stopped || task.finished(),
                "execution finished but task is not"
            );
            Rc::try_unwrap(task.continuation)
                .map_err(|_| ())
                .expect("couldn't cleanup a future");
        }

        // while Self::with(|state| state.storage.pop()).is_some() {}

        #[cfg(debug_assertions)]
        Self::with(|state| state.has_cleaned_up = true);
    }

    /// Invoke the scheduler to decide which task to schedule next. Returns true if the chosen task
    /// is different from the currently running task, indicating that the current task should yield
    /// its execution.
    pub(crate) fn maybe_yield() -> bool {
        Self::with(|state| {
            debug_assert!(
                matches!(state.current_task, ScheduledTask::Some(_))
                    && state.next_task == ScheduledTask::None,
                "we're inside a task and scheduler should not yet have run"
            );

            let result = state.schedule();
            // If scheduling failed, yield so that the outer scheduling loop can handle it.
            if result.is_err() {
                return true;
            }

            // If the next task is the same as the current one, we can skip the context switch
            // and just advance to the next task immediately.
            if state.current_task == state.next_task {
                state.advance_to_next_task();
                false
            } else {
                true
            }
        })
    }

    /// Generate some diagnostic information used when persisting failures.
    ///
    /// Because this method may be called from a panic hook, it must not panic.
    pub(crate) fn failure_info() -> Option<(String, Event)> {
        let fi: Option<Option<(String, Event)>> = Self::try_with(|state| {
            if let Some(task) = state.try_current() {
                let name = task
                    .name()
                    .unwrap_or_else(|| format!("task-{:?}", task.id().0));
                Some((name, state.curr_pos()))
            } else {
                None
            }
        });
        fi.flatten()
    }

    pub(crate) fn current(&self) -> &Task {
        self.get(self.current_task.id().unwrap())
    }

    pub(crate) fn current_mut(&mut self) -> &mut Task {
        self.get_mut(self.current_task.id().unwrap())
    }
    pub(crate) fn try_current(&self) -> Option<&Task> {
        self.try_get(self.current_task.id()?)
    }

    pub(crate) fn get(&self, id: TaskId) -> &Task {
        self.try_get(id).unwrap()
    }

    pub(crate) fn get_mut(&mut self, id: TaskId) -> &mut Task {
        self.tasks.get_mut(id.0).unwrap()
    }

    pub(crate) fn try_get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(id.0)
    }

    /*
        #[allow(dead_code)] // still implementing thread local storage
        pub(crate) fn get_storage<K: Into<StorageKey>, T: 'static>(&self, key: K) -> Option<&T> {
            self.storage
                .get(key.into())
                .map(|result| result.expect("global storage is never destructed"))
        }

        #[allow(dead_code)] // still implementing thread local storage
        pub(crate) fn init_storage<K: Into<StorageKey>, T: 'static>(&mut self, key: K, value: T) {
            self.storage.init(key.into(), value);
        }
    */

    pub(crate) fn next_pos(&mut self) -> Event {
        let tid = self.must.borrow().to_thread_id(self.current().id());
        self.current_mut().instructions += 1;
        let icount = self.current().instructions as u32;
        Event::new(tid, icount)
    }

    pub(crate) fn prev_pos(&mut self) -> Event {
        let tid = self.must.borrow().to_thread_id(self.current().id());
        self.current_mut().instructions -= 1;
        let icount = self.current().instructions as u32;
        Event::new(tid, icount)
    }

    pub(crate) fn curr_pos(&self) -> Event {
        let tid = self.must.borrow().to_thread_id(self.current().id());
        let icount = self.current().instructions as u32;
        Event::new(tid, icount)
    }

    /// Run the scheduler to choose the next task to run. `has_yielded` should be false if the
    /// scheduler is being invoked from within a running task. If scheduling fails, returns an Err
    /// with a String describing the failure.
    fn schedule(&mut self) -> Result<(), String> {
        // Don't schedule twice. If `maybe_yield` ran the scheduler, we don't want to run it
        // again at the top of `step`.
        if self.next_task != ScheduledTask::None {
            return Ok(());
        }

        let runnable = self
            .tasks
            .iter()
            .filter(|t| t.runnable())
            .map(|t| (t.id, t.instructions))
            .collect::<SmallVec<[_; DEFAULT_INLINE_TASKS]>>();

        // We should finish execution when there are no runnable tasks.
        if runnable.is_empty() {
            self.next_task = ScheduledTask::Finished;
            return Ok(());
        }

        self.next_task = self
            .must
            .borrow_mut()
            .next_task(&runnable, self.current_task.id())
            .map(ScheduledTask::Some)
            .unwrap_or(ScheduledTask::Stopped);

        // trace!(?runnable, next_task=?self.next_task);

        Ok(())
    }

    /// Set the next task as the current task, and update our tracing span
    fn advance_to_next_task(&mut self) {
        debug_assert_ne!(self.next_task, ScheduledTask::None);
        self.current_task = self.next_task.take();
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(self.current_task, ScheduledTask::Some(_))
    }
}

#[cfg(debug_assertions)]
impl Drop for ExecutionState {
    fn drop(&mut self) {
        assert!(self.has_cleaned_up || std::thread::panicking());
    }
}
