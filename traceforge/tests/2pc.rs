//! Interval-based two-phase commit.
//!
//! Roles:
//!   - A *coordinator* owns a single transaction, abstracted as an interval of
//!     time. It sends the interval as a request to every participant, waits for
//!     all answers, and decides to COMMIT iff every answer is `true` (otherwise
//!     ABORT). It then broadcasts the decision (which carries the interval).
//!   - A *participant* keeps a set of intervals it currently holds. On a
//!     request it answers `true` iff the requested interval does not intersect
//!     any held interval, and in that case also adds the interval to its set.
//!     On an ABORT decision it removes the corresponding interval again; on
//!     COMMIT it keeps it.
//!
//! Messaging uses the top-level `traceforge::send_msg`/`recv_msg_block` API
//! addressed by `ThreadId`. Each thread only ever receives a single message
//! type, so the typed blocking receive is unambiguous.
//!
//! Safety property checked: no two transactions whose intervals intersect can
//! both commit (isolation).

use std::collections::HashMap;
use traceforge::thread::{self, ThreadId};
use traceforge::*;

/// A closed interval [lo, hi] of time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interval {
    lo: u32,
    hi: u32,
}

impl Interval {
    fn new(lo: u32, hi: u32) -> Self {
        Interval { lo, hi }
    }

    /// Two closed intervals intersect iff neither lies entirely before the
    /// other.
    fn intersects(&self, other: &Interval) -> bool {
        self.lo <= other.hi && other.lo <= self.hi
    }
}

/// Messages sent to a participant.
#[derive(Clone, Debug, PartialEq)]
enum ToParticipant {
    /// Phase 1: the coordinator `coord` proposes transaction `txn` over `interval`.
    Request {
        txn: u32,
        interval: Interval,
        coord: ThreadId,
    },
    /// Phase 2: the outcome of transaction `txn` (the interval is carried again,
    /// as specified). `commit == false` means abort.
    Decision {
        txn: u32,
        interval: Interval,
        commit: bool,
    },
}

/// A participant's answer, sent back to the coordinator.
#[derive(Clone, Debug, PartialEq)]
struct Vote {
    txn: u32,
    participant: u32,
    ok: bool,
}

/// A coordinator's final outcome, reported to the driver.
#[derive(Clone, Debug, PartialEq)]
struct Outcome {
    txn: u32,
    interval: Interval,
    committed: bool,
}

/// Participant `me` serves requests and decisions forever. Once all
/// transactions are done there are no more messages, so the final
/// `recv_msg_block` simply blocks — that is fine; the driver has already
/// gathered every outcome and checked the property by then.
fn participant(me: u32) {
    // Intervals currently held, keyed by the transaction that placed them.
    let mut held: HashMap<u32, Interval> = HashMap::new();

    loop {
        match recv_msg_block::<ToParticipant>() {
            ToParticipant::Request {
                txn,
                interval,
                coord,
            } => {
                let conflict = held.values().any(|h| h.intersects(&interval));
                let ok = !conflict;
                if ok {
                    // Tentatively add the interval; removed again on abort.
                    held.insert(txn, interval);
                }
                send_msg(
                    coord,
                    Vote {
                        txn,
                        participant: me,
                        ok,
                    },
                );
            }
            ToParticipant::Decision {
                txn,
                interval: _,
                commit,
            } => {
                if !commit {
                    // Undo the tentative add (no-op if we voted `false` and
                    // never added it).
                    held.remove(&txn);
                }
            }
        }
    }
}

/// Coordinator for transaction `txn` over `interval`. Broadcasts the request to
/// all `participants`, waits for all answers, broadcasts the decision, and
/// reports the outcome to `driver`.
fn coordinator(txn: u32, interval: Interval, participants: Vec<ThreadId>, driver: ThreadId) {
    let me = thread::current_id();

    // Phase 1: request.
    for &p in &participants {
        send_msg(p, ToParticipant::Request { txn, interval, coord: me });
    }

    // Collect every answer (do not short-circuit, so all votes are consumed).
    let mut commit = true;
    for _ in 0..participants.len() {
        let v = recv_msg_block::<Vote>();
        if !v.ok {
            commit = false;
        }
    }

    // Phase 2: decision (carries the interval again).
    for &p in &participants {
        send_msg(p, ToParticipant::Decision { txn, interval, commit });
    }

    send_msg(driver, Outcome { txn, interval, committed: commit });
}

/// Spawns `num_participants` participants and one coordinator per interval in
/// `intervals`, then collects the outcomes and asserts the isolation property:
/// no two committed transactions have intersecting intervals.
fn driver(intervals: Vec<Interval>, num_participants: usize) {
    let num_coordinators = intervals.len();
    let driver_id = thread::current_id();

    // Participants first, so coordinators can be given their thread ids.
    let mut participants = Vec::with_capacity(num_participants);
    for i in 0..num_participants {
        let h = thread::Builder::new()
            .name(format!("participant {}", i))
            .spawn(move || participant(i as u32))
            .unwrap();
        participants.push(h.thread().id());
    }

    // One coordinator per transaction.
    for (txn, interval) in intervals.into_iter().enumerate() {
        let participants = participants.clone();
        thread::Builder::new()
            .name(format!("coordinator {}", txn))
            .spawn(move || coordinator(txn as u32, interval, participants, driver_id))
            .unwrap();
    }

    // Collect outcomes and check isolation.
    let mut committed: Vec<Interval> = Vec::new();
    for _ in 0..num_coordinators {
        let o = recv_msg_block::<Outcome>();
        if o.committed {
            committed.push(o.interval);
        }
    }
    for a in 0..committed.len() {
        for b in (a + 1)..committed.len() {
            traceforge::assert(!committed[a].intersects(&committed[b]));
        }
    }
}

#[test]
fn two_disjoint_intervals() {
    // Disjoint transactions: both can commit; isolation trivially holds.
    let stats = verify(
        Config::builder().with_print_all_graphs_dot("my_graphs").build(),
        || driver(vec![Interval::new(0, 2), Interval::new(3, 5)], 2),
    );
    println!("two_disjoint_intervals: execs={} block={}", stats.execs, stats.block);
}

#[test]
fn two_overlapping_intervals() {
    // Intersecting transactions: at most one may commit.
    let stats = verify(
        Config::builder().build(),
        || driver(vec![Interval::new(0, 3), Interval::new(2, 5)], 2),
    );
    println!("two_overlapping_intervals: execs={} block={}", stats.execs, stats.block);
}

#[test]
fn three_mixed_intervals() {
    // [0,2] and [5,7] are disjoint from each other; [1,6] overlaps both.
    let stats = verify(
        Config::builder().build(),
        || {
            driver(
                vec![Interval::new(0, 2), Interval::new(5, 7), Interval::new(1, 6)],
                2,
            )
        },
    );
    println!("three_mixed_intervals: execs={} block={}", stats.execs, stats.block);
}
