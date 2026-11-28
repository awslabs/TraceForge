//! One interleaving of the execution.
//!
//! A topological order of all events, hidden ones included: program order, send before receive,
//! spawn before the child's first event, end before join. At each step it takes:
//! 1. the next event of the current thread (the last visible thread that ran);
//! 2. else, the hidden events that this event waits for (e.g. an atomic's thread serving the
//!    request), so that unrelated steps do not cut the thread's flow;
//! 3. else, the smallest stamp, i.e. the order in which the checker added the events.
//!
//! A thread's final `BLOCKED` comes last.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::event::Event;
use crate::event_label::LabelEnum;
use crate::exec_graph::ExecutionGraph;
use crate::thread::ThreadId;

use super::analysis::Analysis;

/// How many events the search of rule 2 visits at most, at each step.
const SEARCH_LIMIT: usize = 10_000;

/// The indexes of `analysis.lines`, in the order of the interleaving.
pub(super) fn interleaving(g: &ExecutionGraph, analysis: &Analysis) -> Vec<usize> {
    let threads: Vec<_> = g.threads.iter().collect();
    let slot: HashMap<ThreadId, usize> = threads
        .iter()
        .enumerate()
        .map(|(i, t)| (t.tid, i))
        .collect();

    // Edges other than program order, in both directions, and how many enter each event.
    let mut succ: HashMap<Event, Vec<Event>> = HashMap::new();
    let mut pred: HashMap<Event, Vec<Event>> = HashMap::new();
    let mut indeg: HashMap<Event, usize> = HashMap::new();
    let mut edge = |a: Event, b: Event| {
        if g.contains(a) && g.contains(b) {
            succ.entry(a).or_default().push(b);
            pred.entry(b).or_default().push(a);
            *indeg.entry(b).or_default() += 1;
        }
    };
    for t in &threads {
        for lab in &t.labels {
            let pos = lab.pos();
            match lab {
                LabelEnum::RecvMsg(r) => {
                    if let Some(s) = r.rf() {
                        edge(s, pos);
                    }
                }
                LabelEnum::Inbox(i) => {
                    for s in i.rfs().unwrap_or_default() {
                        edge(s, pos);
                    }
                }
                LabelEnum::TCreate(c) => edge(pos, Event::new(c.cid(), 0)),
                LabelEnum::TJoin(j) => {
                    if let Some(end) = g.get_thr_opt(&j.cid()).and_then(|c| c.labels.last()) {
                        edge(end.pos(), pos);
                    }
                }
                _ => {}
            }
        }
    }

    let hidden: Vec<bool> = threads
        .iter()
        .map(|t| analysis.thread(t.tid).is_none_or(|v| v.hidden))
        .collect();
    let total: usize = threads.iter().map(|t| t.labels.len()).sum();
    let mut next = vec![0usize; threads.len()];
    let mut cur: Option<usize> = None;
    let mut schedule: Vec<Event> = Vec::with_capacity(total);

    let label = |e: Event| -> Option<&LabelEnum> {
        threads[*slot.get(&e.thread)?].labels.get(e.index as usize)
    };
    let scheduled = |e: Event, next: &[usize]| {
        slot.get(&e.thread)
            .is_none_or(|&i| (e.index as usize) < next[i])
    };
    let ready = |e: Event, next: &[usize], indeg: &HashMap<Event, usize>| {
        slot.get(&e.thread)
            .is_some_and(|&i| next[i] == e.index as usize)
            && indeg.get(&e).copied().unwrap_or(0) == 0
    };
    let final_block = |lab: &LabelEnum| {
        matches!(lab, LabelEnum::Block(_))
            && slot
                .get(&lab.thread())
                .is_some_and(|&i| lab.index() as usize + 1 == threads[i].labels.len())
    };
    let stamp = |lab: &LabelEnum| {
        if lab.stamped() {
            lab.stamp()
        } else {
            usize::MAX
        }
    };
    let next_of = |i: usize, next: &[usize]| threads[i].labels.get(next[i]);

    while schedule.len() < total {
        let mut pick: Option<Event> = None;
        if let Some(c) = cur {
            if let Some(lab) = next_of(c, &next).filter(|l| !final_block(l)) {
                let e = lab.pos();
                if ready(e, &next, &indeg) {
                    pick = Some(e); // 1. stay on the current thread
                } else {
                    // 2. the hidden events the current thread waits for
                    let mut seen: HashSet<Event> = HashSet::new();
                    let mut queue: VecDeque<Event> = VecDeque::from([e]);
                    let mut best: Option<(usize, Event)> = None;
                    while let Some(x) = queue.pop_front() {
                        if seen.len() >= SEARCH_LIMIT || !seen.insert(x) {
                            continue;
                        }
                        let mut before: Vec<Event> = pred.get(&x).cloned().unwrap_or_default();
                        if x.index > 0 {
                            before.push(Event::new(x.thread, x.index - 1));
                        }
                        for p in before {
                            if scheduled(p, &next)
                                || !slot.get(&p.thread).is_some_and(|&i| hidden[i])
                            {
                                continue;
                            }
                            if ready(p, &next, &indeg) {
                                let s = label(p).map(stamp).unwrap_or(usize::MAX);
                                if best.is_none_or(|(bs, _)| s < bs) {
                                    best = Some((s, p));
                                }
                            } else {
                                queue.push_back(p);
                            }
                        }
                    }
                    pick = best.map(|(_, p)| p);
                }
            }
        }
        if pick.is_none() {
            // 3. the smallest stamp; final blocks last
            let candidates = |finals: bool| {
                (0..threads.len())
                    .filter_map(|i| next_of(i, &next))
                    .filter(|l| ready(l.pos(), &next, &indeg) && (finals || !final_block(l)))
                    .map(|l| (stamp(l), l.pos()))
                    .min()
                    .map(|(_, e)| e)
            };
            pick = candidates(false).or_else(|| candidates(true));
        }
        let Some(e) = pick else {
            break; // cannot happen in a consistent graph
        };
        let i = slot[&e.thread];
        next[i] += 1;
        schedule.push(e);
        if let Some(out) = succ.get(&e) {
            for b in out {
                if let Some(d) = indeg.get_mut(b) {
                    *d -= 1;
                }
            }
        }
        if !hidden[i] {
            cur = Some(i);
        }
    }

    // Place each line at its anchor.
    let mut at: HashMap<Event, Vec<usize>> = HashMap::new();
    for (li, l) in analysis.lines.iter().enumerate() {
        at.entry(l.anchor).or_default().push(li);
    }
    let mut order = Vec::with_capacity(analysis.lines.len());
    for e in &schedule {
        if let Some(ls) = at.remove(e) {
            order.extend(ls);
        }
    }
    // Lines whose anchor was not reached (only if the graph is inconsistent).
    let mut rest: Vec<usize> = at.into_values().flatten().collect();
    rest.sort();
    order.extend(rest);
    order
}
