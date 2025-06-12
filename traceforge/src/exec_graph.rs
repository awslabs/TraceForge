use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::channel::Thread;
use crate::event::Event;
use crate::indexed_map::IndexedMap;
use crate::loc::{Loc, RecvLoc};
use crate::revisit::Revisit;
use crate::runtime::task::TaskId;
use crate::thread::{construct_thread_id, main_thread_id};
use crate::vector_clock::VectorClock;
use crate::{event_label::*, Val};
use crate::{replay as REPLAY, ThreadId};

/// Encapsulates the execution information about a single thread
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ThreadInfo {
    tid: ThreadId,
    task_id: Option<TaskId>,
    tclab: TCreate,
    pub(crate) labels: Vec<LabelEnum>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ExecutionGraph {
    pub(crate) threads: IndexedMap<ThreadInfo>,
    stamp: usize,
    task_id_map: HashMap<TaskId, ThreadId>,
    pub(crate) finished_threads: HashSet<ThreadId>,
    #[serde(skip)]
    /// A cache of send events per channel that could (modulo tag) read from them,
    /// sorted by increasing stamp.
    ///
    /// This is *empty* during `replay_mode` (trace replay).
    ///
    /// A send can be included in two channel lists, in case it's monitored.
    sends: HashMap<Loc, Vec<Event>>,
    #[serde(skip)]
    recvs: HashMap<Loc, Vec<Event>>,
    dropped_sends: usize,
}

impl ExecutionGraph {
    pub(crate) fn new() -> ExecutionGraph {
        let t0 = main_thread_id();
        let event = Event::new(t0, 0);
        ExecutionGraph {
            threads: IndexedMap::new_with_first(ThreadInfo {
                tid: t0,
                task_id: Some(TaskId(0)),
                tclab: TCreate::new(event, t0, Some("main".to_owned()), false, None, vec![]),
                labels: vec![LabelEnum::Begin(Begin::main())],
            }),
            stamp: 0,
            task_id_map: HashMap::from([(TaskId(0), t0)]),
            finished_threads: HashSet::new(),
            sends: HashMap::new(),
            recvs: HashMap::new(),
            dropped_sends: 0,
        }
    }

    /// Called just before we start an execution. When doing a forwards or backwards revisit,
    /// the graph is cloned (not constructed fresh) so it can contain old state that
    /// should not be carried forward, such as the old Shuttle TaskId values that corresponded
    /// to threads.
    pub(crate) fn initialize_for_execution(&mut self) {
        // The main thread is always task_id 0, which is assumed by the constructor above.
        // Assert that, so that we can detect state confusion if the task id of thread 0 is
        // accidentally changed.
        assert_eq!(self.threads[0].task_id, Some(TaskId(0)));

        // Reset all the other task ids to None
        for t in &mut self.threads.iter_mut().skip(1) {
            self.task_id_map.remove(&t.task_id.unwrap());
            t.task_id = None;
        }

        self.finished_threads.clear();
        let tids = self.threads.iter().map(|t| t.tid).collect::<Vec<_>>();
        tids.iter().for_each(|tid| self.on_thread_changed(tid));

        // Reset all the End labels values to Default.
        for thr in self.threads.iter_mut() {
            if let Some(LabelEnum::End(e)) = thr.labels.last_mut() {
                e.result.set_pending();
            }
        }

        // Clear all send label values.
        // Note that this has the same effect as serializing and then deserializing
        // the execution graph. Without this code, we are vulnerable to bugs
        // which assume that a replay/revisit will already contain the values.
        for thr in self.threads.iter_mut() {
            for e in &mut thr.labels {
                if let LabelEnum::SendMsg(s) = e {
                    s.val.set_pending();
                }
            }
        }
    }

    pub(crate) fn on_thread_changed(&mut self, tid: &ThreadId) {
        if let Some(LabelEnum::End(_)) = self.get_thr(tid).labels.last() {
            self.finished_threads.insert(*tid);
        } else {
            self.finished_threads.remove(tid);
        }
    }

    pub(crate) fn validate_replay_event(&self, actual: &LabelEnum) {
        let expected = &self.get_thr(&actual.thread()).labels[actual.index() as usize];
        Self::panic_if_err(expected.compare_for_replay(actual));
    }

    pub(crate) fn panic_if_err(res: Result<(), String>) {
        if let Err(e) = res {
            panic!("Incorrect TraceForge Program. TraceForge programs must be deterministic. Any nondeterminism should be under the control of TraceForge via the nondet() function.\n{}",
            e);
        }
    }

    /// Find the ThreadInfo structure for a thread, or panic with an error message.
    pub(crate) fn get_thr(&self, tid: &ThreadId) -> &ThreadInfo {
        self.get_thr_opt(tid).unwrap_or_else(|| {
            panic!(
                "Can't find thread {} in graph with thread ids {:?}",
                *tid,
                self.threads.iter().map(|t| t.tid).collect::<Vec<_>>()
            )
        })
    }

    pub(crate) fn get_thr_opt(&self, tid: &ThreadId) -> Option<&ThreadInfo> {
        self.threads.get(Into::<usize>::into(*tid))
    }

    pub(crate) fn get_thr_opt_mut(&mut self, tid: &ThreadId) -> Option<&mut ThreadInfo> {
        self.threads.get_mut(Into::<usize>::into(*tid))
    }

    pub(crate) fn get_thr_mut(&mut self, tid: &ThreadId) -> &mut ThreadInfo {
        self.get_thr_opt_mut(tid).unwrap_or_else(|| {
            panic!("Can't find thread {}", *tid);
        })
    }

    // ====

    /// Iterate over all sends in the execution
    pub(crate) fn all_store_iter(&self) -> impl Iterator<Item = &SendMsg> {
        self.threads
            .iter()
            .flat_map(|t| t.labels.iter().map(|l| l.pos()))
            .filter_map(move |e| self.send_label(e))
    }

    /// Iterate over the stores that a receive can read from, accounting for tags
    /// and sorted by (increasing) timestamp.
    pub(crate) fn matching_stores<'a>(
        &'a self,
        recv_loc: &'a RecvLoc,
    ) -> impl Iterator<Item = &'a SendMsg> {
        recv_loc
            .locs()
            .iter()
            // There are no duplicate channels in a Location (RecvLoc has disjoint Loc)
            .filter_map(move |c| self.sends.get(c))
            .map(move |v| {
                v.iter()
                    .map(move |&pos| self.send_label(pos).unwrap())
                    // Filter dropped sends and tags
                    .filter(move |&slab| !slab.is_dropped() && recv_loc.matches_tag(slab))
            })
            .fold(
                Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &SendMsg>>,
                |acc, it| {
                    Box::new(merging_iterator::MergeIter::with_custom_ordering(
                        acc,
                        it,
                        |a, b| a.stamp() < b.stamp(),
                    ))
                },
            )
    }

    /// Iterate over the receives that can read from a send, accounting for tags
    /// and sorted by *decreasing* timestamp.
    pub(crate) fn rev_matching_recvs<'a>(
        &'a self,
        send: &'a SendMsg,
    ) -> impl Iterator<Item = &'a RecvMsg> {
        let init = self.recvs.get(send.loc()).map(move |vec| {
            vec.iter()
                .map(move |&pos| self.recv_label(pos).unwrap())
                // In *reverse* stamp order
                .rev()
        });
        // Iterate over the receives that can read from the send
        let init: Box<dyn Iterator<Item = &RecvMsg>> = match init {
            Some(i) => Box::new(i),
            None => Box::new(std::iter::empty()),
        };
        // Get the receives that can *monitor* the send
        send.monitor_sends()
            .keys()
            .map(|&tid| {
                self.recvs.get(&Loc::new(Thread(tid))).map(|v| {
                    v.iter()
                        .map(move |&pos| self.recv_label(pos).unwrap())
                        // Again, *reverse* stamp order
                        .rev()
                })
            })
            .flatten()
            // Chain all together, respecting the (reverse) stamp order
            .fold(init, |acc, it| {
                Box::new(merging_iterator::MergeIter::with_custom_ordering(
                    acc,
                    it,
                    // *Reverse* comparison, since we're combining reversed iterators
                    |a, b| a.stamp() > b.stamp(),
                ))
            })
            // Filter tags
            .filter(move |&rlab| rlab.recv_loc().matches_tag(send))
    }

    pub(crate) fn stamp(&self) -> usize {
        self.stamp
    }

    pub(crate) fn next_stamp(&mut self) -> usize {
        self.stamp += 1;
        self.stamp
    }

    pub(crate) fn add_new_thread(&mut self, tclab: TCreate, task_id: TaskId) {
        assert!(self.get_thr_opt(&tclab.cid()).is_none());

        let tid = tclab.cid();
        let index: usize = tid.into();
        self.threads.set(
            index,
            ThreadInfo {
                tid,
                task_id: Some(task_id),
                tclab,
                labels: vec![],
            },
        );

        self.task_id_map.insert(task_id, tid);
    }

    pub(crate) fn thread_ids(&self) -> BTreeSet<ThreadId> {
        self.threads.iter().map(|t| t.tid).collect()
    }

    pub(crate) fn to_task_id(&self, tid: ThreadId) -> Option<TaskId> {
        self.get_thr(&tid).task_id
    }

    pub(crate) fn to_thread_id(&self, task_id: TaskId) -> ThreadId {
        if let Some(tid) = self.task_id_map.get(&task_id) {
            *tid
        } else {
            panic!("no thread id for task id {:?}", task_id);
        }
    }

    pub(crate) fn get_thread_tclab(&self, tid: ThreadId) -> TCreate {
        self.get_thr(&tid).tclab.clone()
    }

    pub(crate) fn tid_for_spawn(&self, pos: &Event, origination_vec: &[u32]) -> ThreadId {
        assert_eq!(origination_vec.last(), Some(&pos.index));

        // Either this is a backtrack/replay, in which case the graph should already contain
        // pos. In that case, the spawned thread ID must not change, and additionally the
        // new spawn info must be compatible with old spawn info.

        // Else, if this is a new spawn, generate a new tid which is higher than any existing tid.

        // Walk origination_vec to make sure there's a spawn at each entry, except possibly the last
        let mut spawning_thread = main_thread_id();
        for (i, &event_idx) in origination_vec.iter().enumerate() {
            let is_last_spawn = i == origination_vec.len() - 1;
            if event_idx < self.thread_size(spawning_thread) as u32 {
                let spawn_pos = Event::new(spawning_thread, event_idx);
                let lab = self.label(spawn_pos);
                // Event already exists.
                if let LabelEnum::TCreate(tclab) = lab {
                    let expected_origination_vec = &origination_vec[0..=i];
                    // Check the tclab
                    assert_eq!(expected_origination_vec, tclab.origination_vec());

                    if is_last_spawn {
                        return tclab.cid(); // Return the same cid as was used last time.
                    }

                    spawning_thread = tclab.cid(); // Loop to check the next index
                } else {
                    let msg = format!("Expected spawn event at {:?} but have {:?}", spawn_pos, lab);
                    Self::panic_if_err(Result::Err(msg));
                }
            } else if !is_last_spawn {
                let msg = format!(
                    "Expected to find event at {} for thread {}",
                    event_idx, spawning_thread
                );
                Self::panic_if_err(Result::Err(msg));
            }
        }

        // Return a new thread that is one larger than existing.
        let opaque_id = self
            .threads
            .iter()
            .max_by_key(|t| t.tid.to_number())
            .expect("Didn't expect zero threads!")
            .tid
            .to_number();
        let new_id = opaque_id + 1;
        construct_thread_id(new_id)
    }

    pub(crate) fn set_task_for_replay(&mut self, tid: ThreadId, task_id: TaskId) {
        if let Some(tid) = self.task_id_map.get(&task_id) {
            panic!(
                "A different thread {} already is associated to task_id {:?}",
                *tid, &task_id
            );
        }

        let index: usize = tid.into();
        let thread = self.threads.get_mut(index).unwrap();

        if let Some(other_task_id) = thread.task_id {
            panic!(
                "This thread {:?} already has a task_id {:?}",
                thread.tid, other_task_id
            );
        }

        thread.task_id = Some(task_id);
        self.task_id_map.insert(task_id, thread.tid);
    }

    pub(crate) fn thread_size(&self, t: ThreadId) -> usize {
        self.get_thr(&t).labels.len()
    }

    pub(crate) fn thread_last(&self, t: ThreadId) -> Option<&LabelEnum> {
        self.get_thr(&t).labels.last()
    }

    pub(crate) fn thread_first(&self, t: ThreadId) -> Option<&Begin> {
        self.get_thr(&t).labels.first().map(|lab| {
            if let LabelEnum::Begin(blab) = lab {
                blab
            } else {
                panic!()
            }
        })
    }

    pub(crate) fn is_thread_blocked(&self, t: ThreadId) -> bool {
        matches!(self.thread_last(t).unwrap(), LabelEnum::Block(_))
    }

    pub(crate) fn is_thread_complete(&self, t: ThreadId) -> bool {
        let old = matches!(self.thread_last(t).unwrap(), LabelEnum::End(_));
        let new = self.finished_threads.contains(&t);
        assert_eq!(old, new, "finished_threads set is not correct.");
        new
    }

    pub(crate) fn is_thread_daemon(&self, t: ThreadId) -> bool {
        self.get_thr(&t).tclab.daemon()
    }

    /// Add a label to the graph, giving it a new stamp if it does not have one.
    pub(crate) fn add_label(&mut self, lab: LabelEnum) -> Event {
        self.add(lab).pos()
    }

    fn add(&mut self, mut lab: LabelEnum) -> &LabelEnum {
        if !lab.stamped() {
            lab.set_stamp(self.next_stamp());
        }

        let pos = lab.pos();

        let existing_label_count = self.thread_size(lab.thread());

        // Allow label overwrites
        match (lab.index() as usize).cmp(&existing_label_count) {
            Ordering::Greater => {
                panic!(
                    "Label index {} must be <= {}",
                    lab.index(),
                    existing_label_count
                );
            }
            Ordering::Equal => {
                self.get_thr_mut(&pos.thread).labels.push(lab);
            }
            Ordering::Less => {
                // Overwriting a label. Validate it to make sure that the existing execution graph
                // remains consistent.
                let old_label = self.get_thr(&pos.thread).labels[pos.index as usize].clone();
                let old_tclab: Option<TCreate> = if let LabelEnum::TCreate(x) = old_label {
                    Some(x)
                } else {
                    None
                };
                let new_tclab: Option<TCreate> = if let LabelEnum::TCreate(x) = lab.clone() {
                    Some(x)
                } else {
                    None
                };
                assert_eq!(
                    old_tclab.is_some(),
                    new_tclab.is_some(),
                    "Requiring {:?} == {:?}",
                    old_tclab,
                    new_tclab
                );
                self.get_thr_mut(&pos.thread).labels[pos.index as usize] = lab;
            }
        }
        self.on_thread_changed(&pos.thread);
        &self.get_thr(&pos.thread).labels[pos.index as usize]
    }

    pub(crate) fn contains(&self, e: Event) -> bool {
        self.get_thr_opt(&e.thread).is_some() && (e.index as usize) < self.thread_size(e.thread)
    }

    pub(crate) fn remove_last(&mut self, t: ThreadId) {
        self.get_thr_mut(&t).labels.pop();
        self.on_thread_changed(&t);
    }

    pub(crate) fn label(&self, e: Event) -> &LabelEnum {
        &self.get_thr(&e.thread).labels[e.index as usize]
    }

    // Temporarily, remove if it stays unused
    #[allow(dead_code)]
    pub(crate) fn label_opt(&self, e: Event) -> Option<&LabelEnum> {
        self.get_thr(&e.thread).labels.get(e.index as usize)
    }

    pub(crate) fn label_mut(&mut self, e: Event) -> &mut LabelEnum {
        &mut self.get_thr_mut(&e.thread).labels[e.index as usize]
    }

    pub(crate) fn create_label(&self, e: Event) -> Option<&TCreate> {
        if let LabelEnum::TCreate(l) = self.label(e) {
            Some(l)
        } else {
            None
        }
    }

    pub(crate) fn is_recv(&self, e: Event) -> bool {
        matches!(self.label(e), LabelEnum::RecvMsg(_))
    }

    pub(crate) fn recv_label(&self, e: Event) -> Option<&RecvMsg> {
        if let LabelEnum::RecvMsg(l) = self.label(e) {
            Some(l)
        } else {
            None
        }
    }

    pub(crate) fn recv_label_mut(&mut self, e: Event) -> Option<&mut RecvMsg> {
        if let LabelEnum::RecvMsg(l) = self.label_mut(e) {
            Some(l)
        } else {
            None
        }
    }

    pub(crate) fn val(&self, e: Event) -> Option<&Val> {
        match self.label(e) {
            LabelEnum::RecvMsg(rlab) => {
                if let Some(rf) = rlab.rf() {
                    Some(self.send_label(rf).unwrap().recv_val(rlab))
                } else {
                    None
                }
            }
            LabelEnum::Block(_) => None, // This happens during replay.
            a => panic!("Expecting RecvMsg or Block but got {}", a),
        }
    }

    /// Returns the value received by e. Returns None if e is Block.
    pub(crate) fn val_copy(&self, rpos: Event) -> Option<Val> {
        self.val(rpos).cloned()
    }

    pub(crate) fn is_send(&self, e: Event) -> bool {
        matches!(self.label(e), LabelEnum::SendMsg(_))
    }

    pub(crate) fn send_label(&self, e: Event) -> Option<&SendMsg> {
        if let LabelEnum::SendMsg(l) = self.label(e) {
            Some(l)
        } else {
            None
        }
    }

    pub(crate) fn send_label_mut(&mut self, e: Event) -> Option<&mut SendMsg> {
        if let LabelEnum::SendMsg(l) = self.label_mut(e) {
            Some(l)
        } else {
            None
        }
    }

    /// Returns whether this is a send that is not read by *anyone*
    pub(crate) fn is_rf_maximal_send(&self, e: Event) -> bool {
        self.send_label(e)
            .filter(|send| send.is_unread() && !send.is_monitor_read())
            .is_none()
    }

    /// vector clock with events stamp-{before or equal} the revisited (inclusive)
    /// and the porf-prefix of the rev (inclusive)
    // N.B. it doesn't include the revisitor's rf/Create/End dependencies
    pub(crate) fn revisit_view(&self, rev: &Revisit) -> VectorClock {
        let mut v = self.view_from_stamp(self.label(rev.pos).stamp());
        v.update(self.send_label(rev.rev).unwrap().porf());

        // v.update() may cause more TCreate labs to be visible in the vector clock
        // Find those TCreate labels and add their corresponding Begin labels to the clock.
        // This is necessary because the call to v.update() may have made more TCreate events
        // visible than `view_from_stamp` chose to expose.
        for thr in self.threads.iter() {
            if let Some(vc_limit_inclusive) = v.get(thr.tid) {
                for lab in thr.labels.iter().take(vc_limit_inclusive as usize + 1) {
                    if let LabelEnum::TCreate(tclab) = lab {
                        // update_or_set is idempotent--does nothing if the VC already
                        // has this event.
                        v.update_or_set(Event::new(tclab.cid(), 0));
                    }
                }
            }
        }

        v
    }

    /// Return a view with all the events up to the stamp (inclusive)
    pub(crate) fn view_from_stamp(&self, s: usize) -> VectorClock {
        let mut v = VectorClock::new();
        for thread in self.threads.iter() {
            // Label are sorted by stamp. Find the last, if any, s.t. stamp <= s.
            let i = thread.labels.partition_point(|lab| lab.stamp() <= s);
            if i != 0 {
                v.update_or_set(thread.labels[i - 1].pos());
            }
        }
        v
    }

    /// Return the index of the receiving channel, if any
    pub(crate) fn get_receiving_index(&self, rlab: &RecvMsg) -> Option<usize> {
        rlab.rf().and_then(|send| {
            let slab = self.send_label(send).unwrap();
            if rlab.monitors(slab) {
                // Monitor receives should have a single (legacy) channel
                Some(0)
            } else {
                Some(rlab.recv_loc().get_matching_index(slab.send_loc()))
            }
        })
    }

    // Removes recv from the send's readers
    fn remove_from_readers(&mut self, recv: Event) {
        let rlab = self.recv_label(recv).unwrap();
        if let Some(old_send) = rlab.rf() {
            let monitors = rlab.monitors(self.send_label(old_send).unwrap());
            let old_send = self.send_label_mut(old_send).unwrap();
            if monitors {
                old_send.remove_monitor_reader(recv)
            }
            if old_send.reader().is_some_and(|r| r == recv) {
                old_send.set_reader(None);
            }
        }
    }

    /// Change rf in-place, updating the senders' readers
    pub(crate) fn change_rf(&mut self, recv: Event, send: Option<Event>) {
        assert!(self.is_recv(recv));
        assert!(send.is_none() || self.is_send(send.unwrap()));

        self.remove_from_readers(recv);

        // Set recv as a reader of the new send
        if let Some(new_send) = send {
            if self
                .recv_label(recv)
                .unwrap()
                .monitors(self.send_label(new_send).unwrap())
            {
                self.send_label_mut(new_send)
                    .unwrap()
                    .add_monitor_reader(recv)
            } else {
                self.send_label_mut(new_send)
                    .unwrap()
                    .set_reader(Some(recv))
            }
        }

        // Set the recv's rf to the new send
        self.recv_label_mut(recv).unwrap().set_rf(send);
    }

    fn check_spawn_invariants(&self) {
        // This function checks the consistency of the information about which thread spawned
        // which, and at what event number.

        // This information is represented 3 ways:
        // 1. self.threads events, filtered for TCreate
        // 2. self.threads events, filtered for Begin, examining parent
        // 3. self.thdinfo

        // To check this, construct 3 maps, each of which has a format of
        // (child_thread_id -> (parent_thread_id, spawn_event_index)). After this, assert
        // that all the maps are equal; if not, then the graph is inconsistent.
        // Note that the main thread is not present in any of these maps.

        let child_thread_ids: BTreeSet<ThreadId> = self
            .thread_ids()
            .iter()
            .copied()
            .filter(|&tid| tid != main_thread_id())
            .collect();

        let mut threads_from_tcreate: BTreeMap<ThreadId, (ThreadId, usize)> = BTreeMap::new();
        for thread_info in self.threads.iter() {
            let parent_thread_id = thread_info.tid;
            for (event_idx, event) in thread_info.labels.iter().enumerate() {
                if let LabelEnum::TCreate(tc) = &event {
                    let child_thread_id = tc.cid();
                    assert!(!threads_from_tcreate.contains_key(&child_thread_id));
                    threads_from_tcreate.insert(child_thread_id, (parent_thread_id, event_idx));
                }
            }
        }

        // Assert every thread has a TCreate entry.
        let thread_ids_from_tcreate = threads_from_tcreate.keys().copied().collect::<Vec<_>>();
        let child_vec: Vec<ThreadId> = child_thread_ids.iter().copied().collect();
        assert_eq!(
            child_vec, thread_ids_from_tcreate,
            "threads and TCreate labels aren't consistent"
        );

        let mut threads_from_begin: BTreeMap<ThreadId, (ThreadId, usize)> = BTreeMap::new();
        for thread_info in self.threads.iter() {
            if thread_info.tid == main_thread_id() {
                continue;
            }
            let child_thread_id = thread_info.tid;
            if let Some(LabelEnum::Begin(blab)) = thread_info.labels.first() {
                if let Some(Event {
                    thread: parent_thread_id,
                    index: event_idx,
                }) = blab.parent()
                {
                    assert!(!threads_from_begin.contains_key(&child_thread_id));
                    threads_from_begin
                        .insert(child_thread_id, (parent_thread_id, event_idx as usize));
                } else {
                    panic!("Every thread other than main must have a parent");
                }
            } else {
                panic!("First event must be Begin");
            }
        }

        assert_eq!(
            threads_from_begin, threads_from_tcreate,
            "begin and tcreate events are inconsistent"
        );

        let mut threads_from_thdinfo: BTreeMap<ThreadId, (ThreadId, usize)> = BTreeMap::new();
        for thread_info in self.threads.iter() {
            if thread_info.tid == main_thread_id() {
                continue;
            }
            let child_thread_id = thread_info.tid;
            let Event {
                thread: parent_thread_id,
                index: event_idx,
            } = thread_info.tclab.pos();
            assert!(!threads_from_thdinfo.contains_key(&child_thread_id));
            threads_from_thdinfo.insert(child_thread_id, (parent_thread_id, event_idx as usize));
        }

        // the information about which threads exist and how they were spawned MUST match up.
        assert_eq!(
            threads_from_tcreate, threads_from_thdinfo,
            "self.thdinfo has information that's inconsistent with self.threads() events"
        );
    }

    // Cache the send in the per-channel map, accounting for monitors.
    pub(crate) fn register_send(&mut self, send: &Event) {
        let monitor_tids = self
            .send_label(*send)
            .unwrap()
            .monitor_sends()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for tid in monitor_tids {
            // Monitors receive from thread channels
            let legacy_chan = Loc::new(Thread(tid));
            let sends = self.sends.entry(legacy_chan).or_default();
            // debug_assert!(!sends.contains(send));
            sends.push(*send);
        }

        let slab = self.send_label(*send).unwrap();
        let sends = self.sends.entry(slab.loc().clone()).or_default();
        // debug_assert!(!sends.contains(send));
        sends.push(*send);
    }

    // Cache the receive in the per-channel map, *not* accounting for monitor.
    pub(crate) fn register_recv(&mut self, recv: &Event) {
        let rlab = self.recv_label(*recv);
        // We might have been called with a Block event, ignore it
        if rlab.is_none() {
            return;
        }
        let locs = rlab.unwrap().recv_loc().locs().clone();
        locs.iter().for_each(|l| {
            self.recvs.entry(l.clone()).or_default().push(*recv);
        });
    }

    pub(crate) fn dropped_sends(&self) -> usize {
        self.dropped_sends
    }

    pub(crate) fn incr_dropped_sends(&mut self) {
        self.dropped_sends += 1;
    }

    pub(crate) fn decr_dropped_sends(&mut self, count: usize) {
        self.dropped_sends -= count;
    }

    fn cut_to_view(&mut self, v: &VectorClock) {
        let mut deleted: HashSet<Event> = HashSet::new();
        // Flag for fast-path: none has been dropped, nothing to keep track
        let some_dropped = self.dropped_sends() != 0;

        // Send cache: remove the sends which are not visible in the vector clock
        self.sends.values_mut().for_each(|stores| {
            stores.retain(|e| {
                let kept = v.contains(*e);
                if !kept && some_dropped {
                    // Some sends might be in multiple channels (monitored sends)
                    deleted.insert(*e);
                }
                kept
            })
        });
        self.sends.retain(|_, vec| !vec.is_empty());

        // Maintain dropped counter
        if some_dropped {
            let mut deleted_dropped: usize = 0;
            deleted.iter().for_each(|&e| {
                self.send_label(e).map(|s| {
                    if s.is_dropped() {
                        deleted_dropped += 1;
                    }
                });
            });
            self.decr_dropped_sends(deleted_dropped);
        }

        // Recv cache: remove the receives which are not visible in the vector clock
        self.recvs
            .values_mut()
            .for_each(|vec| vec.retain(|e| v.contains(*e)));

        // Readers cache: remove the deleted receives from the sender's readers cache
        let mut deleted_receives = vec![];
        for threads in self.threads.iter() {
            let j = threads.labels.partition_point(|lab| v.contains(lab.pos()));
            for lab in threads.labels[j..].iter() {
                if let LabelEnum::RecvMsg(rlab) = lab {
                    deleted_receives.push(rlab.pos());
                }
            }
        }

        for deleted in deleted_receives {
            self.remove_from_readers(deleted);
        }

        // Erase all the threads not found in the vector clock.
        let threads = &mut self.threads;
        let tasks = &mut self.task_id_map;
        threads.retain(|t| {
            if v.get(t.tid).is_some() {
                true
            } else {
                tasks.remove(&t.task_id.unwrap());
                false
            }
        });

        // Remove the labels from each thread which are not visible in the vector clock
        let tids = self.threads.iter().map(|t| t.tid).collect::<Vec<_>>();
        for tid in tids {
            let event_idx = v
                .get(tid)
                .expect("any thread not in the vector clock should already be erased")
                as usize
                + 1;
            let ind: usize = tid.into();
            self.threads[ind].labels.truncate(event_idx);
            self.on_thread_changed(&tid);
        }

        self.check_spawn_invariants();
    }

    pub(crate) fn cut_to_stamp(&mut self, s: usize) {
        let v = self.view_from_stamp(s);
        self.cut_to_view(&v);
    }

    pub(crate) fn copy_to_view(&self, v: &VectorClock) -> ExecutionGraph {
        // Implement copy to view by cloning and then using cut_to_view.
        // This might be inefficient but it avoids duplicating a bunch of subtle logic
        // from cut_to_view
        let mut other = self.clone();
        other.cut_to_view(v);
        other
    }

    // Returns a VectorClock with the *full* porf view of pos,
    // i.e. including the rf/TCreate/TEnd dependencies.
    pub(crate) fn porf(&self, pos: Event) -> VectorClock {
        let lab = self.label(pos);
        let mut porf = lab.cached_porf().clone();
        match lab {
            LabelEnum::Begin(blab) => {
                if let Some(parent) = blab.parent() {
                    porf.update(self.label(parent).cached_porf());
                }
            }
            LabelEnum::TJoin(jlab) => {
                porf.update(self.thread_last(jlab.cid()).unwrap().cached_porf());
            }
            LabelEnum::RecvMsg(rlab) => {
                if let Some(rf) = rlab.rf() {
                    porf.update(self.label(rf).cached_porf());
                }
            }
            _ => { /* Nothing more to do */ }
        };
        porf
    }

    /// Returns whether the first event is in the porf view of the second event.
    ///
    /// Prefer this over constructing the porf view and then checking `contains`.
    pub(crate) fn in_porf(&self, first: Event, second: Event) -> bool {
        let lab = self.label(second);
        match lab {
            LabelEnum::Begin(blab) => {
                if let Some(parent) = blab.parent() {
                    if self.label(parent).cached_porf().contains(first) {
                        return true;
                    }
                }
            }
            LabelEnum::TJoin(jlab) => {
                if self
                    .thread_last(jlab.cid())
                    .unwrap()
                    .cached_porf()
                    .contains(first)
                {
                    return true;
                }
            }
            LabelEnum::RecvMsg(rlab) => {
                if let Some(rf) = rlab.rf() {
                    if self.label(rf).cached_porf().contains(first) {
                        return true;
                    }
                }
            }
            _ => { /* Nothing more to do */ }
        };
        return lab.cached_porf().contains(first);
    }

    /// This function creates a linearization of an execution graph.
    /// `pos` represents the position of the error node (a Block node due to assertion violation)
    /// from which we will build the linearization, i.e.,
    /// `pos` will be the last node in the linearized data structure.
    /// This helps us obtain a minimal execution trace since there might be other
    /// nodes in the graph that are not relevant to the assertion violation.
    pub(crate) fn top_sort(&self, pos: Option<Event>) -> REPLAY::TopologicallySortedExecutionGraph {
        let maxs = if pos.is_some() {
            vec![pos.unwrap()]
        } else {
            self.threads
                .iter()
                .map(|t| t.tid)
                .filter(|&tid| {
                    let last = self.thread_last(tid).unwrap().pos();
                    !self.is_send(last) || self.is_rf_maximal_send(last)
                })
                .map(|tid| self.thread_last(tid).unwrap().pos())
                .collect()
        };

        // A new vector clock to track how events are added to the linearization
        let mut v = VectorClock::new();

        // Create an empty linearization
        let mut sorted_graph = REPLAY::TopologicallySortedExecutionGraph::new();

        for e in maxs {
            self.top_sort_util(&mut v, &mut sorted_graph, e);
        }

        sorted_graph
    }

    /// Recursive function to add nodes to the graph
    fn top_sort_util(
        &self,
        view: &mut VectorClock,
        graph: &mut REPLAY::TopologicallySortedExecutionGraph,
        e: Event,
    ) {
        if view.contains(e) {
            return;
        }

        // Start index to begin the iteration for adding events of a given thread
        let start_idx = view.get(e.thread).unwrap_or(0);

        view.update_or_set(e);

        // Iterate over nodes of the current thread that are not added yet
        for i in start_idx..=e.index {
            // Index reference into the graph
            let ei = Event::new(e.thread, i);

            // If the event is a RecvMsg, call `top_sort_util` on the sender first
            if self.is_recv(ei) && self.recv_label(ei).unwrap().rf().is_some() {
                self.top_sort_util(view, graph, self.recv_label(ei).unwrap().rf().unwrap());
            }

            // If the event is a TJoin, call `top_sort_util` on the terminating thread
            if let LabelEnum::TJoin(jlab) = self.label(ei) {
                self.top_sort_util(view, graph, self.thread_last(jlab.cid()).unwrap().pos());
            }

            // If a thread is starting, call the parent thread first
            if let LabelEnum::Begin(blab) = self.label(ei) {
                if blab.parent().is_some() {
                    self.top_sort_util(view, graph, blab.parent().unwrap());
                }
            }

            // Now that all orderings are covered, add the label to the graph
            graph.insert_label(self.label(ei).clone());
        }
    }
}

impl Default for ExecutionGraph {
    fn default() -> Self {
        ExecutionGraph::new()
    }
}

impl std::fmt::Display for ExecutionGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Printing exec graph")?;
        for thread_info in self.threads.iter() {
            let tid = thread_info.tid;
            let daemon = (if thread_info.tclab.daemon() {
                ", daemon"
            } else {
                ""
            })
            .to_owned();
            match thread_info.tclab.name() {
                None => writeln!(f, "thread {}{}:", tid, daemon)?,
                Some(name) => writeln!(f, "thread \"{}\"[tid={}{}]:", name, tid, daemon)?,
            }
            for lab in thread_info.labels.iter() {
                writeln!(f, "\t{}", lab)?;
            }
        }
        // TODO: Display Sends per channel?
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[should_panic(expected = "no thread id for task id")]
    fn test_to_thread_id_with_no_thread_id() {
        let exec_graph = ExecutionGraph::default();
        exec_graph.to_thread_id(TaskId(1));
    }
}
