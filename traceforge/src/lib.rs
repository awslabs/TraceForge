// #![doc = include_str!("../../README.md")]
pub mod channel;
mod cons;
pub mod coverage;
pub use coverage::{CoverageInfo, ExecutionId};

mod event;
mod event_label;
mod exec_graph;
mod exec_pool;
pub mod future;
// pub mod turmoil; // working on tcp support
// mod experimental_runtimes;
mod identifier;
mod indexed_map;
pub mod loc;
pub mod monitor_types;
pub mod msg;
mod must;
mod predicate;
mod replay;
mod revisit;
mod runtime;
pub mod sync;
mod telemetry;
mod testmode;
use future::spawn_receive;
pub use testmode::{parallel_test, test};

pub mod thread;
mod vector_clock;

pub use crate::msg::Val; // `Val` is used by monitors.

use channel::{cons_to_model, self_loc_comm, thread_loc_comm, Receiver};
use coverage::ExecutionObserver;
use event_label::{Block, BlockType, CToss, Choice, RecvMsg, SendMsg};
use loc::{CommunicationModel, Loc, RecvLoc, SendLoc};
use msg::Message;

use rand::{prelude::*, rngs::OsRng, RngCore};
use replay::ReplayInformation;
use runtime::execution::{Execution, ExecutionState};
use runtime::failure::persist_task_failure;
use runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use runtime::thread::switch;

use log::{info, trace};
use serde::{Deserialize, Serialize};
use smallvec::alloc::sync::Arc;
use std::cell::RefCell;
use std::future::Future;
use std::iter;
use std::rc::Rc;
use std::time::Instant;
use thread::{spawn_without_switch, JoinHandle, ThreadId};

use crate::event_label::*;
use crate::exec_pool::ExecutionPool;
use crate::must::{MonitorInfo, Must};
use crate::predicate::PredicateType;

use std::any::type_name;

fn type_of<T>(_: &T) -> &'static str {
    type_name::<T>()
}

/// TraceForge exploration statistics.
#[derive(Default, Clone, Debug)]
pub struct Stats {
    /// Number of complete executions explored
    pub execs: usize,
    /// Number of blocked executions explored
    pub block: usize,
    // Aggregate coverage information
    pub coverage: CoverageInfo,
}

impl Stats {
    pub(crate) fn add(&mut self, rhs: &Stats) {
        self.execs += rhs.execs;
        self.block += rhs.block;
        self.coverage.merge(&rhs.coverage);
    }
}

/// Available scheduling policies for TraceForge.
///
/// These have no outcome on the number of executions
/// explored by TraceForge; they are mostly useful for debugging.
#[derive(PartialEq, Eq, Default, Clone, Copy, Serialize, Deserialize, Debug)]
pub enum SchedulePolicy {
    /// left-to-right (default)
    #[default]
    LTR,
    /// arbitrary
    Arbitrary,
}

/// Available TraceForge modes. These are not set directly
/// by the user, but rather by the way TraceForge is called
/// (e.g., [`verify`] vs [`estimate`])
#[derive(PartialEq, Clone, Copy, Serialize, Deserialize, Debug)]
pub(crate) enum ExplorationMode {
    Verification,
    Estimation,
}

/// Available consistency models
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ConsType {
    /// Totally-ordered coherence (deprecated)
    MO,
    /// Unordered channels
    Bag,
    /// Use FIFO instead.
    #[deprecated]
    WB,
    /// FIFO channels
    FIFO,
    /// Use Causal instead
    #[deprecated]
    CD,
    /// Causal Delivery
    Causal,
    /// Mailbox Delivery
    Mailbox,
}

/// Manually implement Serialize so that we can avoid the compile warning
/// caused by the macro expansion of #[derive(Serialize)] on deprecated
/// enum members.
impl Serialize for ConsType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            ConsType::MO => "MO",
            ConsType::Bag => "Bag",
            #[allow(deprecated)]
            ConsType::WB => "WB",
            ConsType::FIFO => "FIFO",
            #[allow(deprecated)]
            ConsType::CD => "CD",
            ConsType::Causal => "Causal",
            ConsType::Mailbox => "Mailbox",
        })
    }
}

/// Manually implement Deserialize so that we can avoid the compile
/// warning caused by the macro expansion of #[derive(Deserialize)]
/// on deprecated enum members.
impl<'de> Deserialize<'de> for ConsType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "MO" => Ok(ConsType::MO),
            "Bag" => Ok(ConsType::Bag),
            #[allow(deprecated)]
            "WB" => Ok(ConsType::WB),
            "FIFO" => Ok(ConsType::FIFO),
            #[allow(deprecated)]
            "CD" => Ok(ConsType::CD),
            "Causal" => Ok(ConsType::Causal),
            "Mailbox" => Ok(ConsType::Mailbox),
            _ => Err(serde::de::Error::custom(format!(
                "Invalid ConsType variant: {}",
                s
            ))),
        }
    }
}

/// TraceForge configuration options.
///
/// Use the [`ConfigBuilder`] class to construct a `Config` struct.
#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub(crate) stack_size: usize,
    pub(crate) progress_report: usize,
    pub(crate) thread_threshold: u32,
    pub(crate) warnings_as_errors: bool,
    pub(crate) keep_going_after_error: bool,
    pub(crate) mode: ExplorationMode,
    pub(crate) cons_type: ConsType,
    pub(crate) schedule_policy: SchedulePolicy,
    pub(crate) max_iterations: Option<u64>,
    pub(crate) verbose: usize,
    pub(crate) seed: u64,
    pub(crate) symmetry: bool,
    pub(crate) vr: bool,
    pub(crate) lossy_budget: usize,
    pub(crate) dot_file: Option<String>,
    pub(crate) trace_file: Option<String>,
    pub(crate) error_trace_file: Option<String>,
    pub(crate) turmoil_trace_file: Option<String>,
    pub(crate) parallel: bool,
    pub(crate) parallel_workers: Option<usize>,
    #[serde(skip)]
    pub(crate) callbacks: Arc<Mutex<Vec<Box<dyn ExecutionObserver + Send>>>>,
}

impl Config {
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::new()
    }

    pub(crate) fn rename_files(&mut self, suffix: String) {
        if let Some(dot) = &self.dot_file {
            self.dot_file = Some(dot.to_owned() + &suffix);
        }
        if let Some(trace) = &self.trace_file {
            self.trace_file = Some(trace.to_owned() + &suffix);
        }
        if let Some(turmoiltf) = &self.turmoil_trace_file {
            self.turmoil_trace_file = Some(turmoiltf.to_owned() + &suffix);
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        ConfigBuilder::new().build()
    }
}

/// Builds a [`Config`] struct.
pub struct ConfigBuilder(Config);

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigBuilder {
    pub fn new() -> Self {
        ConfigBuilder(Config {
            stack_size: 0x8000,
            progress_report: 0,
            thread_threshold: 1000,
            warnings_as_errors: false,
            keep_going_after_error: false,
            mode: ExplorationMode::Verification,
            cons_type: ConsType::FIFO,
            schedule_policy: SchedulePolicy::LTR,
            max_iterations: None,
            verbose: 0,
            seed: OsRng.next_u64(),
            symmetry: false,
            vr: false,
            lossy_budget: 0,
            dot_file: None,
            trace_file: None,
            error_trace_file: None,
            turmoil_trace_file: None,
            parallel: false,
            parallel_workers: None,
            callbacks: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Checks whether the current config is valid and
    /// returns it if it is. Raises an error otherwise
    fn check_valid(self) -> Self {
        if self.0.symmetry {
            panic!("Symmetry reduction is currently not supported")
        }
        if self.0.symmetry && self.0.schedule_policy == SchedulePolicy::Arbitrary {
            eprintln!("Symmetry reduction can only be used with LTR!");
            std::process::exit(exitcode::CONFIG);
        } else {
            self
        }
    }

    /// Determines TraceForge's running mode:
    /// Verification is for exhaustive exploration
    /// Estimation is for Monte Carlo estimation
    ///
    /// This is not something a user will set as a parameter.
    /// Instead, the mode is set by the top level routines `verify` or `estimate`
    #[allow(dead_code)]
    pub(crate) fn with_mode(mut self, m: ExplorationMode) -> Self {
        self.0.mode = m;
        self
    }

    /// Specifies the default stack size for user threads
    pub fn with_stack_size(mut self, s: usize) -> Self {
        self.0.stack_size = s;
        self
    }

    /// Prints a progress report message after every "n" executions.
    /// This is useful, when you are waiting for models with large numbers of executions.
    ///
    /// Note that if you do not specify this option, you will get the default behavior,
    /// an adaptive progress report that prints after 1, 2, 3, ..., 10, 20, 30, ... 100, 200, 300, etc.
    ///
    /// To completely disable such output, use with_progress_report(u32::MAX)
    pub fn with_progress_report(mut self, n: usize) -> Self {
        self.0.progress_report = n;
        self
    }

    /// Specifies the thread size above which TraceForge warns for infinite executions.
    /// That is, if a thread has more than `s` many events, a warning is printed on the console
    pub fn with_thread_threshold(mut self, s: u32) -> Self {
        self.0.thread_threshold = s;
        self
    }

    /// Whether to treat warnings as actual errors
    pub fn with_warnings_as_errors(mut self, b: bool) -> Self {
        self.0.warnings_as_errors = b;
        self
    }

    /// Allow the exploration to continue even after an assertion violation
    /// has been discovered. Works only with `traceforge::assert`s since unlike `std::assert`,
    /// it does not panic
    pub fn with_keep_going_after_error(mut self, b: bool) -> Self {
        self.0.keep_going_after_error = b;
        self
    }

    /// Specifies the consistency model for TraceForge
    pub fn with_cons_type(mut self, t: ConsType) -> Self {
        self.0.cons_type = t;
        self
    }

    /// Specifies the scheduling policy for TraceForge
    pub fn with_policy(mut self, p: SchedulePolicy) -> Self {
        self.0.schedule_policy = p;
        self
    }

    /// Specifies an upper bound on the number of iterations
    pub fn with_max_iterations(mut self, n: u64) -> Self {
        self.0.max_iterations = Some(n);
        self
    }

    /// Controls how much input is printed in `stdout`
    /// 0 = default, sparse information
    /// 1 = more information, and print the execution graph every time it's blocked.
    /// 2 = even more information, and also print the graph of every execution, whether blocked or not.
    ///
    /// Note that you can **ALSO** get more information by increasing the log level
    /// by initializing the standard Rust logging.
    pub fn with_verbose(mut self, v: usize) -> Self {
        self.0.verbose = v;
        self
    }

    /// Seeds TraceForge's random number gneerator.
    /// Has no effect without `[SchedulePolicy::Random]`
    /// being the selected scheduling policy.
    pub fn with_seed(mut self, s: u64) -> Self {
        self.0.seed = s;
        self
    }

    /// Enables symmetry reduction
    pub fn with_symmetry(mut self, s: bool) -> Self {
        self.0.symmetry = s;
        self
    }

    /// Enables value reduction
    pub fn with_value(self, _s: bool) -> Self {
        panic!("Value reduction is currently not supported")
    }

    /// Consider executions where up to `budget` lossy messages are dropped.
    pub fn with_lossy(mut self, budget: usize) -> Self {
        self.0.lossy_budget = budget;
        self
    }

    /// Whenever the execution graph is printed, the same
    /// information will be written to this file in DOT format.
    ///
    /// Note that this is not printed when a counterexample is generated.
    ///
    /// See with_verbose() for more information
    pub fn with_dot_out(mut self, filename: &str) -> Self {
        self.0.dot_file = Some(filename.to_string());
        self
    }

    /// Whenever the execution graph is printed, the same
    /// information will be written to this file in text format.
    ///
    /// Note that this is not printed when a counterexample is generated.
    ///
    /// See with_verbose() for more information
    pub fn with_trace_out(mut self, filename: &str) -> Self {
        self.0.trace_file = Some(filename.to_string());
        self
    }

    /// Enables trace printing that can be read by turmoil in addition to console printing
    pub fn with_turmoil_trace_out(mut self, filename: &str) -> Self {
        self.0.turmoil_trace_file = Some(filename.to_string());
        self
    }

    /// If a counterexample is detected, a trace will be written to this file.
    /// This trace will allow you to replay the execution if you call
    /// replay(must_program, "/path/to/error/trace").
    /// "must_program" must be the same function/closure that generated
    /// the counterexample.
    pub fn with_error_trace(mut self, filename: &str) -> Self {
        self.0.error_trace_file = Some(filename.to_string());
        self
    }

    /// Enables parallel processing of model. By default the number of system
    /// cores is chosen as for the max worker count unless .with_parallel_workers()
    /// explicitly sets a value or env var MUST_PARALLEL_WORKERS is set.
    pub fn with_parallel(mut self, use_parallel: bool) -> Self {
        self.0.parallel = use_parallel;
        self
    }

    /// Sets the max number of parallel workers. None implies using using the
    /// number of available cores in the system unless overridden by env var
    /// MUST_PARALLEL_WORKERS. Requires that .with_parallel(true) is also set.
    pub fn with_parallel_workers(mut self, max_workers: usize) -> Self {
        self.0.parallel_workers = Some(max_workers);
        self
    }

    /// Registers a callback that is called at the end of an execution by the model checker
    ///
    pub fn with_callback(self, cb: Box<dyn ExecutionObserver + Send>) -> Self {
        self.0
            .callbacks
            .lock()
            .expect("Could not lock callbacks configuration")
            .push(cb);
        self
    }

    /// Consumes the builder and produces the [`Config`]
    pub fn build(self) -> Config {
        self.check_valid().0
    }
}

/// Model Checker API
///
/// Verifies `f` under the options specified in `conf`.
/// `f` acts as the main thread and may spawn other threads.
pub fn verify<F>(conf: Config, f: F) -> Stats
where
    F: Fn() + Send + Sync + 'static,
{
    let f = Arc::new(f);
    if conf.parallel {
        ExecutionPool::new(&conf).explore(&f)
    } else {
        let must = Rc::new(RefCell::new(Must::new(conf, false)));
        explore(&must, &f);
        let stats = must.borrow().stats();
        stats
    }
}

/// Model Checker API
///
/// Replays `f` using `replay_info`.
pub fn replay<F>(f: F, error_file: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    let replay_str = std::fs::read_to_string(error_file).unwrap();
    let replay_info: ReplayInformation = serde_json::from_str(&replay_str).unwrap();

    // Enable verbose logging for counterexamples even if it wasn't enabled before.
    // This is sort of a hack until I can refactor `replay` to allow you to
    // pass a config to replay.
    replay_info.config().verbose = 2;

    let must = Rc::new(RefCell::new(Must::new(replay_info.config(), true)));
    let f = Arc::new(f);

    info!("Sorted Execution Graph:");
    info!("{}", replay_info.sorted_error_graph());

    // Add the error graph to this new instance of TraceForge
    must.borrow_mut().load_replay_information(replay_info);

    explore(&must, &f);
}

/// Estimates the number of executions the program needs
/// in order to be verified
// The return value can be `Inf` to denote the estimate is too large for a `f64` representation
//
pub fn estimate_execs<F>(f: F) -> f64
where
    F: Fn() + Send + Sync + 'static,
{
    estimate_execs_with_samples(f, 1000)
}

/// Same as [`estimate_execs`] but with a user-defined number of
/// samples
/// The return value can be `Inf` to denote the estimate is too large for a `f64` representation
pub fn estimate_execs_with_samples<F>(f: F, samples: u128) -> f64
where
    F: Fn() + Send + Sync + 'static,
{
    assert!(samples > 0);

    estimate_execs_with_config(Config::builder().build(), f, samples)
}

/// There is no way to write a counterexample file without using this function.
/// There is a good question though about what name we should offer this to
/// customers under.
pub fn estimate_execs_with_config<F>(mut config: Config, f: F, samples: u128) -> f64
where
    F: Fn() + Send + Sync + 'static,
{
    config.mode = ExplorationMode::Estimation;
    config.schedule_policy = SchedulePolicy::LTR;
    config.cons_type = ConsType::FIFO;

    let f = Arc::new(f);

    let num_samples = samples;
    let mut estimate_sum: f64 = 0.0;
    let mut nb_executions = 0;
    for _ in 0..num_samples {
        let must = Rc::new(RefCell::new(Must::new(config.clone(), false)));
        explore(&must, &f);
        estimate_sum += must.borrow().execs_est();
        let stats = must.borrow().stats();
        nb_executions += stats.execs + stats.block;
    }
    info!("[lib.rs] ESTIMATE ran {} executions", nb_executions);
    estimate_sum / (num_samples as f64)
}

fn explore<F>(must: &Rc<RefCell<must::Must>>, f: &Arc<F>)
where
    F: Fn() + Send + Sync + 'static,
{
    must.borrow_mut().started_at = Instant::now();
    Must::set_current(Some(must.clone()));
    CONTINUATION_POOL.set(&ContinuationPool::new(), || loop {
        let f = Arc::clone(f);
        let execution = Execution::new(Rc::clone(must));
        Must::begin_execution(must);
        execution.run(move || f());
        if Must::complete_execution(must) {
            // `done` internally calls `run_metrics_after`
            break;
        }
    });
    // end of model checking
    must.borrow_mut().run_metrics_at_end();
}

///
/// Monitor API
///
/// MonitorCreateFn is a type which packages up the sender and receiver's thread ID into
/// a new actor message which is of the right type for the monitor to receive
type MonitorCreateFn = fn(ThreadId, ThreadId, Val) -> Option<Val>;
/// MonitorAcceptorFn returns true if the monitor should receive this message.
/// It's better to return false here than to have the monitor receive the message and ignore
/// the message because if the monitor receives but ignores it, this reduces the optimization
/// of DPOR.
type MonitorAcceptorFn = fn(ThreadId, ThreadId, Val) -> bool;

pub fn spawn_monitor<F, T>(
    monitor_function: F,
    create_fn: MonitorCreateFn,
    acceptor_fn: MonitorAcceptorFn,
    monitor: Arc<Mutex<dyn Monitor>>,
) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Message + 'static,
{
    ExecutionState::with(|s| s.must.borrow().validate_monitor_spawn(&s.curr_pos()));

    let jh = spawn_without_switch(monitor_function, None, true, None, None);

    // Register the monitor before calling switch(). You need to register it before
    // calling switch() because during replay, the replay execute the monitor first, find the monitor to
    // be blocked, and not run any more of the TraceForge program, which leads to a situation
    // where the monitor never gets registered at all during replay.
    let thread_id = jh.thread().id();
    ExecutionState::with(|s| {
        info!("[lib.rs] Registering monitor: {:?}", thread_id);

        let monitor_info = MonitorInfo {
            thread_id,
            create_fn,
            acceptor_fn,
            monitor_struct: monitor,
        };
        s.must.borrow_mut().handle_register_mon(monitor_info);
    });

    // Safe to switch() now
    switch();
    jh
}

/// Makes a value available for inspection at the end of an execution
pub fn publish<T: Message + 'static>(val: T) {
    ExecutionState::with(|s| {
        let thread_id = s.must.borrow().to_thread_id(s.current().id());
        s.must.borrow_mut().publish(thread_id, val);
    });
}

// Ensure that select has non-overlapping locations
fn validate_locs(locs: &Vec<&Loc>) {
    for (i, c1) in locs.iter().enumerate() {
        for c2 in locs.iter().skip(i + 1) {
            if c1 == c2 {
                panic!("Detected duplicate channel {:?} in select", c1);
            }
        }
    }
}

// Helper to query the execution about some necessary info
fn get_execution_state_info() -> (ThreadId, CommunicationModel) {
    ExecutionState::with(|s| {
        (
            s.curr_pos().thread,
            cons_to_model(s.must.borrow().config.cons_type),
        )
    })
}

// Validate that the message has the correct underlying type, and panic otherwise
fn expect_msg<T: 'static>(val: Val) -> T {
    match val.as_any().downcast::<T>() {
        Ok(v) => *v,
        Err(_) => {
            panic!(
                "wrong message return type; expecting {} but got {}",
                type_name::<T>(),
                val.type_name
            );
        }
    }
}

// A heterogenous select, only to be used for the async_recv implementation
pub(crate) fn select_val_block<'a, T: Message + 'static, U: Message + 'static>(
    // The main receiver
    primary: &'a Receiver<T>,
    // A secondary receiver, usually a one-shot-like channel whose communication model doesn't matter
    secondary: &'a Receiver<U>,
) -> (Val, usize) {
    let locs = iter::once(&primary.inner).chain(iter::once(&secondary.inner));
    // *Only* use main recv's communication model
    let comm = primary.comm;
    recv_val_block_with_tag(locs, comm, None)
}

///
/// Message API
///
/// Async API, unstable
pub fn async_recv_msg<T>(recv: &Receiver<T>) -> impl Future<Output = T>
where
    T: Message + Clone + 'static,
{
    futures::TryFutureExt::unwrap_or_else(spawn_receive(recv), |_| {
        panic!("Async receive future failed!")
    })
}

/// Select API, unstable
///
// TODO: If select_* are really necessary
// (given that one can directly use select on Futures),
// consider a macro that would allow heterogenous receives
// (select on Receivers of different types).
pub fn select_msg<'a, T: Message + 'static>(
    recvs: impl Iterator<Item = &'a &'a Receiver<T>>,
    comm: CommunicationModel,
) -> Option<(T, usize)> {
    let locs = recvs.map(|r| &r.inner);
    recv_msg_with_tag(locs, comm, None)
}

pub fn select_tagged_msg<'a, F, T>(
    recvs: impl Iterator<Item = &'a &'a Receiver<T>>,
    comm: CommunicationModel,
    f: F,
) -> Option<(T, usize)>
where
    F: Fn(ThreadId, Option<u32>) -> bool + 'static + Send + Sync,
    T: Message + 'static,
{
    let locs = recvs.map(|r| &r.inner);
    recv_msg_with_tag(locs, comm, Some(PredicateType(Arc::new(f))))
}

pub fn select_msg_block<'a, T: Message + 'static>(
    recvs: impl Iterator<Item = &'a &'a Receiver<T>>,
    comm: CommunicationModel,
) -> (T, usize) {
    let locs = recvs.map(|r| &r.inner);
    recv_msg_block_with_tag(locs, comm, None)
}

pub fn select_tagged_msg_block<'a, F, T>(
    recvs: impl Iterator<Item = &'a &'a Receiver<T>>,
    comm: CommunicationModel,
    f: F,
) -> (T, usize)
where
    F: Fn(ThreadId, Option<u32>) -> bool + 'static + Send + Sync,
    T: Message + 'static,
{
    let locs = recvs.map(|r| &r.inner);
    recv_msg_block_with_tag(locs, comm, Some(PredicateType(Arc::new(f))))
}

/// Main API

/// Sends to `t` the message `v`
pub fn send_msg<T: Message + 'static>(t: ThreadId, v: T) {
    let (loc, comm) = thread_loc_comm(t);
    send_msg_with_tag(v, None, &loc, comm, false)
}

/// Sends to `t` the message `v`, which can be lost
pub fn send_lossy_msg<T: Message + 'static>(t: ThreadId, v: T) {
    let (loc, comm) = thread_loc_comm(t);
    send_msg_with_tag(v, None, &loc, comm, true)
}

/// Sends to `t` the message `v` tagged with 'tag
pub fn send_tagged_msg<T: Message + 'static>(t: ThreadId, tag: u32, v: T) {
    let (loc, comm) = thread_loc_comm(t);
    send_msg_with_tag(v, Some(tag), &loc, comm, false)
}

/// Sends to `t` the message `v`, which can be lost, tagged with 'tag
pub fn send_tagged_lossy_msg<T: Message + 'static>(t: ThreadId, tag: u32, v: T) {
    let (loc, comm) = thread_loc_comm(t);
    send_msg_with_tag(v, Some(tag), &loc, comm, true)
}

/// Helper for [`send_msg`] and [`send_tagged_msg`]
fn send_msg_with_tag<T: Message + 'static>(
    v: T,
    tag: Option<u32>,
    loc: &Loc,
    comm: CommunicationModel,
    lossy: bool,
) {
    switch();
    ExecutionState::with(|s| {
        // creating the send label for the system send
        let pos = s.next_pos();
        let sender_tid = pos.thread;
        let val = Val::new(v);
        let mut monitor_msgs = MonitorSends::new();

        // Monitors can also observe explicit-channel messages.
        for (thread_id, mon) in s.must.borrow_mut().monitors().iter() {
            let monitor_accepts_this_msg = (mon.acceptor_fn)(pos.thread, sender_tid, val.clone());
            if monitor_accepts_this_msg {
                let mvalue = (mon.create_fn)(pos.thread, sender_tid, val.clone());
                if let Some(mv) = mvalue {
                    trace!(
                        "Produced value {:?} of type {}",
                        mv,
                        String::from(type_of(&mv))
                    );
                    monitor_msgs.insert(*thread_id, mv);
                }
            }
        }
        trace!(
            "[lib.rs] The number of required monitor messages {}",
            monitor_msgs.len()
        );

        let slab = SendMsg::new(
            pos,
            SendLoc::new(loc, sender_tid, tag),
            comm,
            val,
            monitor_msgs,
            lossy,
        );

        let maybe_stuck = s.must.borrow_mut().handle_send(slab);
        maybe_stuck.iter().for_each(|r| {
            let task = match s.must.borrow().to_task_id(r.thread) {
                Some(task) => task,
                None => return,
            };
            let task = s.get_mut(task);
            if !task.is_stuck() {
                return;
            }
            // If task is stuck waiting for the send,
            // the instruction counter is exactly one instruction behind.
            if task.instructions as u32 == r.index - 1 {
                task.unstuck();
            }
        });
    });
}

/// Returns a message from the thread queue or times out
pub fn recv_msg<T: Message + 'static>() -> Option<T> {
    let (loc, comm) = self_loc_comm();
    recv_msg_with_tag(iter::once(&loc), comm, None).map(|x| x.0)
}

/// Returns a tagged message from the thread queue or times out
pub fn recv_tagged_msg<F, T>(f: F) -> Option<T>
where
    F: Fn(ThreadId, Option<u32>) -> bool + 'static + Send + Sync,
    T: Message + 'static,
{
    let (loc, comm) = self_loc_comm();
    recv_msg_with_tag(iter::once(&loc), comm, Some(PredicateType(Arc::new(f)))).map(|x| x.0)
}

fn recv_msg_with_tag<'a, T: Message + 'static>(
    locs: impl Iterator<Item = &'a Loc>,
    comm: CommunicationModel,
    tag: Option<PredicateType>,
) -> Option<(T, usize)> {
    recv_val_with_tag(locs, comm, tag).map(|(val, ind)| (expect_msg(val), ind))
}

fn recv_val_with_tag<'a>(
    locs: impl Iterator<Item = &'a Loc>,
    comm: CommunicationModel,
    tag: Option<PredicateType>,
) -> Option<(Val, usize)> {
    let locs = locs.collect::<Vec<_>>();
    validate_locs(&locs);
    loop {
        switch();
        let locs = locs.clone();
        let tag = tag.clone();
        let (val, ind) = ExecutionState::with(|s| {
            let pos = s.next_pos();
            s.must.borrow_mut().handle_recv(
                RecvMsg::new(pos, RecvLoc::new(locs, tag), comm, None, true),
                false,
            )
        });
        if val.as_ref().is_some_and(Val::is_pending) {
            // The sender thread hasn't been executed far enough to reach the send label.
            // Block this thread and let the other threads run until the send is reached.
            ExecutionState::with(|s| {
                s.current_mut().stuck();
                s.prev_pos();
            });
        } else {
            return val.map(|v| (v, ind.unwrap()));
        }
    }
}

/// Returns a message from the queue.
pub fn recv_msg_block<T: Message + 'static>() -> T {
    let (loc, comm) = self_loc_comm();
    recv_msg_block_with_tag(iter::once(&loc), comm, None).0
}

/// Returns a message from the queue that matches `tag`
pub fn recv_tagged_msg_block<F, T>(f: F) -> T
where
    F: Fn(ThreadId, Option<u32>) -> bool + 'static + Send + Sync,
    T: Message + 'static,
{
    let (loc, comm) = self_loc_comm();
    recv_msg_block_with_tag(iter::once(&loc), comm, Some(PredicateType(Arc::new(f)))).0
}

/// Helper function for [`recv_msg_block`] and [`recv_tagged_msg_block`]
fn recv_msg_block_with_tag<'a, T: Message + 'static>(
    locs: impl Iterator<Item = &'a Loc>,
    comm: CommunicationModel,
    tag: Option<PredicateType>,
) -> (T, usize) {
    let (val, ind) = recv_val_block_with_tag(locs, comm, tag);
    (expect_msg(val), ind)
}

fn recv_val_block_with_tag<'a>(
    locs: impl Iterator<Item = &'a Loc>,
    comm: CommunicationModel,
    tag: Option<PredicateType>,
) -> (Val, usize) {
    let locs = locs.collect::<Vec<_>>();
    validate_locs(&locs);
    loop {
        switch();
        let locs = locs.clone();
        let (val, ind) = ExecutionState::with(|s| {
            let pos = s.next_pos();
            s.must.borrow_mut().handle_recv(
                RecvMsg::new(pos, RecvLoc::new(locs, tag.clone()), comm, None, false),
                true,
            )
        });
        match val {
            Some(box_msg) => {
                if box_msg.is_pending() {
                    // The joined thread has not finished executing yet,
                    // so the End label doesn't have the value returned by the thread.
                    // Block this thread and let the other thread finish.
                    ExecutionState::with(|s| s.current_mut().stuck());
                } else {
                    return (box_msg, ind.unwrap());
                }
            }
            _ => {}
        };

        ExecutionState::with(|s| s.prev_pos());
    }
}

/// Models a nondeterministic choice in the model
/// #[deprecated(since="0.2", note="please use `<bool>::nondet()` instead")]
pub fn nondet() -> bool {
    switch();
    ExecutionState::with(|s| {
        let pos = s.next_pos();
        s.must.borrow_mut().handle_ctoss(CToss::new(pos))
    })
}
#[deprecated(
    since = "0.2.0",
    note = "please use `nondet()` or `<bool>::nondet()` instead"
)]
pub fn coin_toss() -> bool {
    nondet()
}

use crate::monitor_types::{Monitor, MonitorResult};
use std::ops::{Range, RangeInclusive};
use std::sync::Mutex;

pub trait TypeNondet {
    fn nondet() -> Self;
}

impl TypeNondet for bool {
    fn nondet() -> Self {
        switch();
        ExecutionState::with(|s| {
            let pos = s.next_pos();
            s.must.borrow_mut().handle_ctoss(CToss::new(pos))
        })
    }
}

pub trait Nondet<T> {
    // By making the nondet function take a reference, this means that the
    // range does not get moved / consumed, so it can be used multiple times without
    // forcing the caller to clone it.
    // This seems appropriate since the member functions used within nondet(),
    // namely `start` and `end` can be called using a const reference; they don't
    // need a stronger mutable reference or a copy.
    fn nondet(&self) -> T;
}

impl Nondet<usize> for RangeInclusive<usize> {
    fn nondet(&self) -> usize {
        switch();
        ExecutionState::with(|s| {
            let pos = s.next_pos();
            if self.start() > self.end() {
                panic!("Range {:?} is not well-formed", self)
            }
            let mut r = RangeInclusive::new(*self.start(), *self.end());
            s.must.borrow_mut().handle_choice(Choice::new(pos, &mut r))
        })
    }
}

impl Nondet<usize> for Range<usize> {
    fn nondet(&self) -> usize {
        switch();
        ExecutionState::with(|s| {
            let pos = s.next_pos();
            if self.start >= self.end {
                panic!("Range {:?} is not well-formed", self)
            }
            let mut r = RangeInclusive::new(self.start, self.end - 1);
            s.must.borrow_mut().handle_choice(Choice::new(pos, &mut r))
        })
    }
}

/// Provides a sampler from random values
/// This requires that you are running TraceForge in statistical mode
#[doc(hidden)]
pub fn sample<
    T: Clone + std::fmt::Debug + Serialize + for<'a> Deserialize<'a>,
    D: Distribution<T>,
>(
    distr: D,
    max_samples: usize,
) -> T {
    ExecutionState::with(|s| {
        let pos = s.next_pos();
        let mut must = s.must.borrow_mut();
        must.handle_sample(pos, distr, max_samples)
    })
}

/// Blocks (stops) the exploration if `cond` is `false`.
///
/// The purpose of assume!(x) is to tell TraceForge that the current execution should
/// not be explored any more if x (any boolean condition) is false.
///
/// More importantly, the entire tree of executions which proceed from a false
/// assumption should not be explored any more either.
///
/// Note: Using tagged receives (`recv_tagged_msg_block`) can often be used for the
/// same purpose, with even more efficiency. `assume(false)` can stop the current
/// execution, but `recv_tagged_msg_block` can stop the unwanted execution from
/// ever being generated.
///
/// This is very useful if the creator of the model knows something about the model
/// that TraceForge does not know. For example, suppose that you have the following code:
///
/// ```ignore
/// let mut sum = 0;
/// for i in 0..5 {
///     let n: i32 = traceforge::recv_msg_block();
///     sum += n;
/// }
/// ```
///
/// If there are 5 messages to deliver, received from different senders, there
/// are 5! = 120 different orders in which the messages could be delivered, and
/// TraceForge will try all of them. But the order does not matter for purposes of computing a
/// sum. So you could write:
///
/// ```ignore
/// let mut sum = 0;
/// let mut prev = std::i32::MIN;
/// for i in 0..5 {
///     let n: i32 = recv_msg_block();
///     assume!(n >= prev);
///     prev = n;
///     sum += n;
/// }
/// ```
///
/// This new loop only explores executions where the values are increasing order.
/// If the values are distinct, this will mean that there is only one canonical execution
/// which will be explored after exiting the loop. It will require 120x fewer executions
/// by TraceForge in order to explore the remainder of the program.
#[macro_export]
macro_rules! assume {
    ($bool:expr) => {
        $crate::assume_impl($bool, Some((stringify!($bool), file!(), line!())));
    };
}

/// Blocks the exploration if `cond` is `false`.
#[deprecated(note = "Use assume!(x) instead to get more information.")]
pub fn assume(cond: bool) {
    assume_impl(cond, None)
}

// Used by the macro `assume!`. Not intended to be invoked directly.
#[doc(hidden)]
pub fn assume_impl(cond: bool, macro_info: Option<(&str, &str, u32)>) {
    switch();
    if !cond {
        match macro_info {
            Some((descr, file, line)) => {
                log::info!(
                    "This execution is ending because `assume!({})` is false at {}:{}",
                    descr,
                    file,
                    line
                );
            }
            None => {
                log::info!("This execution is ending because `assume(???)` is false.");
                log::warn!("Use macro `assume!(x)` instead to get better debug information.");
            }
        }
        ExecutionState::with(|s| {
            let pos = s.next_pos();
            s.must
                .borrow_mut()
                .handle_block(Block::new(pos, BlockType::Assume))
        });
        switch();
    }
}

/// TraceForge's wrapper for an assertion. It behaves similarly to the system's `assert!`
/// but allows the underlying model checker to continue exploration even if an assertion
/// violation has been found.
///
/// You can have both the system `assert!` and TraceForge's `assert` in a model. The system `assert!`
/// panics on failure, but TraceForge's assert can carry on with the search if the `keep_going_after_error`
/// flag is set in the configuration.
pub fn assert(cond: bool) {
    if !cond {
        ExecutionState::with(|s| {
            let pos = s.next_pos();

            let mut must = s.must.borrow_mut();
            if must.config().keep_going_after_error {
                let name = if let Some(task) = s.try_current() {
                    task.name()
                        .unwrap_or_else(|| format!("task-{:?}", task.id().0))
                } else {
                    "<unknown>".into()
                };
                // block the current execution but continue
                must.handle_block(Block::new(pos, BlockType::Assert));
                // the assertion violation is reported only if the execution graph is consistent
                // needed for semantics like Mailbox which generate executions under causal delivery and which need to be filtered to satisfy the stronger mailbox semantics
                if must.is_consistent() {
                    let message = persist_task_failure(name, Some(pos));
                    info!("Persisted failure {message}");
                }
            } else {
                // call system assert and panic
                // Add a block node to the graph
                must.handle_block(Block::new(pos, BlockType::Assert));
                // as above, we report the assertion violation only if the execution graph is consistent
                if must.is_consistent() {
                    info!("Error Detected!");
                    println!("{}", must.print_graph(None));
                    // The graph is completely generated, now build the linearization
                    must.store_replay_information(Some(pos));

                    // Report the failure
                    assert!(cond);
                }
            }
        });
    }
}

/// Spawns a new thread symmetric to `tid`
pub fn spawn_symmetric<F, T>(f: F, tid: crate::thread::ThreadId) -> crate::thread::JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Message + 'static,
{
    let jh = crate::thread::spawn_without_switch(f, None, false, None, Some(tid));
    switch();
    jh
}

// This function is public so that it can be invoked from within the expansion of the
// Monitor macro; it should not be directly invoked from customer models.
#[doc(hidden)]
pub fn invoke_on_stop(monitor: &mut dyn Monitor) -> MonitorResult {
    Must::invoke_on_stop(monitor)
}
