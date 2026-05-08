use crate::cons::Consistency;
use crate::event::Event;
use crate::exec_graph::ExecutionGraph;
use crate::exec_pool::ExecutionPool;
use crate::future::PollerMsg;
use crate::loc::{Loc, WakeMsg};
use crate::revisit::{Revisit, RevisitEnum};
use crate::runtime::failure::init_panic_hook;
use crate::runtime::task::TaskId;
use crate::telemetry::{Recorder, Telemetry};
use crate::vector_clock::VectorClock;
use crate::{event_label::*, ExecutionState, MonitorAcceptorFn, MonitorCreateFn};
use crate::{replay as REPLAY, Val};
use crate::{Config, ExplorationMode, SchedulePolicy, Stats};
use log::{debug, info, trace, warn};
use rand::distributions::Distribution;
use rand::{prelude::SliceRandom, Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;

use core::panic;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::msg::Message;
use crate::thread::{main_thread_id, ThreadId};

use crate::monitor_types::{EndCondition, ExecutionEnd, Monitor, MonitorResult};
use std::any::TypeId;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Write;

const EXECS: &str = "execs";
const BLOCKED: &str = "blocked";
const EXECS_EST: &str = "execs_est";

macro_rules! cast {
    ($target: expr, $pat: path) => {{
        if let $pat(a) = $target {
            a
        } else {
            std::io::stderr().flush().unwrap();
            panic!("mismatch variant when cast to {}", stringify!($pat));
        }
    }};
}

type RQueue = BTreeMap<usize, Vec<RevisitEnum>>;
type StateStack = Vec<MustState>;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct MustState {
    graph: ExecutionGraph,
    rqueue: RQueue,
}

impl MustState {
    fn new() -> Self {
        Self {
            graph: ExecutionGraph::new(),
            rqueue: RQueue::new(),
        }
    }
}

thread_local! {
    /// This thread local variable stores the Must that is being used by the current
    /// thread's exploration. At present this is only used by the panic handler.
    ///
    /// The rest of the code gets Must by either calling ExecutionState::with(|s| s.must)
    /// or by just passing an Rc<RefCell<Must>> up and down the call stack.
    /// However, those don't work with the panic handler.
    ///
    /// In the future, we probably should change the code more so that Must is just
    /// a thread local static RefCell<Option<Must>>, and it's never passed up and
    /// down the stack anywhere, and is not stored inside ExecutionState either.
    ///
    /// Notes:
    /// 1. All must exploration happens on a single OS thread, even though Must presents
    /// the illusion of multiple threads.
    /// 2. However, during unit testing, Rust runs all tests on different threads at
    /// the same time concurrently, which means that this cannot be static, and
    /// we need to strictly avoid any kind of storage which is global such as
    /// passing Must into the panic handler.
    static CURRENT_MUST: RefCell<Option<Rc<RefCell<Must>>>> = const { RefCell::new(None) };
}

/// Information about the monitor
pub(crate) struct MonitorInfo {
    /// The thread id of the monitor
    pub thread_id: ThreadId,
    /// Packages up the sender and receiver in a message whose type is right for the monitor.
    pub create_fn: MonitorCreateFn,
    /// Returns true if the monitor accepts this message.
    pub acceptor_fn: MonitorAcceptorFn,
    /// The monitor's struct.
    /// This uses an Arc<Mutex<_>> to hold the monitor because the monitor's data will be
    /// used both inside the monitor thread (to receive messages) and at the end of the
    /// execution (from the Must thread). Only one of these accesses can be happening at once
    /// so we could have just used unsafe to share the data, but using Arc<Mutex<_>> shows
    /// the compiler that we are not doing anything that's ultimately unsafe
    pub monitor_struct: Arc<Mutex<dyn Monitor>>,
}

type ExecutionGraphEnqueuePair = (Arc<Mutex<VecDeque<Option<ExecutionGraph>>>>, Arc<Condvar>);

// No getters so that the borrow checker does not get confused
pub(crate) struct Must {
    states: StateStack,
    current: MustState,
    replay_info: REPLAY::ReplayInformation,
    checker: Consistency,
    pub config: Config,
    monitors: BTreeMap<ThreadId, MonitorInfo>,
    rng: Pcg64Mcg,
    stop: bool,
    warn_limit: usize,
    pqueue: Option<ExecutionGraphEnqueuePair>,
    pub telemetry: Telemetry,
    published_values: BTreeMap<(ThreadId, TypeId), Val>,
    pub started_at: Instant,

    // Named nondeterministic choice support
    // Per-choice-name thread indexing: each choice name has independent thread indices
    // Frozen mapping used to ensure consistent thread indices across all executions.
    // Keys are origination_vecs (spawn lineage paths) which are stable across executions,
    // unlike ThreadIds which can change when scheduling decisions differ.
    pub(crate) frozen_thread_index_map: Option<HashMap<String, HashMap<Vec<u32>, usize>>>,
    // Current execution's mapping (built during first execution, then copied from frozen)
    // Map: choice_name -> (origination_vec -> thread_idx)
    pub(crate) thread_index_map: HashMap<String, HashMap<Vec<u32>, usize>>,
    // Next available index for each choice name
    pub(crate) next_thread_index: HashMap<String, usize>,
    // Per-execution counters: (choice_name, thread_idx) -> occurrence count
    pub(crate) choice_occurrence_counters: HashMap<(String, usize), usize>,
    // Cache for global named choices: once resolved, the same value is returned for all threads
    pub(crate) global_named_choices: HashMap<String, bool>,
    // Maximum number of events across all complete (non-blocked) execution graphs
    max_graph_events: usize,
}

impl Must {
    pub(crate) fn new(conf: Config, replay_mode: bool) -> Self {
        let seed = conf.seed;
        if conf.schedule_policy == SchedulePolicy::Arbitrary
            || conf.mode == ExplorationMode::Estimation
        {
            info!("Random schedule seed: {:?}", seed);
        }
        let telemetry = Telemetry::new(conf.keep_per_execution_coverage);
        let _ = telemetry.register_counter(&EXECS.to_owned());
        let _ = telemetry.register_counter(&BLOCKED.to_owned());
        let _ = telemetry.register_histogram(&EXECS_EST.to_owned());

        Self {
            states: Vec::new(),
            current: MustState::new(),
            replay_info: REPLAY::ReplayInformation::new(conf.clone(), replay_mode),
            checker: Consistency {},
            config: conf,
            monitors: BTreeMap::new(),
            rng: Pcg64Mcg::seed_from_u64(seed),
            stop: false,
            warn_limit: 1,
            pqueue: None,
            telemetry,
            published_values: BTreeMap::new(),
            started_at: Instant::now(),
            frozen_thread_index_map: None,
            thread_index_map: HashMap::new(),
            next_thread_index: HashMap::new(),
            choice_occurrence_counters: HashMap::new(),
            global_named_choices: HashMap::new(),
            max_graph_events: 0,
        }
    }

    /// Resets the Must instance for a new sample exploration.
    /// This avoids reallocating the entire Must struct between samples,
    /// reducing heap fragmentation and memory overhead.
    pub(crate) fn reset_for_sample(&mut self, seed: u64) {
        self.states.clear();
        self.current = MustState::new();
        self.monitors.clear();
        self.published_values.clear();
        self.stop = false;
        self.warn_limit = 1;
        self.config.seed = seed;
        self.rng = Pcg64Mcg::seed_from_u64(seed);
        self.replay_info = REPLAY::ReplayInformation::new(self.config.clone(), false);
        self.telemetry = Telemetry::default();
        let _ = self.telemetry.register_counter(&EXECS.to_owned());
        let _ = self.telemetry.register_counter(&BLOCKED.to_owned());
        let _ = self.telemetry.register_histogram(&EXECS_EST.to_owned());
        self.frozen_thread_index_map = None;
        self.thread_index_map.clear();
        self.next_thread_index.clear();
        self.choice_occurrence_counters.clear();
        self.global_named_choices.clear();

    }

    pub(crate) fn gen_bool(&mut self) -> bool {
        self.rng.gen_range(0..=1) == 0
    }
	 
    pub(crate) fn current() -> Option<Rc<RefCell<Must>>> {
        CURRENT_MUST.with(|current_must| current_must.borrow().clone())
    }

    pub(crate) fn set_current(must: Option<Rc<RefCell<Self>>>) {
        CURRENT_MUST.with(|current_must| {
            *current_must.borrow_mut() = must;
        });
    }

    pub(crate) fn begin_execution(must: &Rc<RefCell<Must>>) {
        let mut must = must.borrow_mut();
        must.current.graph.initialize_for_execution();
        must.telemetry.coverage.new_eid();

        // Reset per-execution state for named choices
        // Initialize frozen mapping if not yet created (first execution)
        if must.frozen_thread_index_map.is_none() {
            must.frozen_thread_index_map = Some(HashMap::new());
            debug!("Initialized empty frozen thread index mapping for incremental freezing");
        }

        // Restore from frozen mapping (which grows incrementally as threads are discovered)
        let frozen_map = must.frozen_thread_index_map.as_ref().unwrap().clone();
        must.thread_index_map = frozen_map.clone();
        // Restore next_thread_index for each choice name
        must.next_thread_index = frozen_map
            .iter()
            .map(|(name, map)| (name.clone(), map.len()))
            .collect();

        if !frozen_map.is_empty() {
            let total_mappings: usize = frozen_map.values().map(|m| m.len()).sum();
            debug!("Restored frozen thread index mapping for {} choice names with {} total thread mappings",
                frozen_map.len(), total_mappings);
        }

        must.choice_occurrence_counters.clear();
        must.global_named_choices.clear();

        // TODO: when must is borrowed, the panic handler cannot capture
        // a counterexample. run_metrics_before() invokes must model code
        // that might panic, and it would be nice to refactor the code so that
        // a lock on Must is not held when calling run_metrics_before.
        must.run_metrics_before();
    }

    pub(crate) fn publish<T: Message + 'static>(&mut self, thread_id: ThreadId, val: T) {
        self.published_values
            .insert((thread_id, TypeId::of::<T>()), Val::new(val));
    }

    pub(crate) fn invoke_on_stop(monitor: &mut dyn Monitor) -> MonitorResult {
        let published_values =
            ExecutionState::with(|s| s.must.borrow_mut().published_values.clone());
        let execution_end = ExecutionEnd {
            condition: EndCondition::MonitorTerminated,
            published_values,
            _unused_lifetime: std::marker::PhantomData,
        };
        monitor.on_stop(&execution_end)
    }

    pub(crate) fn run_metrics_before(&mut self) {
        let eid = self.telemetry.coverage.current_eid();
        for cb in &mut self
            .config
            .callbacks
            .lock()
            .expect("Could not lock callbacks")
            .iter_mut()
        {
            cb.before(eid);
        }
    }

    pub(crate) fn run_metrics_at_end(&mut self) {
        for cb in &mut self
            .config
            .callbacks
            .lock()
            .expect("Could not lock callbacks")
            .iter_mut()
        {
            cb.at_end_of_exploration();
        }
    }

    pub(crate) fn to_thread_id(&self, task_id: TaskId) -> ThreadId {
        self.current.graph.to_thread_id(task_id)
    }

    pub(crate) fn to_task_id(&self, tid: ThreadId) -> Option<TaskId> {
        self.current.graph.to_task_id(tid)
    }

    pub(crate) fn set_parallel_queues(&mut self, pq: ExecutionGraphEnqueuePair) {
        self.pqueue = Some(pq);
    }

    pub(crate) fn reset_execution_graph(&mut self, eg: ExecutionGraph) {
        self.current.rqueue.clear();
        self.states.clear();
        self.current.graph = eg;
    }

    /// Cheaply reset a Must instance for reuse by a new parallel task.
    /// Clears accumulated state (counters, states, monitors, telemetry)
    /// so stats start fresh for this task.
    pub(crate) fn reset_for_reuse(&mut self) {
        self.states.clear();
        self.current = MustState::new();
        self.monitors.clear();
        self.stop = false;
        self.published_values.clear();
        self.started_at = Instant::now();
        self.choice_occurrence_counters.clear();
        self.global_named_choices.clear();
        self.max_graph_events = 0;
        // Reset telemetry so stats() starts from zero for this task.
        self.telemetry = Telemetry::new(self.config.keep_per_execution_coverage);
        let _ = self.telemetry.register_counter(&EXECS.to_owned());
        let _ = self.telemetry.register_counter(&BLOCKED.to_owned());
        let _ = self.telemetry.register_histogram(&EXECS_EST.to_owned());
        // Note: frozen_thread_index_map, thread_index_map, next_thread_index,
        // config, rng are intentionally NOT reset — they are either set
        // explicitly by the caller (frozen map) or persist across tasks.
    }

    /// Drain only the saved states (not current). Returns them as work items.
    /// Current state remains in place, untouched.
    pub(crate) fn drain_saved_states(&mut self) -> Vec<(ExecutionGraph, RQueue)> {
        self.states
            .drain(..)
            .map(|state| (state.graph, state.rqueue))
            .collect()
    }

    /// Check if the current state's revisit queue is empty.
    pub(crate) fn current_rqueue_empty(&self) -> bool {
        self.current.rqueue.is_empty()
    }

    /// Load a state stack with partitioned queues (for parallel workers).
    /// Each state gets its graph and a partitioned subset of its revisits.
    pub(crate) fn load_state_stack(&mut self, mut stack: Vec<(ExecutionGraph, RQueue)>) {
        self.states.clear();

        if stack.is_empty() {
            return;
        }

        // Pop the last entry — it becomes current
        let (last_graph, last_rqueue) = stack.pop().unwrap();
        self.current.graph = last_graph;
        self.current.rqueue = last_rqueue;

        // Remaining entries become saved states (moved, not cloned)
        for (graph, rqueue) in stack {
            self.states.push(MustState { graph, rqueue });
        }
    }

    /// Add the replay information to a fresh instance of Must
    pub(crate) fn load_replay_information(&mut self, replay_info: REPLAY::ReplayInformation) {
        self.replay_info = replay_info;
        self.current = self.replay_info.extract_error_state();
        self.config = self.replay_info.config();
    }

    /// Extract the replay information from a failing execution
    pub(crate) fn store_replay_information(&mut self, pos: Option<Event>) {
        println!("Random schedule seed: {:?}.", self.config().seed);

        if !self.replay_info.error_found() {
            let sorted_error_graph = self.current.graph.top_sort(pos);

            let replay_info = REPLAY::ReplayInformation::create(
                sorted_error_graph,
                self.current.clone(),
                self.config.clone(),
            );

            let error_trace_file = self.config.error_trace_file.as_ref();
            match error_trace_file {
                None => {
                    warn!("No counterexample trace will because Must is not configured with a filename. Use `Config::with_error_trace()`");
                }
                Some(f) => {
                    let mut file = File::create(f).unwrap();
                    match serde_json::to_string_pretty(&replay_info) {
                        Ok(replay_str) => {
                            writeln!(&mut file, "{}", replay_str).unwrap();
                        }
                        Err(err) => {
                            println!("Can't serialize graph to json: {}", err);
                        }
                    };
                    self.replay_info = replay_info;
                }
            }
        }
    }

    /// If the replayed event, i.e., `label` matches the `current_event`, it means
    /// that the `current_event` from the linearization has been replayed.
    /// So, now it's time to replay the next event from the linearization.
    fn try_consume(&mut self, label: &LabelEnum) {
        if self.replay_info.replay_mode() {
            if let Some(current_event) = self.replay_info.current_event() {
                if label.pos() == current_event.pos() {
                    // Playing the current event.
                    info!("|| Consuming {}", label);
                    self.replay_info.reset_current_event();
                } else {
                    std::io::stderr().flush().unwrap();
                    panic!(
                        "Replay failure: Executing {} instead of the counterexample's {}",
                        label.pos(),
                        current_event.pos()
                    );
                }
            }
        }
    }

    /// This function tries to consume the current event (if possible)
    /// and updates the graph with any field that was lost during (de)serialization.
    fn process_event(&mut self, label: LabelEnum) {
        self.current.graph.unreplayed_events.remove(&label.pos());
        self.try_consume(&label);
        self.recover_lost_data(label);
    }

    pub(crate) fn handle_register_mon(&mut self, monitor_info: MonitorInfo) {
        self.monitors.insert(monitor_info.thread_id, monitor_info);
    }

    /// Returns the value read, if any, along with the rlab's receiving channel index, if any.
    /// Note: It can be that there is a "value" but no index (Val::default, during replay).
    pub(crate) fn handle_recv(
        &mut self,
        rlab: RecvMsg,
        blocking: bool,
    ) -> (Option<Val>, Option<usize>) {
        if self.is_replay(rlab.pos()) {
            info!("| Replay Mode for receive {}", rlab);
            // Try to see if the `current_event` matches `rlab`
            let pos = rlab.pos();
            let lab = LabelEnum::RecvMsg(rlab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);

            // If the send that R reads from has a different reader R', assert that
            // R' is in a cancelled async receive, then fix up the reader.
            let g = &mut self.current.graph;
            let rlab = g.recv_label(pos).unwrap();
            if let Some(send_pos) = rlab.rf() {
                let slab = g.send_label(send_pos).unwrap();
                if let Some(reader) = slab.reader() {
                    if reader != pos {
                        // Verify R' is in a cancelled async receive (same check as cons.rs),
                        // OR that the mismatch is due to monitor message tracking.
                        // not a PollerMsg/WakeMsg, and a later event on R's thread reads
                        // from a PollerMsg::Cancel.
                        assert!(
                            slab.is_monitored_from(&pos.thread)
                            || slab.is_monitored_from(&reader.thread)
                            || (slab.val.as_any_ref().downcast_ref::<PollerMsg>().is_none()
                            && slab.val.as_any_ref().downcast_ref::<WakeMsg>().is_none()
                            && g.get_thr(&reader.thread).labels[(reader.index as usize + 1)..]
                                .iter()
                                .any(|lab| {
                                    if let LabelEnum::RecvMsg(recv) = lab {
                                        recv.rf().is_some_and(|rf| {
                                            if let LabelEnum::SendMsg(send) = g.label(rf) {
                                                send.val.as_any_ref().downcast_ref::<PollerMsg>()
                                                    .is_some_and(|msg| matches!(msg, PollerMsg::Cancel))
                                            } else {
                                                false
                                            }
                                        })
                                    } else {
                                        false
                                    }
                                })),
                            "Replay: send {} has reader {} but replaying receive {} and reader is not in a cancelled async receive",
                            send_pos, reader, pos
                        );
                        let slab = g.send_label_mut(send_pos).unwrap();
                        if slab.is_monitored_from(&pos.thread) {
                            // Monitor is replaying its receive; add as monitor reader
                            slab.add_monitor_reader(pos);
                        } else if slab.is_monitored_from(&reader.thread) {
                            // Existing reader is a monitor; move it to monitor readers
                            slab.add_monitor_reader(reader);
                            slab.set_reader(Some(pos));
                        } else {
                            slab.push_cancelled_recv_reader(reader);
                            slab.set_reader(Some(pos));
                        }
                    }
                }
            }

            let g = &self.current.graph;
            // Fetch it again, it might have been updated
            let rlab = g.recv_label(pos).unwrap();
            return (g.val_copy(pos), g.get_receiving_index(rlab));
        }
        info!("| Handle Mode for {}", rlab);

        let pos = self.add_to_graph(LabelEnum::RecvMsg(rlab));
        let val = self.visit_rfs(pos, blocking);
        self.current.graph.register_recv(&pos);
        let g = &self.current.graph;
        (
            val,
            g.recv_label(pos).and_then(|r| g.get_receiving_index(r)),
        )
    }

    // Returns the events that *might* be stuck waiting for the send,
    // in case this is a replay.
    pub(crate) fn handle_send(&mut self, slab: SendMsg) -> Vec<Event> {
        let spos = slab.pos();
        let mut stuck: Vec<Event> = Vec::new();
        if self.is_replay(spos) {
            info!("| Replay Mode for {} with reader {:?}", slab, slab.reader());
            let lab = LabelEnum::SendMsg(slab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);

            // Wake up the tasks that (want to) read from this send
            let LabelEnum::SendMsg(slab) = self.current.graph.label(spos) else {
                unreachable!()
            };
            // The reader might be stuck waiting us, inform caller
            // to handle appropriately (has access to ExecutionState).
            if let Some(r) = slab.reader() { stuck.push(r) }
            // Similar for monitor readers
            slab.monitor_readers().iter().for_each(|&r| stuck.push(r));
            return stuck;
        }
        info!("| Handle Mode for {}", slab);

        trace!("[must.rs] Handling send at position {}", slab.pos());

        let pos = self.add_to_graph(LabelEnum::SendMsg(slab));
        trace!("[must.rs] Adding the system send {}", pos);

        // Consider dropping the send message
        // TODO: Estimation mode

        // TODO: Currently, we consider dropping the message at the time the send appears.
        // If there's no one to receive, we might be doing unnecessary work.
        // For models apart from Mailbox/TotalOrder, we could instead lazily
        // consider message drops implicitly at the time a receive is added:
        // receiving from a later send is equivalent to dropping the send.
        // For models apart from mailbox (?), checking consistency remains
        // polynomial but might require some caching to do it efficiently
        // (which sends have implicitly been dropped).
        let slab = self.current.graph.send_label(pos).unwrap();
        if slab.is_lossy() && self.dropped_messages() < self.config.lossy_budget {
            push_worklist(
                &mut self.current.rqueue,
                slab.stamp(),
                RevisitEnum::new_forward(pos, Event::new_init()),
            )
        }

        self.calc_revisits(pos);
        self.current.graph.register_send(&spos);

        // stuck is only used during replay
        assert!(stuck.is_empty());
        stuck
    }

    /// Returns the next thread id to use in thread creation.
    pub(crate) fn next_thread_id(&self, pos: &Event) -> ThreadId {
        let parent_tclab: TCreate = self.current.graph.get_thread_tclab(pos.thread);
        let mut origination_vec = parent_tclab.origination_vec();
        origination_vec.push(pos.index);
        self.current.graph.tid_for_spawn(pos, &origination_vec)
    }

    /// Returns the origination_vec for the given thread.
    pub(crate) fn thread_origination_vec(&self, tid: ThreadId) -> Vec<u32> {
        self.current.graph.get_thread_tclab(tid).origination_vec()
    }

    /// Returns the filtered_origination_vec for the given thread.
    pub(crate) fn thread_filtered_origination_vec_from_tid(&self, tid: ThreadId) -> Vec<u32> {
        self.current.graph.get_thread_tclab(tid).filtered_origination_vec()
    }

    /// Counts the number of TCreate events in the given thread up to and including
    /// the specified event index, excluding those whose names contain the filter pattern.
    fn count_filtered_tcreate_events(&self, thread: ThreadId, up_to_index: u32, filter_pattern: &str) -> u32 {
        let mut count = 0;
        let thread_size = self.current.graph.thread_size(thread) as u32;

        // Iterate only up to the minimum of up_to_index and the actual thread size - 1
        // (since we're currently adding a new event at up_to_index, it may not exist yet)
        let max_idx = up_to_index.min(thread_size.saturating_sub(1));

        for idx in 0..=max_idx {
            let event = Event::new(thread, idx);
            if let LabelEnum::TCreate(tclab) = self.current.graph.label(event) {
                // Check if this thread creation should be counted
                let should_count = if let Some(ref name) = tclab.name() {
                    !name.contains(filter_pattern)
                } else {
                    // Unnamed threads are counted
                    true
                };

                if should_count {
                    count += 1;
                }
            }
        }

        count
    }

    pub(crate) fn handle_tcreate(
        &mut self,
        tid: ThreadId,
        cid: TaskId,
        sym_cid: Option<ThreadId>,
        pos: Event,
        name: Option<String>,
        is_daemon: bool,
    ) {
        let parent_tclab: TCreate = self.current.graph.get_thread_tclab(pos.thread);
        let mut origination_vec = parent_tclab.origination_vec();
        origination_vec.push(pos.index);

        // Compute filtered_origination_vec
        let mut filtered_origination_vec = parent_tclab.filtered_origination_vec();
        let filtered_count = self.count_filtered_tcreate_events(
            pos.thread,
            pos.index,
            crate::FILTERED_THREAD_NAME_PATTERN
        );
        filtered_origination_vec.push(filtered_count);

        let tclab = TCreate::new(pos, tid, name, is_daemon, sym_cid, origination_vec, filtered_origination_vec);

        if self.is_replay(pos) {
            info!("| Replay Mode for {}", tclab);
            // Try to see if the `current_event` matches `tclab`
            self.current.graph.set_task_for_replay(tid, cid);
            let lab = LabelEnum::TCreate(tclab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            return;
        }
        info!("| Handle Mode for {}", tclab);

        let spawn_pos = self.add_to_graph(LabelEnum::TCreate(tclab.clone()));
        assert_eq!(spawn_pos, pos);

        self.current.graph.add_new_thread(tclab, cid);
        let blab = Begin::new(Event::new(tid, 0), Some(spawn_pos), sym_cid);

        self.add_to_graph(LabelEnum::Begin(blab));
    }

    pub(crate) fn handle_tjoin(&mut self, tjlab: TJoin) -> Option<Val> {
        if self.is_replay(tjlab.pos()) {
            info!("| Replay Mode for {}", tjlab);
            // Try to see if the `current_event` matches `tjlab`
            let lab = LabelEnum::TJoin(tjlab.clone());
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            return Some(
                cast!(
                    self.current.graph.thread_last(tjlab.cid()).unwrap(),
                    LabelEnum::End
                )
                .result()
                .clone(),
            );
        }
        info!("| Handle Mode for {}", tjlab);

        if self.current.graph.is_thread_complete(tjlab.cid()) {
            let cid = tjlab.cid();
            self.add_to_graph(LabelEnum::TJoin(tjlab));
            Some(
                cast!(self.current.graph.thread_last(cid).unwrap(), LabelEnum::End)
                    .result()
                    .clone(),
            )
        } else {
            self.add_to_graph(LabelEnum::Block(Block::new(
                tjlab.pos(),
                BlockType::Join(tjlab.cid()),
            )));
            None
        }
    }

    pub(crate) fn handle_tend(&mut self, elab: End) {
        if self.is_replay(elab.pos()) {
            info!("| Replay Mode for {}", elab);
            let lab = LabelEnum::End(elab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            return;
        }
        info!("| Handle Mode for {}", elab);
        self.add_to_graph(LabelEnum::End(elab));
    }

    pub(crate) fn handle_unique(&mut self, nclab: Unique) -> Loc {
        let chan = nclab.get_loc();
        if self.is_replay(nclab.pos()) {
            info!("| Replay Mode for {}", nclab);
            let lab = LabelEnum::Unique(nclab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            return chan;
        }
        info!("| Handle Mode for {}", nclab);
        self.add_to_graph(LabelEnum::Unique(nclab));
        chan
    }

    pub(crate) fn handle_ctoss(&mut self, ctlab: CToss) -> bool {
        if self.is_replay(ctlab.pos()) {
            info!("| Replay Mode for {}", ctlab);
            // Try to see if the `current_event` matches `ctlab`
            let lab = LabelEnum::CToss(ctlab.clone());
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            if let LabelEnum::CToss(tclab) = self.current.graph.label(ctlab.pos()) {
                return tclab.result();
            }
            std::io::stderr().flush().unwrap();
            panic!();
        }
        info!("| Handle Mode for {}", ctlab);
        let maximal = ctlab.maximal();

        let pos = self.add_to_graph(LabelEnum::CToss(ctlab));
        let stamp = self.current.graph.label(pos).stamp();

        if self.config.mode == ExplorationMode::Estimation {
            return self.pick_ctoss(pos);
        }

        push_worklist(
            &mut self.current.rqueue,
            stamp,
            RevisitEnum::new_forward(pos, Event::new_init()),
        );
        maximal
    }

    /// Handle a CToss with a predetermined value. Similar to handle_ctoss but does not add revisits.
    pub(crate) fn handle_ctoss_predetermined(&mut self, mut ctlab: CToss, value: bool) -> bool {
        if self.is_replay(ctlab.pos()) {
            info!(
                "| Replay Mode for {} with predetermined value {}",
                ctlab, value
            );
            // In replay mode, validate the event exists and process it
            ctlab.set_result(value);
            ctlab.set_predetermined();
            let lab = LabelEnum::CToss(ctlab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            // Return the predetermined value (ignoring what was in the graph)
            return value;
        }

        info!(
            "| Handle Mode for {} with predetermined value {}",
            ctlab, value
        );

        ctlab.set_result(value);
        ctlab.set_predetermined();
        self.add_to_graph(LabelEnum::CToss(ctlab));
        // Note: We don't add a revisit here because the value is predetermined
        value
    }    

    pub(crate) fn handle_choice(&mut self, chlab: Choice) -> usize {
        let result = chlab.result();
        let end = *chlab.range().end();

        if self.is_replay(chlab.pos()) {
            info!("| Replay Mode for {}", chlab);
            // Try to see if the `current_event` matches `chlab`
            let lab = LabelEnum::Choice(chlab.clone());
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            if let LabelEnum::Choice(tclab) = self.current.graph.label(chlab.pos()) {
                return tclab.result();
            }
            std::io::stderr().flush().unwrap();
            panic!();
        }
        info!("| Handle Mode for {}", chlab);

        let pos = self.add_to_graph(LabelEnum::Choice(chlab));
        let stamp = self.current.graph.label(pos).stamp();

        if self.config.mode == ExplorationMode::Estimation {
            return self.pick_choice(pos);
        }
        if result < end {
            // a revisit is needed only if the range has further elements
            push_worklist(
                &mut self.current.rqueue,
                stamp,
                RevisitEnum::new_forward(pos, Event::new_init()),
            );
        }
        result
    }

    pub(crate) fn handle_block(&mut self, blab: Block) {
        if self.is_replay(blab.pos()) {
            info!("| Replay Mode for {}", blab);
            let lab = LabelEnum::Block(blab);
            self.current.graph.validate_replay_event(&lab);
            self.process_event(lab);
            return;
        }
        self.add_to_graph(LabelEnum::Block(blab));
    }

    pub(crate) fn handle_sample<
        T: Clone + std::fmt::Debug + Serialize + for<'a> Deserialize<'a>,
        D: Distribution<T>,
    >(
        &mut self,
        pos: Event,
        distr: D,
        max_samples: usize,
    ) -> T {
        if self.is_replay(pos) {
            info!("| Replay mode for sample");
            let l = self.current.graph.label(pos);
            match l {
                LabelEnum::Sample(s) => {
                    let v = s.current().clone();
                    self.try_consume(&LabelEnum::Sample(s.clone())); // consume the next element in the trace being replayed
                    return serde_json::from_value(v).unwrap();
                }
                _ => panic!(),
            }
        }

        assert!(max_samples > 0);

        let mut it = self.rng.clone().sample_iter(distr);
        let first = it.next().unwrap();
        let rest = if max_samples == 1 {
            vec![]
        } else {
            it.take(max_samples - 2)
                .map(|val| serde_json::to_value(val).unwrap())
                .collect::<Vec<serde_json::Value>>()
        };
        let l = LabelEnum::Sample(Sample::new(
            pos,
            serde_json::to_value(first.clone()).unwrap(),
            rest,
        ));

        info!("| Handle Mode for {}", l);

        let pos = self.add_to_graph(l);

        if max_samples > 1 {
            let stamp = self.current.graph.label(pos).stamp();
            push_worklist(
                &mut self.current.rqueue,
                stamp,
                RevisitEnum::new_forward(pos, Event::new_init()),
            );
        }
        first
    }

    // this checks if the current graph is consistent
    // trivially true unless the semantics is Mailbox
    pub(crate) fn is_consistent(&self) -> bool {
        self.checker.is_consistent(&self.current.graph)
    }

    pub(crate) fn dropped_messages(&self) -> usize {
        self.current.graph.dropped_sends()
    }

    pub(crate) fn next_task(
        &mut self,
        runnable: &[(TaskId, usize)],
        _current: Option<TaskId>,
    ) -> Option<TaskId> {
        if self.is_stopped() {
            return None;
        }

        // If in replay mode, use the linearization to obtain the next thread
        // that must be executed
        if self.replay_info.replay_mode() {
            return self.replay_info.next_task().map(|tid| {
                self.to_task_id(tid)
                    .expect("task id not found in the execution graph!")
            });
        }

        let next = match self.config.schedule_policy {
            SchedulePolicy::LTR => runnable
                .iter()
                .find(|(t, i)| self.is_thread_runnable(t, i))
                .map(|(t, _)| t.to_owned()),
            SchedulePolicy::Arbitrary => runnable
                .choose_multiple(&mut self.rng, runnable.len())
                .find(|(t, i)| self.is_thread_runnable(t, i))
                .map(|(t, _)| t.to_owned()),
        };
        if next.is_some() {
            next
        } else {
            self.unblock_ready(runnable)
        }
    }

    fn is_thread_runnable(&self, t: &TaskId, i: &usize) -> bool {
        let thread_id = self.to_thread_id(*t);
        let g = &self.current.graph;

        // runnable when:
        match g.thread_last(thread_id).unwrap() {
            // Either the last event is Block and
            LabelEnum::Block(blab) => match blab.btype() {
                // it's an internal blocking and the instruction points
                // at least *2* instructions before it (see event_label::Block)
                BlockType::Join(_) | BlockType::Value(_) => (*i as u32) < blab.pos().index - 1,
                // it's a user blocking and the instruction points before it
                BlockType::Assume | BlockType::Assert => (*i as u32) < blab.pos().index,
            },
            // or the last event is not Block
            _ => true,
        }
    }

    fn unblock_ready(&mut self, runnable: &[(TaskId, usize)]) -> Option<TaskId> {
        let blocked = runnable
            .iter()
            .filter(|(t, _)| {
                let t = self.to_thread_id(*t);
                self.is_waiting_on_written(t) || self.is_waiting_on_finished(t)
            })
            .collect::<Vec<_>>();

        blocked
            .iter()
            .for_each(|task| self.current.graph.remove_last(self.to_thread_id(task.0)));

        blocked.first().map(|(t, _)| t.to_owned())
    }

    fn is_waiting_on_written(&self, t: ThreadId) -> bool {
        let g = &self.current.graph;
        if let LabelEnum::Block(blab) = g.thread_last(t).unwrap() {
            if let BlockType::Value(loc) = blab.btype() {
                g.matching_stores(loc).any(|send| {
                    // We need to consider two cases:
                    // . Monitor reading from the send:
                    // . . We are monitoring it and we haven't read it already
                    send.can_be_monitor_read(&blab.pos()) ||
                    // . Plain read from the send:
                    // . . It is unread and the location *really* matches (not via monitoring)
                        (send.can_be_read_from(loc) &&
                            // disregard cancelled sends
                            !send.is_cancelled_wrt(blab.as_event_label()))
                })
            } else {
                false
            }
        } else {
            false
        }
    }

    fn is_waiting_on_finished(&self, t: ThreadId) -> bool {
        if let LabelEnum::Block(blab) = self.current.graph.thread_last(t).unwrap() {
            match blab.btype() {
                BlockType::Join(jlab) => self.current.graph.finished_threads.contains(jlab),
                _ => false,
            }
        } else {
            false
        }
    }

    fn block_exec(&mut self, bt: BlockType) {
        self.current.graph.thread_ids().iter().for_each(|&t| {
            self.add_to_graph(LabelEnum::Block(Block::new(
                self.current.graph.thread_last(t).unwrap().pos().next(),
                bt.clone(),
            )));
        });
    }

    fn stop(&mut self) {
        self.stop = true;
    }

    fn unstop(&mut self) {
        self.stop = false;
    }

    fn is_stopped(&self) -> bool {
        self.stop
    }

    /// Check if the execution is blocked. Return None if it's not blocked, or Some(Block)
    /// to tell why it is blocked.
    fn check_blocked(&mut self) -> Option<BlockType> {
        self.current.graph.check_blocked()    
    }

    /// `complete_execution` is invoked when a particular single execution has finished.
    /// `complete_execution` returns false if there is another execution to do, or
    /// true if there is nothing more to explore.
    ///
    /// It takes a Rc<RefCell<Must>>, rather than &mut self, because it needs
    /// the ability to call into Must model code (the monitor on_stop) while
    /// not holding a reference to entire Must object.
    pub(crate) fn complete_execution(must: &Rc<RefCell<Must>>) -> bool {
        let maybe_block = must.borrow_mut().check_blocked();
        let exceeded_max_executions = must.borrow_mut().record_ending_telemetry(&maybe_block);

        let condition = match maybe_block {
            None => EndCondition::AllThreadsCompleted,
            Some(block) => match block {
                BlockType::Assume | BlockType::Assert => EndCondition::FailedAssumption,
                BlockType::Value(_) | BlockType::Join(_) => EndCondition::Deadlock,
            },
        };

        Must::call_on_stop_on_monitors(must, &condition);
        must.borrow_mut().published_values.clear();
        must.borrow_mut().call_telemetry_after(&condition);

        if exceeded_max_executions {
            return true; // no more executions.
        }

        must.borrow_mut().unstop();
        !must.borrow_mut().try_revisit()
    }

    fn record_ending_telemetry(&mut self, maybe_block: &Option<BlockType>) -> bool {
        // Debug: print events that were not replayed during this execution.
        let unreplayed = &self.current.graph.unreplayed_events;
        if !unreplayed.is_empty() {
            let mut sorted: Vec<_> = unreplayed.iter().collect();
            sorted.sort();
            debug!("[DEBUG] Unreplayed events ({}):", sorted.len());
            for ev in &sorted {
                let label = self.current.graph.label(**ev);
                debug!("  {} -> {}", ev, label);
            }
        } else {
            debug!("[DEBUG] All events were replayed.");
        }
        let elapsed = Instant::now() - self.started_at;
        if maybe_block.is_some() {
            if self.is_consistent() {
                self.telemetry.counter(BLOCKED.to_owned()); // increment BLOCKED
                let event_count: usize = self.current.graph.threads.iter().map(|t| t.labels.len()).sum();
                if event_count > self.max_graph_events {
                    self.max_graph_events = event_count;
                }
                if self.config.verbose >= 2 {
                    println!("One more blocked execution");
                    println!("{}", self.print_graph(None));
                    println!("Finished printing graph");
                }
            }
        } else if self.is_consistent() {
            self.telemetry.counter(EXECS.to_owned()); // increment EXECS
            let event_count: usize = self.current.graph.threads.iter().map(|t| t.labels.len()).sum();
            if event_count > self.max_graph_events {
                self.max_graph_events = event_count;
            }
            self.print_turmoil_trace();
            if self.config.verbose >= 1 {
                println!("One more complete execution");
                println!("{}", self.print_graph(None));
            }
        }

        let num_execs = self.telemetry.read_counter(EXECS.to_owned()).unwrap_or(0);
        let num_blocked = self.telemetry.read_counter(BLOCKED.to_owned()).unwrap_or(0);
        let num_total = num_execs + num_blocked;
        let speed: String = if elapsed.as_secs() < 5 {
            "".to_string()
        } else {
            format!(" ({:.2}/sec)", num_total as f64 / elapsed.as_secs() as f64)
        };
        let progress_desc = format!(
            "Executions attempted so far: {} total {} finished normally {} blocked{}.",
            num_total, num_execs, num_blocked, speed
        );

        if self.config.progress_report > 0 {
            if num_total.is_multiple_of(self.config.progress_report as u64) {
                // Although it might be nice to use \r (carriage return) here to
                // repeatedly rewrite the same line with new progress reports, this
                // will eat up the last log line, and if the program is printing anything
                // else at all (very likely) then the goal of rewriting the same
                // line is defeated anyway.
                println!("{}", progress_desc);
                let _ = std::io::stdout().flush();
                eprintln!("{}", progress_desc);
                let _ = std::io::stderr().flush();
            }
        } else {
            // Implement P-style progress report, which reports
            // after 1, 2, 3, .... 10, 20, 30, ... 100, 200, 300, etc.
            if Self::should_report(num_total) {
                println!("{}", progress_desc);
            }
        }

        if let Some(n) = self.config.max_iterations {
            if n <= num_total {
                println!("Stopping exploration because max_iterations was reached.");
                return true; // done
            }
        }

        false // not done
    }

    pub(crate) fn should_report(n: u64) -> bool {
        if n == 0 {
            return false;
        }
        // Cap at every 1M once we reach that scale
        if n >= 1_000_000 {
            return n.is_multiple_of(1_000_000);
        }
        // Below that, use P-style: report at 1,2,..,9, 10,20,..,90, 100,200,..,900, etc.
        let mut p = n;
        while p.is_multiple_of(10) {
            p /= 10;
        }
        p < 10
    }

    /// All of the monitors on_stop functions and return an error if there is one.
    fn call_on_stop_on_monitors(must: &Rc<RefCell<Must>>, condition: &EndCondition) {
        // Allow panics in Monitor::on_stop to be caught.
        let _guard = init_panic_hook();

        if condition == &EndCondition::FailedAssumption {
            // Don't execute the monitor's on_stop since an assumption failed.
            return;
        }

        // Extract all of the monitors from the must.monitor's BTree.
        let mut monitors: Vec<MonitorInfo> = vec![];
        let mut mustp = must.borrow_mut();
        while let Some((_, monitor_info)) = mustp.monitors.pop_first() {
            if !mustp
                .current
                .graph
                .finished_threads
                .contains(&monitor_info.thread_id)
            {
                monitors.push(monitor_info);
            }
        }
        drop(mustp);

        let published_values = must.borrow().published_values.clone();
        let execution_end = ExecutionEnd {
            condition: condition.clone(),
            published_values,
            _unused_lifetime: PhantomData,
        };

        // Run the on_stop function for any monitors that did not already get terminated.
        // Note that we are not holding the lock on Must because we extracted the
        // monitors earlier.
        for monitor_info in monitors {
            let mut monitor = monitor_info.monitor_struct.lock().unwrap();
            let res = (*monitor).on_stop(&execution_end);
            if let Err(msg) = res {
                // Store the replay information first.
                must.borrow_mut().store_replay_information(None);
                println!("{}", must.borrow_mut().print_graph(None));
                std::io::stderr().flush().unwrap();
                panic!(
                    "\u{1b}[1;31mA monitor returned the message: {}\u{1b}[0m",
                    msg
                );
            }
        }
    }

    fn call_telemetry_after(&mut self, condition: &EndCondition) {
        // run all registered on-stop handlers with end condition and coverage information
        // This is not ideal that we are locking Must while calling them; we can't
        // generate a counterexample if they panic. OTOH, the callbacks should not.
        // A monitor provides a general solution for generating a counterexample at the end of
        // an execution.
        for cb in &mut self
            .config
            .callbacks
            .lock()
            .expect("Could not lock callbacks")
            .iter_mut()
        {
            cb.after(
                self.telemetry.coverage.current_eid(),
                condition,
                self.telemetry.coverage.export_current().into(),
            );
        }

        // Clean up per-execution coverage data after observers have been notified
	    self.telemetry.coverage.cleanup_current_execution();
    }

    fn visit_rfs(&mut self, pos: Event, blocking: bool) -> Option<Val> {
        let mut rfs = self.checker.rfs(
            &self.current.graph,
            self.current.graph.recv_label(pos).unwrap(),
            self.is_monitor(&pos),
        );

        self.filter_symmetric_rfs(&mut rfs, pos);

        // At this point, we have handled all the cases for nonblocking receive
        // so we know blocking == true
        if !blocking {
            if !rfs.is_empty() {
                if self.config.mode == ExplorationMode::Estimation {
                    self.telemetry
                        .histogram(EXECS_EST.to_owned(), (rfs.len() + 1) as f64);

                    let idx = self.rng.gen_range(0..=rfs.len());

                    info!("| Choosing {} out of {}", idx, rfs.len());

                    if idx < rfs.len() {
                        self.current.graph.change_rf(pos, Some(rfs[idx]));
                    } else {
                        self.current.graph.change_rf(pos, None);
                    }
                    return self.current.graph.val_copy(pos);
                } else {
                    rfs.iter().for_each(|&rf| {
                        push_worklist(
                            &mut self.current.rqueue,
                            self.current.graph.label(pos).stamp(),
                            RevisitEnum::new_forward(pos, rf),
                        );
                    });
                }
            }
            self.current.graph.change_rf(pos, None);
            return self.current.graph.val_copy(pos);
        }

        if !rfs.is_empty() {
            if self.config.mode == ExplorationMode::Estimation {
                self.telemetry
                    .histogram(EXECS_EST.to_owned(), rfs.len() as f64);

                let idx = self.rng.gen_range(0..=(rfs.len() - 1));

                info!("| Choosing {} out of {}", idx, rfs.len());

                self.current.graph.change_rf(pos, Some(rfs[idx]));
            } else {
                debug!("Forward revisits at {}: {:?}", pos, rfs);
                self.current.graph.change_rf(pos, Some(rfs[0]));
                rfs.iter().skip(1).for_each(|&rf| {
                    push_worklist(
                        &mut self.current.rqueue,
                        self.current.graph.label(pos).stamp(),
                        RevisitEnum::new_forward(pos, rf),
                    );
                });
            }
            self.current.graph.val_copy(pos)
        } else {
            // Overwrites RecvMsg
            self.add_to_graph(LabelEnum::Block(Block::new(
                pos,
                BlockType::Value(
                    self.current
                        .graph
                        .recv_label(pos)
                        .unwrap()
                        .recv_loc()
                        .clone(),
                ),
            )));
            None
        }
    }

    fn is_maximal_extension(&self, rev: &Revisit) -> bool {
        let g = &self.current.graph;
        let rlab = g.recv_label(rev.pos).unwrap();
        let slab = g.send_label(rev.rev).unwrap();
        let porf = slab.porf();
        let stamp = rlab.stamp();
        for thread in g.threads.iter() {
            // Binary seach to find the first event that would be deleted
            // (the predicate is monotonic over po-ordered events)
            let i = thread
                .labels
                .partition_point(|lab| lab.stamp() <= stamp || porf.contains(lab.pos()));
            // Starting from there, see if there's any non-maximal
            // Note: slice[slice.len()..] is indeed valid and produces an empty slice
            if thread.labels[i..]
                .iter()
                .any(|lab| !self.is_maximal(lab, rev))
            {
                return false;
            }
        }
        true
    }

    // computing the set of backward revisits for the send at position "pos"
    fn calc_revisits(&mut self, pos: Event) {
        // pos = (thread,index)
        let slab = self.current.graph.send_label(pos).unwrap(); // the send label at the input position
        let stamp = slab.stamp(); // the position in the interleaving order
        if self.config.symmetry {
            let flab = self.current.graph.thread_first(slab.pos().thread).unwrap();
            if flab.sym_id().is_some() && self.is_prefix_symmetric(flab.sym_id(), pos) {
                return;
            }
        }

        let g = &self.current.graph;
        let send_porf = slab.porf();

        // Take the matching receives, in reverse stamp order
        let revs = g
            .rev_matching_recvs(slab)
            // Filter out the ones that are porf-before the send
            .filter(|&rlab| !send_porf.contains(rlab.pos()))
            // Take them while they pass the maximality check,
            // stopping at the first receive that fails:
            // it cannot be removed and thus any deeper (stamp-earlier) revisit is futile.
            .take_while(|recv| self.is_maximal_recv(recv, &Revisit::new(recv.pos(), pos)))
            // Finally, filter out the receives that cannot consistently read from the send.
            .filter(|rlab| {
                self.checker
                    .is_revisit_consistent(g, rlab, slab, self.is_monitor(&rlab.pos()))
            })
            // And again, take while the revisit is maximal (deeper revisits are futile if this fails)
            .take_while(|&rlab| self.is_maximal_extension(&Revisit::new(rlab.pos(), pos)))
            .map(|recv| recv.pos())
            .collect::<Vec<_>>();

        if self.config.mode == ExplorationMode::Estimation {
            self.pick_revisit(revs, pos);
            return;
        }

        if !revs.is_empty() {
            debug!("$$$$$$$ Bacward revisits for send at {}: {:?}", pos, revs);
        }

        revs.iter().for_each(|&r| {
            push_worklist(
                &mut self.current.rqueue,
                stamp,
                RevisitEnum::new_backward(r, pos),
            );
        });
    }

    // Return whether lab reads from a stamp-later send that would
    // be deleted from the revisit.
    fn revisited_by_deleted(&self, rlab: &RecvMsg, rev: &Revisit) -> bool {
        let g = &self.current.graph;
        rlab.rf().is_some_and(|rf| {
            let stamp = g.label(rf).stamp();
            // Reads from stamp-later
            stamp > rlab.stamp() &&
            // Deleted from revisit:
                // stamp-after rev.pos
                stamp > g.label(rev.pos).stamp() &&
                // and not porf-before rev.rev
                !g.send_label(rev.rev).unwrap().porf().contains(rf)
        })
    }

    fn reads_tiebreaker(&self, rlab: &RecvMsg, rev: &Revisit) -> bool {
        self.checker
            .reads_tiebreaker(&self.current.graph, rlab, rev, self.is_monitor(&rlab.pos()))
    }

    fn is_monitor(&self, recv: &Event) -> bool {
        self.monitors.contains_key(&recv.thread)
    }

    fn is_maximal_recv(&self, rlab: &RecvMsg, rev: &Revisit) -> bool {
        // Revisitable flag is a (faster) alternative to checking
        // if the sends deleted by a revisit are read by a stamp-earlier receive.
        !self.revisited_by_deleted(rlab, rev)
            && rlab.is_revisitable()
            && self.reads_tiebreaker(rlab, rev)
    }

    fn is_maximal(&self, lab: &LabelEnum, rev: &Revisit) -> bool {
        match lab {
            LabelEnum::RecvMsg(rlab) => self.is_maximal_recv(rlab, rev),
            // Predetermined CToss events are always maximal: they are not branching
            // points, so no forward revisit exists to discover blocked backward revisits.
            LabelEnum::CToss(ctlab) => {
                ctlab.is_predetermined() || ctlab.result() == ctlab.maximal()
            }
            // Instead of checking if a send is read by a stamp-earlier receive,
            // we handle this via the revisitable flag on the corresponding receive.
            LabelEnum::SendMsg(slab) => !slab.is_dropped(),
            LabelEnum::Choice(chlab) => chlab.result() == *chlab.range().end(),
            _ => true,
        }
    }

    fn filter_symmetric_rfs(&self, rfs: &mut Vec<Event>, pos: Event) {
        assert!(self.current.graph.is_recv(pos));

        let mut sym_rfs = HashSet::new();
        for rf in rfs.iter() {
            let blab = self.current.graph.thread_first(rf.thread).unwrap();
            if blab.sym_id().is_some()
                && rfs.iter().any(|rf2| {
                    rf2 != rf
                        && rf2.thread == blab.sym_id().unwrap()
                        && self.is_prefix_symmetric(blab.sym_id(), *rf)
                        && self.current.graph.label(*rf2).stamp()
                            < self.current.graph.label(*rf).stamp()
                })
            {
                sym_rfs.insert(*rf);
            }
        }
        rfs.retain(|rf| !sym_rfs.contains(rf));
    }

    fn is_prefix_symmetric(&self, sym_id: Option<ThreadId>, pos: Event) -> bool {
        if sym_id.is_none() {
            return false;
        }
        let tid = pos.thread;
        let sym_id = sym_id.unwrap();
        let sym_size = self.current.graph.thread_size(sym_id);
        let index = pos.index;
        if sym_size <= (index as usize) {
            return false;
        }
        (1..index).all(|i| {
            let lab = self.current.graph.label(Event::new(tid, i));
            let sym_lab = self.current.graph.label(Event::new(sym_id, i));
            match (lab, sym_lab) {
                // Two receives cannot be reading from the same send, so this
                // is false (unless they both timeout).
                // Checking for same-value, however, is not sound (see `symmetry_reduction.rs` test).
                (LabelEnum::RecvMsg(a), LabelEnum::RecvMsg(b)) => a.rf() == b.rf(),
                _ => true,
            }
        })
    }

    fn add_to_graph(&mut self, lab: LabelEnum) -> Event {
        let tid = lab.thread();
        let tindex = self.current.graph.thread_size(tid);
        if tindex > self.config.thread_threshold as usize && self.warn_limit > 0 {
            self.warn(&format!(
                "Large thread size {} events)! Is the test bounded?",
                tindex
            ));
            // debug
            eprintln!("Printing the large graph:");
            println!("{}", self.print_graph(None));
            // when a graph becomes too big, we can stop the search and return.
            // TODO: In principle, we should allow the exploration to proceed on the other threads.
            // TODO: We should implement this by adding a Block(TooBigThread) at the end of the
            // large thread but allowing other threads to proceed.
            // TODO: Needs scoping and work
            self.stop();
        }
        let pos = self.current.graph.add_label(lab);
        self.checker.calc_views(&mut self.current.graph, pos);
        pos
    }

    /// Recover data that was Default'd either
    /// a) during (de)serialization (counterexample replay, look for `#[serde(skip)]`, or
    /// b) explicitly (revisit replay, look for `set_pending()`)
    fn recover_lost_data(&mut self, label: LabelEnum) {
        let g = &mut self.current.graph;
        let pos = label.pos();
        match g.label_mut(pos) {
            LabelEnum::RecvMsg(rlab) => {
                if self.replay_info.replay_mode() {
                    if let LabelEnum::RecvMsg(new_rlab) = label {
                        rlab.recover_lost(new_rlab);
                    } else {
                        unreachable!();
                    }
                }
            }
            LabelEnum::SendMsg(slab) => {
                if let LabelEnum::SendMsg(new_slab) = label {
                    if self.replay_info.replay_mode() {
                        slab.recover_lost(new_slab);
                    } else {
                        slab.recover_val(new_slab);
                    }
                } else {
                    unreachable!();
                }
            }
            LabelEnum::End(elab) => {
                if let LabelEnum::End(new_elab) = label {
                    if self.replay_info.replay_mode() {
                        elab.recover_lost(new_elab);
                    } else {
                        elab.recover_result(new_elab);
                    }
                } else {
                    unreachable!();
                }
            }
            _ => {}
        }
        // Do *not* recover cache during trace replay:
        // It is, so far, not used, *and* we cannot guarantee
        // that they are sorted by stamp.
        // If we ever need the cache, without the order guarantee,
        // add a `register_send`, if `replay_mod()`.
    }

    pub(crate) fn try_revisit(&mut self) -> bool {
        loop {
            debug!("Finished execution with current rqueue {:?}", self.current.rqueue.clone());
            if self.current.rqueue.is_empty() {
                if self.try_pop_state() {
                    continue;
                }
                return false;
            }
            let rev = { pop_worklist(&mut self.current.rqueue, self.config.schedule_policy == SchedulePolicy::Arbitrary, &mut self.rng) };
            if self.config.verbose >= 3 {
                println!("Revisit {} <= {}", rev.pos(), rev.rev());
            }
            if match &rev {
                RevisitEnum::ForwardRevisit(r) => self.forward_revisit(r),
                RevisitEnum::BackwardRevisit(r) => self.backward_revisit(r),
            } {
                return true;
            }
        }
    }

    fn forward_revisit(&mut self, rev: &Revisit) -> bool {
        info!("================ begin forward_revisit ===================");
        debug!("[forward_revisit] revisit pos={}, rev={}", rev.pos, rev.rev);
        let lab = self.current.graph.label_mut(rev.pos);
        let pos = lab.pos();
        let stamp = lab.stamp();
        debug!("[forward_revisit] label={}, stamp={}", lab, stamp);

        match lab {
            LabelEnum::CToss(ctlab) => ctlab.set_result(!ctlab.result()),
            LabelEnum::Choice(chlab) => {
                let result = chlab.result();
                let end = *chlab.range().end();
                chlab.set_result(result + 1);

                if result + 1 < end {
                    // we have not reached the end yet, so set another revisit
                    push_worklist(
                        &mut self.current.rqueue,
                        stamp,
                        RevisitEnum::new_forward(pos, Event::new_init()),
                    );
                }
            }
            LabelEnum::Sample(sample) => {
                let more = sample.next();
                if more {
                    // we have not reached the end yet, so set another revisit
                    push_worklist(
                        &mut self.current.rqueue,
                        stamp,
                        RevisitEnum::new_forward(pos, Event::new_init()),
                    );
                }
            }
            LabelEnum::RecvMsg(_rlab) => self.change_rf(rev),
            LabelEnum::SendMsg(slab) => {
                slab.set_dropped();
                self.current.graph.incr_dropped_sends();
            }
            _ => panic!(),
        };
        self.current.graph.cut_to_stamp(stamp);
        debug!("After cut");
        debug!("{}", self.print_graph(None));
        true
    }

    // Mark events in the porf-prefix as non revisitable
    fn mark_prefix_non_revisitable(&mut self, send: Event) {
        let prefix = self.current.graph.send_label(send).unwrap().porf().clone();

        // Iterate on the prefix's labs
        for thread in self.current.graph.threads.iter_mut() {
            let j = thread
                .labels
                .partition_point(|lab| prefix.contains(lab.pos()));
            for lab in &mut thread.labels[..j] {
                if let LabelEnum::RecvMsg(rlab) = lab {
                    rlab.set_revisitable(false)
                }
            }
        }
    }

    fn backward_revisit(&mut self, rev: &Revisit) -> bool {
        info!(
            "================ begin backward_revisit for {:?} ===================",
            rev
        );
        let v = self.current.graph.revisit_view(rev);
        let mut ng = self.current.graph.copy_to_view(&v);
        // If any send's reader was set to the revisited receive via
        // cancelled_recv_readers fallback, update it before change_rf.
        ng.pop_fallback_readers(rev.pos);
        // println!("After computing the new graph");
        self.push_state();
        self.current.graph = ng;

        self.mark_prefix_non_revisitable(rev.rev);

        // println!("After marking prefix");

        self.change_rf(rev);

        // println!("After change rf");

        if self.config.verbose >= 3 {
            println!("After backward revisit graph");
            println!("{}", self.current.graph);
        }

        if let Some(pqueue_pair) = &self.pqueue {
            let mut queue = pqueue_pair
                .0
                .lock()
                .expect("Couldn't lock shared work queue");

            if queue.len() < ExecutionPool::MAX_QUEUE_SIZE {
                // Push this revisit onto the parallel revisit queue
                // and return false. This signals to the caller that this
                // worker can continue working on other local executions
                // that are available.
                queue.push_back(Some(self.current.graph.clone()));
                pqueue_pair.1.notify_one();
                return false;
            }
        }

        true
    }

    fn pick_ctoss(&mut self, pos: Event) -> bool {
        self.telemetry.histogram(EXECS_EST.to_owned(), 2.0);

        let toss = rand::thread_rng().gen_range(0..=1) == 0;
        cast!(self.current.graph.label_mut(pos), LabelEnum::CToss).set_result(toss);
        toss
    }

    fn pick_choice(&mut self, pos: Event) -> usize {
        let choice = cast!(self.current.graph.label_mut(pos), LabelEnum::Choice);
        let range = choice.range();
        let start = *range.start();
        let end = *range.end();
        let rand_value = rand::thread_rng().gen_range(start..=end);
        choice.set_result(rand_value);

        self.telemetry
            .histogram(EXECS_EST.to_owned(), (end - start + 1) as f64);
        rand_value
    }

    /// Change an rf according to the revisit
    fn change_rf(&mut self, rev: &Revisit) {
        self.current.graph.change_rf(rev.pos, Some(rev.rev));
    }

    fn pick_revisit(&mut self, revs: Vec<Event>, pos: Event) {
        self.telemetry
            .histogram(EXECS_EST.to_owned(), (revs.len() + 1) as f64);

        let idx = rand::thread_rng().gen_range(0..=revs.len());
        if idx < revs.len() {
            push_worklist(
                &mut self.current.rqueue,
                self.current.graph.label(pos).stamp(),
                RevisitEnum::new_backward(revs[idx], pos),
            );
            // Note: this code adds a Block with BlockType::Assume to the current execution.
            // This makes it seem like the Must model had "assume(false)" when in fact it does not.
            // This behavior only happens during Must `estimate` mode, where a random number is used
            // to pick some other revisit to execute instead of the current execution to simulate
            // the case that one of the other random revisits was chosen instead.

            // Using `BlockType::Assume` is an implementation detail which can leak out to the customer
            // in a couple ways--if they print out the execution graph they can see it, and if they
            // use a monitor, the monitor's EndCondition will be EndCondition::AssumeFailed.
            self.block_exec(BlockType::Assume); // Block this and revisit something else.
            self.stop();
        }
    }

    fn try_pop_state(&mut self) -> bool {
        if self.states.is_empty() {
            return false;
        }
        let state = self.states.pop().unwrap();
        self.current = state;
        true
    }

    fn push_state(&mut self) {
        self.states.push(std::mem::take(&mut self.current));
    }

    fn is_replay(&self, pos: Event) -> bool {
        self.current.graph.contains(pos)
    }

    fn warn(&mut self, msg: &str) {
        eprintln!("{}", msg);
        self.warn_limit -= 1;
        if self.config.warnings_as_errors {
            eprintln!("Exiting process because warnings_as_errors is set");
            std::process::exit(exitcode::DATAERR);
        }
    }

    pub(crate) fn stats(&self) -> Stats {
        Stats {
            execs: self.telemetry.read_counter(EXECS.into()).unwrap_or(0) as usize,
            block: self.telemetry.read_counter(BLOCKED.into()).unwrap_or(0) as usize,
            coverage: self.telemetry.coverage.export_aggregate().into(),
            max_graph_events: self.max_graph_events,
        }
    }

    pub(crate) fn execs_est(&self) -> f64 {
        self.telemetry
            .read_histogram(EXECS_EST.into())
            .unwrap_or(0.0)
    }

    pub(crate) fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn monitors(&mut self) -> &mut BTreeMap<ThreadId, MonitorInfo> {
        &mut self.monitors
    }

    /// Prints the trace in Turmoil format
    pub(crate) fn print_turmoil_trace(&self) {
        if self.config.turmoil_trace_file.is_some() {
            let trace = self.current.graph.top_sort(None);

            let serialized_trace = trace.filter();
            let serialized_trace_str = serde_json::to_string(&serialized_trace).unwrap();

            let mut out_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.config.turmoil_trace_file.as_ref().unwrap())
                .unwrap();

            std::io::Write::write(
                &mut out_file,
                format!("{}\n", serialized_trace_str).as_bytes(),
            )
            .unwrap();
        }
    }

    pub(crate) fn print_graph(&self, pos: Option<Event>) -> String {
        let out = if self.config.pretty_graph_printing {
            format!("{}", self.current.graph.pretty_display())
        } else {
            format!("{}", self.current.graph)
        };
        if self.config.dot_file.is_some() {
            self.print_graph_dot(pos)
                .expect("could not dot-print to supplied file");
        }
        if self.config.trace_file.is_some() {
            self.print_graph_trace(pos)
                .expect("could not print trace to supplied file");
        }
        
        out
    }

    fn print_graph_dot(&self, error: Option<Event>) -> std::io::Result<()> {
        let v = if let Some(event) = error {
            self.current.graph.porf(event)
        } else {
            self.current
                .graph
                .view_from_stamp(self.current.graph.stamp())
        };
        let num_execs = self.telemetry.read_counter(EXECS.to_owned()).unwrap_or(0);
        let create_file = error.is_some() || num_execs == 1;
        let mut out_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(create_file)
            .write(true)
            //.append(!create_file)
            .append(false)
            .open(self.config.dot_file.as_ref().unwrap())
            .unwrap();

        std::io::Write::write(
            &mut out_file,
            "strict digraph {\n\
            node [shape=plaintext]\n\
            labeljust=l\n\
            splines=false\n"
                .to_string()
                .as_bytes(),
        )?;

        let g = &self.current.graph;
        for (tid, ind) in v.entries() {
            std::io::Write::write(
                &mut out_file,
                format!("subgraph cluster_{} {{\n", tid).as_bytes(),
            )?;
            std::io::Write::write(
                &mut out_file,
                format!("\tlabel=\"thread {}\"\n", tid).as_bytes(),
            )?;
            for j in 1..ind {
                let pos = Event::new(tid, j);
                let is_error = error.is_some() && error.unwrap() == pos;
                std::io::Write::write(
                    &mut out_file,
                    format!(
                        "\t\"{}\" [label=<{}>{}]\n",
                        pos,
                        g.label(pos),
                        if is_error {
                            ",style=filled,fillcollor=yellow"
                        } else {
                            ""
                        }
                    )
                    .as_bytes(),
                )?;
            }
            std::io::Write::write(&mut out_file, "}\n".to_string().as_bytes())?;
        }

        for (tid, ind) in v.entries() {
            for j in 1..ind + 1 {
                let pos = Event::new(tid, j);
                if j < ind {
                    // last event for this thread
                    std::io::Write::write(
                        &mut out_file,
                        format!("\"{}\" -> \"{}\"\n", pos, pos.next()).as_bytes(),
                    )?;
                }
                if g.is_recv(pos) {
                    let rlab = g.recv_label(pos).unwrap();
                    if rlab.rf().is_some() {
                        std::io::Write::write(
                            &mut out_file,
                            format!("\"{}\" -> \"{}\"[color=green]\n", rlab.rf().unwrap(), pos)
                                .as_bytes(),
                        )?;
                    }
                }
            }
        }

        std::io::Write::write(&mut out_file, "}\n".to_string().as_bytes())?;
        Ok(())
    }

    fn print_graph_trace(&self, error: Option<Event>) -> std::io::Result<()> {
        let g = &self.current.graph;

        let maxs = if let Some(e) = error {
            vec![e]
        } else {
            g.thread_ids()
                .iter()
                .filter(|&&tid| {
                    let last = g.thread_last(tid).unwrap().pos();
                    !g.is_send(last) || g.is_rf_maximal_send(last)
                })
                .map(|&tid| g.thread_last(tid).unwrap().pos())
                .collect()
        };

        let num_execs = self.telemetry.read_counter(EXECS.to_owned()).unwrap_or(0);
        let create_file = error.is_some() || num_execs == 1;
        let mut out_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(create_file)
            .write(true)
            .append(!create_file)
            .open(self.config.trace_file.as_ref().unwrap())
            .unwrap();

        let mut v = VectorClock::new();
        for e in maxs {
            self.print_graph_trace_util(&mut out_file, &mut v, e)?
        }
        std::io::Write::write(&mut out_file, "\n".to_string().as_bytes())?;
        Ok(())
    }

    fn print_graph_trace_util(
        &self,
        file: &mut std::fs::File,
        view: &mut VectorClock,
        e: Event,
    ) -> std::io::Result<()> {
        let g = &self.current.graph;

        if view.contains(e) {
            return Ok(());
        }

        let start_idx = view.get(e.thread).unwrap_or(0);

        view.update_or_set(e);
        for i in start_idx..=e.index {
            let ei = Event::new(e.thread, i);
            if g.is_recv(ei) && g.recv_label(ei).unwrap().rf().is_some() {
                self.print_graph_trace_util(file, view, g.recv_label(ei).unwrap().rf().unwrap())?;
            }
            if let LabelEnum::TJoin(jlab) = g.label(ei) {
                self.print_graph_trace_util(file, view, g.thread_last(jlab.cid()).unwrap().pos())?;
            }
            if let LabelEnum::Begin(blab) = g.label(ei) {
                if blab.parent().is_some() {
                    self.print_graph_trace_util(file, view, blab.parent().unwrap())?;
                }
            }
            std::io::Write::write(file, format!("{}\n", g.label(ei),).as_bytes())?;
        }
        Ok(())
    }

    /// Enforce that monitors are only spawned from the main thread
    /// at the very start of the execution.
    pub(crate) fn validate_monitor_spawn(&self, curr: &Event) {
        // Check for simplicity (optional)
        if curr.thread != main_thread_id() {
            panic!("Monitors can only be spawned from the main thread");
        }
        let g = &self.current.graph;
        for i in 1..curr.index {
            let lab = g.create_label(Event::new(curr.thread, i));
            if lab.is_none() || !self.monitors.contains_key(&lab.unwrap().cid()) {
                panic!("Monitors must be spawned before any other instruction");
            }
        }
    }
    pub(crate) fn unstuck_joiners(state: &mut ExecutionState, finished: ThreadId) {
        let must = state.must.borrow();
        for task in state.tasks.iter_mut() {
            if !task.is_stuck() {
                continue;
            }
            // A task with TaskState::Blocked that is waiting for a Join
            // must have the Join label in the graph, *but* the instruction
            // pointer is one instruction behind.
            // Detect this, ensure it's waiting for the finished tid, and unblock it
            let tid = must.to_thread_id(task.id());
            let curr = Event::new(tid, task.instructions as u32);
            if let LabelEnum::TJoin(jlab) = must.current.graph.label(curr.next()) {
                if jlab.cid() == finished {
                    task.unstuck();
                }
            }
        }
    }
}

fn push_worklist(worklist: &mut RQueue, stamp: usize, r: RevisitEnum) {
    if worklist.get(&stamp).is_none() {
        worklist.insert(stamp, Vec::new());
    }
    let alts = worklist.get_mut(&stamp).unwrap();
    alts.push(r);
}

fn pop_worklist(worklist: &mut RQueue, is_arbitrary: bool, rng: &mut Pcg64Mcg) -> RevisitEnum {
    let (stamp, rev, is_empty) = {
        let (stamp, revs) = worklist
            .iter_mut()
            .next_back()
            .expect("worklist is not empty");
        if !is_arbitrary  {
            let rev = revs.pop().unwrap();
            (*stamp, rev, revs.is_empty())
        }
        else {
            // Choose randomly from alternatives at the highest stamp
	        let idx = rng.gen_range(0..revs.len());
	        let rev = revs.swap_remove(idx);
            (*stamp, rev, revs.is_empty())
        }
    };
    if is_empty {
        worklist.remove(&stamp);
    }
    rev
}

#[cfg(test)]
mod tests {
    use REPLAY::{ReplayInformation, TopologicallySortedExecutionGraph};

    use super::*;

    use crate::{
        event::Event,
        loc::{CommunicationModel, SendLoc},
        thread::construct_thread_id,
        Config, LabelEnum,
    };

    fn setup_must_for_replay() -> Must {
        let main_tid = construct_thread_id(0);
        let config = Config::default();
        let mut must = Must::new(config.clone(), true);
        let mut tseg = TopologicallySortedExecutionGraph::new();
        let send_at_0 = LabelEnum::SendMsg(SendMsg::new(
            Event::new(main_tid, 0),
            SendLoc::new_empty(main_tid),
            CommunicationModel::default(),
            Val::new("bob"),
            MonitorSends::new(),
            false,
        ));
        tseg.insert_label(send_at_0.clone());
        let error_state = MustState::new();
        must.replay_info = ReplayInformation::create(tseg, error_state, config.clone());
        must.replay_info.next_task(); // Advance to (t0, 0)
        must
    }

    #[test]
    #[should_panic(expected = "Executing (t0, 1) instead of the counterexample's (t0, 0)")]
    fn test_try_consume_panic_on_index_mismatch() {
        let mut must = setup_must_for_replay();

        let tid = construct_thread_id(0);
        let send_at_1 = LabelEnum::SendMsg(SendMsg::new(
            Event::new(tid, 1),
            SendLoc::new_empty(tid),
            CommunicationModel::default(),
            Val::new("bob"),
            MonitorSends::new(),
            false,
        ));

        // Try to replay with the wrong thread.
        must.try_consume(&send_at_1);
    }
}
