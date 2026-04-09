use crate::event::Event;
use crate::event_label::{AsEventLabel, Inbox, LabelEnum, RecvMsg, SendMsg};
use crate::exec_graph::ExecutionGraph;
use crate::loc::CommunicationModel;
use crate::revisit::Revisit;
use crate::vector_clock::VectorClock;

// A generic consistency which will, eventually, support arbitrary
// communication models, depending on the channel.

// The intended semantics of consistent(G) is
// 1. porf-acyclic(G)
// 2. for each communication model M, the restriction of G
// to events of models not stronger than M is consistent under M
// 3. receive events of monitors act as if they are CausalOrder

pub(crate) struct Consistency {}

impl Consistency {
    // Checks if there is a TotalOrder relation between the two sends slab1 and slab2
    fn send_before(&self, g: &ExecutionGraph, slab1: Event, slab2: Event) -> bool {
        // Apart from slab1, also do not query send_before(slab2, slab2)
        Self::aux_send_before(g, slab1, slab2, &mut vec![slab1, slab2])
    }

    // send_before = (porf U induced_send_before)^+
    fn aux_send_before(
        g: &ExecutionGraph,
        slab1: Event,
        // Every recursive call uses the same slab2
        slab2: Event,
        // Events s that we shouldn't query for send_before(s, slab2),
        // either because we are already (nested) in the process of answering the query
        // or because we already know that the query returns false
        seen: &mut Vec<Event>,
    ) -> bool {
        // porf <= send_before
        if g.send_label(slab2).unwrap().porf().contains(slab1) {
            return true;
        }

        // Transitivity: [slab1];send_before;[slab];send_before <= send_before
        for slab in g.all_store_iter() {
            // Only use TotalOrder sends as transitive steps
            if slab.comm() != CommunicationModel::TotalOrder {
                continue;
            }

            // Avoid recursing on send_before(s, s2), for some s that has already been tried
            if seen.contains(&slab.pos()) {
                continue;
            }

            // Check if [slab1];porf;[slab];send_before;[slab2]
            if slab.porf().contains(slab1) {
                seen.push(slab.pos());
                if Self::aux_send_before(g, slab.pos(), slab2, seen) {
                    return true;
                }
            }

            // Check if [slab1];induced_send_before;[slab];send_before;[slab2]
            // where (s1, s2) in induced_send_before iff s1 is read by a r1 that also matches s2 and
            // s2 is not read by an earlier receive r2.
            // Since there are no concurrent receives, "later" means porf.

            // slab1 is read
            let rlab1 = match g.send_label(slab1).unwrap().reader() {
                Some(rlab1) => g.recv_label(rlab1).unwrap(),
                None => continue,
            };

            // by a receive rlab1 that could also read slab
            if !rlab1.matches(slab) {
                continue;
            }

            // slab is not read, or is read by rlab s.t. (rlab1, rlab) in porf
            if slab
                .reader()
                .is_none_or(|rlab| g.in_porf(rlab1.pos(), rlab))
            {
                seen.push(slab.pos());
                if Self::aux_send_before(g, slab.pos(), slab2, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// Returns the subset of the sends s.t. they can be read from rlab after (possibly) restricting the graph to the view.
    /// The view implicitly excludes one event: View = (VectorClock, excluded Event)
    /// Lack of view implies we consider the whole graph.
    fn filter_available_sends_in_view<'a>(
        g: &'a ExecutionGraph,
        rlab: &'a RecvMsg,
        sends: impl Iterator<Item = &'a SendMsg>,
        view: Option<(&'a VectorClock, Option<Event>)>,
        check_concurrent: bool,
    ) -> impl Iterator<Item = &'a SendMsg> {
        let rpos = rlab.pos();
        sends.filter(move |&slab| {
            let spos = slab.pos();

            // exclude one event
            if view.is_some_and(|(_, excl)| excl.is_some_and(|ev| ev == spos)) {
                return false;
            }

            // send must be in the view, if it exists
            if view.is_some_and(|view| !view.0.contains(slab.pos())) {
                return false;
            }

            // *Assumption*: if this send message is monitored by the receive's thread,
            // it cannot be that it is a send message towards the monitor itself.
            if slab.is_monitored_from(&rpos.thread) {
                !slab.monitor_readers().iter().any(|&reader| {
                    // As long as it is not monitor-read by an same-thread (same-monitor)
                    // event that would remain in the view, the send can be monitor-read.
                    //
                    // Exclude the receive itself
                    reader != rpos
                        && reader.thread == rpos.thread
                        && view.is_none_or(|view| view.0.contains(reader))
                })
            } else {
                // there is no reader, or the reader is *not* in the view (we exclude the receive itself)
                match slab.reader() {
                    None => true,
                    Some(reader) => {
                        // Check for concurrent receives.
                        // We shouldn't include the receive's rf, i.e. use cached_porf.

                        // N.B. it should suffice to check only when we add rlab
                        // (it's the last event in its thread).
                        // Otherwise, we should also check whether
                        // rlab is before reader OR reader is bofore rlab
                        if check_concurrent && !rlab.cached_porf().contains(reader) {
                            println!("{}", g);
                            panic!(
                                "Detected concurrent receives: {} and {}",
                                reader,
                                rlab.pos()
                            );
                        }
                        reader == rpos || view.is_some_and(|view| !view.0.contains(reader))
                    }
                }
            }
        })
    }

    fn filter_available_sends_in_view_for_inbox<'a>(
        g: &'a ExecutionGraph,
        ilab: &'a Inbox,
        sends: impl Iterator<Item = &'a SendMsg>,
        view: Option<(&'a VectorClock, Option<Event>)>,
        check_concurrent: bool,
    ) -> impl Iterator<Item = &'a SendMsg> {
        let rpos = ilab.pos();
        sends.filter(move |&slab| {
            let spos = slab.pos();

            // Revisit view can explicitly exclude one send.
            if view.is_some_and(|(_, excl)| excl.is_some_and(|ev| ev == spos)) {
                return false;
            }

            // Keep only sends present in the chosen prefix view.
            if view.is_some_and(|view| !view.0.contains(slab.pos())) {
                return false;
            }

            match slab.reader() {
                None => true,
                Some(reader) => {
                    // Same concurrency sanity check as plain receives.
                    if check_concurrent && !ilab.cached_porf().contains(reader) {
                        println!("{}", g);
                        panic!(
                            "Detected concurrent receives: {} and {}",
                            reader,
                            ilab.pos()
                        );
                    }
                    // Keep send if it is still unread in the view, or already read by this inbox.
                    reader == rpos || view.is_some_and(|view| !view.0.contains(reader))
                }
            }
        })
    }

    /// Returns whether the send has no sb-predecessor (porf-predecessors if flag is set) among the rest sends
    fn is_sb_miminal(send: &SendMsg, sends: &[&SendMsg], porf_override: bool) -> bool {
        let view = if porf_override {
            send.porf()
        } else {
            send.sb()
        };
        !sends.iter().any(|&e| view.contains(e.pos()))
    }

    /// Keeps the sb-minimals (porf-minimals is flag is set) among the (*stamp-ordered*) sends
    fn retain_sb_minimals<'a>(
        sends: impl Iterator<Item = &'a SendMsg>,
        porf_override: bool,
    ) -> Vec<&'a SendMsg> {
        // Among sends, stamp order respects porf, which includes sb for any model apart from TotalOrder.
        // Therefore, we can detect overwrites in a single forward pass.
        // Note: Amend this is we end up incrementally checking TotalOrder consistency as well.

        let mut sb_min = Vec::new();
        sends.for_each(|s| {
            if Self::is_sb_miminal(s, &sb_min, porf_override) {
                sb_min.push(s)
            }
        });
        sb_min
    }

    /// Returns the coherent matching stores that can be consistently read by recv
    /// when restricting the graph to the view (we exclude one event from the view).
    fn coherent_rfs_in_view(
        &self,
        g: &ExecutionGraph,
        // an optional view, excluding one event (a newly added send)
        view: Option<(&VectorClock, Option<Event>)>,
        recv: &RecvMsg,
        porf_override: bool,
        check_concurrent: bool,
    ) -> Vec<Event> {
        // Sends that the receive can read from
        let sends = g
            .matching_stores(recv.recv_loc())
            // filter-out WakeMsg in our porf prefix: the respective futures were cancelled
            .filter(|&s| !s.is_cancelled_wrt(recv.as_event_label()));

        // Keep those that will exist and be unread after the revisit, checking
        // for concurrent receives.
        let rfs = Self::filter_available_sends_in_view(g, recv, sends, view, check_concurrent);

        // Optional optimization for NoOrder
        let mut rfs: Vec<Event> = if recv.comm() != CommunicationModel::NoOrder {
            // *Assuming* there are no concurrent receives,
            // all existing matching receives are porf-before the current receives.
            // Therefore the consistent sends are exactly the sb-minimal ones.
            Self::retain_sb_minimals(rfs, porf_override)
                .iter()
                .map(|lab| lab.pos())
                .collect()
        } else {
            rfs.map(|lab| lab.pos()).collect()
        };

        // Return them in an arbitrary but fixed order that does
        // *not* depend on the stamps.

        // This is the single place that uses Event's Ord constraint,
        // and *depends* on ThreadId's Ord implementation being stable
        // across executions (i.e. the underlying opaque_id not changing).
        // If this becomes a problem, one can recover a stable, deterministic,
        // ordering on ThreadId's from the execution graph:
        // consider the restriction to Create/Begin events, and use
        // e.g. a dfs pre-order for ordering TheadIds (and by extension, Events).
        rfs.sort();
        rfs
    }

    fn coherent_inbox_rfs_in_view(
        &self,
        g: &ExecutionGraph,
        view: Option<(&VectorClock, Option<Event>)>,
        inbox: &Inbox,
        check_concurrent: bool,
    ) -> Vec<Event> {
        // Candidate sends that match the inbox location/predicate.
        let sends = g.matching_stores(inbox.recv_loc());

        let rfs =
            Self::filter_available_sends_in_view_for_inbox(g, inbox, sends, view, check_concurrent);

        let mut rfs: Vec<Event> = if inbox.comm() != CommunicationModel::NoOrder {
            // Respect the channel's delivery model, mirroring recv behavior.
            Self::retain_sb_minimals(rfs, false)
                .iter()
                .map(|lab| lab.pos())
                .collect()
        } else {
            rfs.map(|lab| lab.pos()).collect()
        };

        // Stable ordering for canonical subset derivation.
        rfs.sort();
        rfs
    }

    /// Calculates and populates necessary views for pos
    pub(crate) fn calc_views(&self, g: &mut ExecutionGraph, pos: Event) {
        if pos.index == 0 {
            let mut empty = VectorClock::new();
            empty.set_tid(pos.thread);
            g.label_mut(pos).set_porf_cache(empty.clone());
            g.label_mut(pos).set_posw_cache(empty.clone());
            return;
        }

        let prev = pos.prev();
        let mut porf = g.label(prev).cached_porf().clone();
        let mut posw = g.label(prev).cached_posw().clone();

        porf.update_idx(pos);
        posw.update_idx(pos);

        // Cached views do not include prev's direct dependencies (rf/TCreate/TEnd).
        // Adjust them to do so.

        // rf dependencies
        if let Some(rlab) = g.recv_label(prev) {
            if let Some(rf) = rlab.rf() {
                porf.update(g.label(rf).cached_porf());
                match rlab.comm() {
                    CommunicationModel::TotalOrder => { /* empty */ }
                    // posw does *not* include rf from TotalOrder events
                    _ => posw.update(g.label(rf).cached_posw()),
                }
            }
        }
        if let Some(ilab) = g.inbox_label(prev) {
            if let Some(rfs) = ilab.rfs() {
                for rf in rfs {
                    porf.update(g.label(rf).cached_porf());
                    match ilab.comm() {
                        CommunicationModel::TotalOrder => { /* empty */ }
                        // posw does *not* include rf from TotalOrder events
                        _ => posw.update(g.label(rf).cached_posw()),
                    }
                }
            }
        }

        // TCreate dependencies
        if let LabelEnum::Begin(blab) = g.label(prev) {
            if let Some(parent) = blab.parent() {
                porf.update(g.label(parent).cached_porf());
                // Create -> Begin contributes to sw as well
                posw.update(g.label(parent).cached_posw());
            }
        }

        // TEnd dependencies
        if let LabelEnum::TJoin(jlab) = g.label(prev) {
            porf.update(g.thread_last(jlab.cid()).unwrap().cached_porf());
            // Join -> End contributes to sw as well
            posw.update(g.thread_last(jlab.cid()).unwrap().cached_posw());
        }

        // Set send's sb view
        if let Some(slab) = g.send_label_mut(pos) {
            let mut sb = VectorClock::new();
            match slab.comm() {
                CommunicationModel::NoOrder => { /* empty */ }
                // Local: just include yourself (and po-predecessors)
                CommunicationModel::LocalOrder => sb.set(pos),
                CommunicationModel::CausalOrder => sb.update(&posw),
                // Treat Total similar to Causal, and check full consistency at the end
                CommunicationModel::TotalOrder => sb.update(&porf),
            }
            slab.set_sb(sb);
        }

        // Cache the views
        g.label_mut(pos).set_porf_cache(porf);
        g.label_mut(pos).set_posw_cache(posw);
    }

    pub(crate) fn is_consistent(&self, g: &ExecutionGraph) -> bool {
        for slab1 in g.all_store_iter() {
            if slab1.comm() != CommunicationModel::TotalOrder {
                continue;
            }
            for slab2 in g.all_store_iter() {
                if slab2.comm() != CommunicationModel::TotalOrder {
                    continue;
                }

                let s1 = slab1.pos();
                let s2 = slab2.pos();

                if s1 == s2 {
                    continue;
                }

                // For each pair (s1, s2) of sends with TotalOrder

                // s.t. s2 is read by a receive r2
                let r2 = match slab2.reader() {
                    None => continue,
                    Some(r2) => r2,
                };
                // that could have also read s1,
                if !g.recv_label(r2).unwrap().matches(slab1) {
                    continue;
                }

                // if s1 is read by a later (wrt r2) receive r1,
                if slab1.reader().is_some_and(|r1| g.in_porf(r1, r2)) {
                    continue;
                }

                // and s1 is causally_before s2,
                if self.send_before(g, s1, s2) {
                    // then the execution is inconsistent
                    return false;
                    // because s1 is ordered both
                    // - before s2 (send_before), and
                    // - after s2 (via their respective receives)
                }

                // N.B. We assumed that there are no concurrent receives
                // to reduce "r1 is earlier than r2" to "(r1, r2) in porf".
                // Otherwise, we need to explicitly enumerate linearizations
                // to judge consistency.
            }
        }
        true
    }

    /// Returns whether an affected receive is maximal during a revisit
    pub(crate) fn reads_tiebreaker(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        rev: &Revisit,
        porf_override: bool,
    ) -> bool {
        let (view, exclude) = match &rev.rev {
            crate::revisit::RevisitPlacement::Default(send) => {
                // rlab is not in the prefix of the revisitor
                assert!(!g.send_label(*send).unwrap().porf().contains(rlab.pos()));
                (
                    g.revisit_view(&Revisit::new(rlab.pos(), *send)),
                    Some(*send),
                )
            }
            crate::revisit::RevisitPlacement::Inbox(sends) => {
                let rev_inbox = Revisit::new_inbox(rlab.pos(), sends.clone());
                (g.revisit_view(&rev_inbox), None)
            }
        };
        // rlab is stamp greater or equal that revisitee's stamp
        assert!(rlab.stamp() >= g.label(rev.pos).stamp());

        // Nonblocking receives are maximal only when they timeout
        if rlab.is_non_blocking() {
            return rlab.rf().is_none();
        }

        // First (non-revisit) is the maximal one.
        rlab.rf().unwrap()
            == self.coherent_rfs_in_view(g, Some((&view, exclude)), rlab, porf_override, false)[0]
    }

    pub(crate) fn inbox_reads_tiebreaker(
        &self,
        g: &ExecutionGraph,
        ilab: &Inbox,
        rev: &Revisit,
    ) -> bool {
        // Non-blocking inbox is maximal when it currently takes the empty subset.
        if ilab.is_non_blocking() {
            return ilab.rfs().map_or(true, |rfs| rfs.is_empty());
        }

        let Some(current) = ilab.rfs() else {
            return false;
        };

        let view = g.revisit_view(rev);
        let exclude = match &rev.rev {
            // For recv-style revisit placement, remove the newly inserted send.
            crate::revisit::RevisitPlacement::Default(ev) => Some(*ev),
            // Inbox placement already names the whole candidate set in the revisit view.
            crate::revisit::RevisitPlacement::Inbox(_) => None,
        };

        let mut cands = self.coherent_inbox_rfs_in_view(g, Some((&view, exclude)), ilab, false);

        Consistency::normalize_event_set(&mut cands);

        // Canonical subset in the revisit view: first `min` coherent sends.
        // Maximality requires the current inbox read to be exactly this subset.
        let limit = ilab.min().min(cands.len());
        let canonical: Vec<Event> = cands.into_iter().take(limit).collect();

        current == canonical
    }

    /// Returns the rf options for rlab, with the first being the non-revisit rf step
    pub(crate) fn rfs(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        porf_override: bool,
    ) -> Vec<Event> {
        self.coherent_rfs_in_view(g, None, rlab, porf_override, true)
    }

    pub(crate) fn inbox_rfs(&self, g: &ExecutionGraph, ilab: &Inbox) -> Vec<Event> {
        // Deterministic coherent inbox candidates for base execution / canonical subset.
        self.coherent_inbox_rfs_in_view(g, None, ilab, true)
    }

    /// Returns whether the resulting execution would be consistent
    ///
    /// Assumes that rlab is not porf-before slab
    pub(crate) fn is_revisit_consistent(
        &self,
        g: &ExecutionGraph,
        rlab: &RecvMsg,
        slab: &SendMsg,
        porf_override: bool,
    ) -> bool {
        assert!(rlab.matches(slab));

        let com = rlab.comm();

        // Optional optimization for NoOrder
        if com == CommunicationModel::NoOrder {
            return true;
        }

        // *Assuming* there are no concurrent receives
        // (which implies that the model is prefix-closed)
        // it suffices to check that slab is not overwritten in the resulting execution

        let rpos = rlab.pos();
        let spos = slab.pos();

        // We disregard the various communication models and check consistency
        // as if everything was CausalOrder.
        let send_sb = if porf_override {
            slab.porf()
        } else {
            slab.sb()
        };

        let view = g.revisit_view(&Revisit::new(rpos, spos));

        let sends = g.matching_stores(rlab.recv_loc()).filter(|&lab| {
            let pos = lab.pos();
            pos != spos && send_sb.contains(pos)
        });

        // if any of them, apart from slab, could be read by rlab after the revisit, then the execution is inconsistent
        let overwritten =
            Self::filter_available_sends_in_view(g, rlab, sends, Some((&view, Some(spos))), false)
                .next()
                .is_none();
        overwritten
    }

    /// Inbox consistency for set semantics: order does not matter.
    /// True iff the chosen subset satisfies bounds and every send is valid/available.
    pub(crate) fn is_revisit_consistent_inbox(
        &self,
        g: &ExecutionGraph,
        inbox: &Inbox,
        sends: &Vec<Event>,
    ) -> bool {
        if let Some(max) = inbox.max() {
            if sends.len() > max {
                return false;
            }
        }
        if sends.len() < inbox.min() {
            return false;
        }

        // Each chosen send must exist, match, be undropped, and not already read by another receiver.
        for &s in sends {
            let Some(slab) = g.send_label(s) else {
                return false;
            };
            if slab.is_dropped() || !inbox.matches(slab) {
                return false;
            }
            if slab.reader().is_some_and(|r| r != inbox.pos()) {
                return false;
            }
        }
        true
    }

    pub(crate) fn normalize_event_set(events: &mut Vec<Event>) {
        // Canonicalize subset representation before comparisons/ownership checks.
        events.sort();
        events.dedup();
    }

    // the owner of a set of (send) events is the newer one (from which backward revisit are generated)
    pub(crate) fn inbox_owner(g: &ExecutionGraph, events: &[Event]) -> Option<Event> {
        events.iter().copied().max_by_key(|e| g.label(*e).stamp())
    }
}
