//! What replay validation detects when a program is not re-entrant, and what it
//! does not.
//!
//! `verify` runs the program closure once per execution and replays the prefix it
//! has already recorded. A program that carries state across executions can
//! therefore present a different event at a replayed position than the one the
//! graph recorded. **Most** replayed event handlers compare the event against the
//! recorded label by calling `ExecutionGraph::validate_replay_event`
//! (`LabelEnum::compare_for_replay`), and a mismatch panics with *"Incorrect
//! TraceForge Program. TraceForge programs must be deterministic..."*. Not all of
//! them do: the replay branch for `sample()` returns the recorded value without
//! validating it, and a spawn's thread id is resolved — and can panic by itself —
//! before any comparison happens. So neither "every replayed event is compared"
//! nor "every replay panic comes from the comparison" is true.
//!
//! What that comparison does cover is **part of the shape** of an event and **none
//! of its data**. "Part of" is doing real work there: a send's destination is shape
//! under any reading of the word, and it is not compared. The tests below measure
//! both halves:
//!
//! * the loud half — four kinds of divergence that panic, the event-kind one in
//!   both directions, each paired with a control that keeps the program stateful
//!   but removes the divergence, so the panic is attributable to the divergence and
//!   not to the program's shape (a `#[should_panic]` test otherwise passes on any
//!   matching panic);
//! * the silent half — five divergences that complete with no diagnostic: three
//!   the comparison does not look at, and two it looks at behind a guard that
//!   never opens;
//! * and the consequence: a program whose only flaw is one of those silent
//!   divergences can make `verify` *suppress a failure* — one it does report for
//!   the same program with its first-entry behaviour held stable.
//!
//! That last pair is one witness rather than a theorem, and it is worth being exact
//! about what it shows. A program that breaks re-entrancy has no single state space
//! for the engine to be complete over, so the comparison is against a re-entrant
//! program, not a claim about valid ones. What it shows is that nothing tells you
//! which of the two you ran.
//!
//! All tests here use `ConsType::FIFO` and a captured `AtomicUsize` as the
//! cross-execution state. The counter's value `k` is the entry number: `k == 0` is
//! the first entry, whose events are the ones the graph records.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use traceforge::thread::main_thread_id;
use traceforge::{
    nondet, recv_msg, recv_msg_block, recv_tagged_msg_block, send_msg, send_tagged_msg, thread,
    Config, ConsType, Nondet,
};

fn cfg() -> Config {
    Config::builder().with_cons_type(ConsType::FIFO).build()
}

fn named<F: FnOnce() + Send + 'static>(n: &str, f: F) -> thread::JoinHandle<()> {
    thread::Builder::new().name(n.to_string()).spawn(f).unwrap()
}

// ===========================================================================
// The loud half: divergences the replay comparison catches.
// Each test is followed by its control.
// ===========================================================================

/// A send's **tag** differs at a replayed position: the first entry tags with `1`,
/// later entries with `2`.
///
/// Detected by `compare_for_replay`'s `SendMsg` arm through
/// `slocs_are_compatible`, which compares the sender thread id and the tag. That
/// check is not gated on anything, unlike the value comparison in the same arm.
///
/// Detail line:
/// `Expected to send message with tag=Some([1]) from t1 but actually sent with=Some([2]) from t1`
#[test]
#[should_panic(expected = "TraceForge programs must be deterministic")]
fn send_tag_divergence_panics() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let tag: u32 = if k == 0 { 1 } else { 2 };
        let m = main_thread_id();
        let _a = named("a", move || send_tagged_msg(m, tag, 1u64));
        let _b = named("b", move || send_tagged_msg(m, 1, 2u64));
        let _x: u64 = recv_tagged_msg_block(|_, _t| true);
        let _y: u64 = recv_tagged_msg_block(|_, _t| true);
    });
}

/// Control for [`send_tag_divergence_panics`]: the counter is still read, so the
/// closure is still stateful, but the tag no longer changes. No panic.
// The two arms are deliberately identical: that is what makes this a control.
#[allow(clippy::if_same_then_else)]
#[test]
fn send_tag_control_is_accepted() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let tag: u32 = if k == 0 { 1 } else { 1 };
        let m = main_thread_id();
        let _a = named("a", move || send_tagged_msg(m, tag, 1u64));
        let _b = named("b", move || send_tagged_msg(m, 1, 2u64));
        let _x: u64 = recv_tagged_msg_block(|_, _t| true);
        let _y: u64 = recv_tagged_msg_block(|_, _t| true);
    });
}

/// A `nondet` **range** differs at a replayed position: `0..2` on the first entry,
/// `0..3` later.
///
/// Detected by `compare_for_replay`'s `Choice` arm, which compares the range.
///
/// Detail line: `Expected nondet over range 0..=1 but got 0..=2`
#[test]
#[should_panic(expected = "TraceForge programs must be deterministic")]
fn nondet_range_divergence_panics() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let hi = if k == 0 { 2usize } else { 3usize };
        let _pick = (0..hi).nondet();
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });
}

/// Control for [`nondet_range_divergence_panics`]: stateful, constant range. No
/// panic.
// The two arms are deliberately identical: that is what makes this a control.
#[allow(clippy::if_same_then_else)]
#[test]
fn nondet_range_control_is_accepted() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let hi = if k == 0 { 2usize } else { 2usize };
        let _pick = (0..hi).nondet();
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });
}

/// A spawned thread's **name** differs at a replayed position.
///
/// Detected by `compare_for_replay`'s `TCreate` arm, which compares the name, the
/// daemon flag and the symmetric thread id.
///
/// Detail line:
/// `Expected the thread to be named Some("a") but it was named Some("DIVERGED")`
#[test]
#[should_panic(expected = "TraceForge programs must be deterministic")]
fn spawned_thread_name_divergence_panics() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let n = if k == 0 { "a" } else { "DIVERGED" };
        let m = main_thread_id();
        let _a = named(n, move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });
}

/// Control for [`spawned_thread_name_divergence_panics`]: stateful, constant name.
/// No panic.
// The two arms are deliberately identical: that is what makes this a control.
#[allow(clippy::if_same_then_else)]
#[test]
fn spawned_thread_name_control_is_accepted() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let n = if k == 0 { "a" } else { "a" };
        let m = main_thread_id();
        let _a = named(n, move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });
}

/// A different **kind of event** at a replayed position: the first entry tosses a
/// coin at main's event 2, later entries spawn a thread there.
///
/// This direction is **not** detected by `compare_for_replay`. A spawn resolves
/// its thread id through `ExecutionGraph::tid_for_spawn`, which panics by itself
/// when the recorded label at the spawn position is not a `TCreate`, before any
/// replay comparison happens. The message is the same, so the panic a user sees is
/// not always the validator's.
///
/// Detail line:
/// `Expected spawn event at Event { thread: ThreadId { opaque_id: 0 }, index: 2 } but have (t0, 2): NONDET true`
#[test]
#[should_panic(expected = "TraceForge programs must be deterministic")]
fn spawn_where_a_nondet_was_recorded_panics() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let _b = named("b", move || send_msg(m, 2u64));
        if k == 0 {
            // first entry: main's event 2 is a coin toss
            let _ = nondet();
        } else {
            // later entries: main's event 2 is a thread spawn instead
            let _a = named("a", move || send_msg(m, 1u64));
        }
        let _x: u64 = recv_msg_block();
    });
}

/// The same divergence the other way round: the first entry spawns at main's event
/// 2, later entries toss a coin there.
///
/// This direction *is* the replay comparison's doing: no arm of
/// `compare_for_replay` matches a recorded `TCreate` against an actual `CToss`, so
/// it falls through to the catch-all that reports the two actions by name.
/// Together with [`spawn_where_a_nondet_was_recorded_panics`] this shows the two
/// distinct detection paths behind one message.
///
/// Detail line:
/// `At this point in the thread, it should have spawned another thread/future but it called nondet() -> bool instead.`
#[test]
#[should_panic(expected = "TraceForge programs must be deterministic")]
fn nondet_where_a_spawn_was_recorded_panics() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let _b = named("b", move || send_msg(m, 2u64));
        if k == 0 {
            let _a = named("a", move || send_msg(m, 1u64));
        } else {
            let _ = nondet();
        }
        let _x: u64 = recv_msg_block();
    });
}

/// Control for the two event-kind tests: stateful, same kind of event at main's
/// event 2 on every entry. No panic.
// The two arms are deliberately identical: that is what makes this a control.
#[allow(clippy::if_same_then_else)]
#[test]
fn event_kind_control_is_accepted() {
    let c = Arc::new(AtomicUsize::new(0));
    traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let _b = named("b", move || send_msg(m, 2u64));
        if k == 0 {
            let _ = nondet();
        } else {
            let _ = nondet();
        }
        let _x: u64 = recv_msg_block();
    });
}

// ===========================================================================
// The silent half: divergences the replay comparison does not cover.
// Their positive control is the loud half above: the same harness, the same
// program shape, and the same kind of cross-execution state do produce a
// diagnostic when the diverging field is one that is compared. So "no panic"
// here is a property of the field, not of the harness.
// ===========================================================================

/// A send's **value** differs at a replayed position. No diagnostic.
///
/// `compare_for_replay` does compare a `SendMsg` value, but only
/// `if !s.val().is_pending()` — and `initialize_for_execution` calls
/// `set_pending()` on every send value in the graph before each execution, with no
/// condition. The guard therefore never opens: there is no execution in which a
/// recorded send value reaches the comparison settled.
///
/// The recorded value is not merely unchecked, it is overwritten: once validation
/// passes, `Must::process_event` calls `recover_lost_data`, whose `SendMsg` case is
/// `self.val = other.val` — the value from the label the re-entered program just
/// produced. The graph then holds the diverged value at the replayed position.
/// That is observable from outside the crate, awkwardly, by running with
/// `Config::with_dot_out` and `with_verbose(1)` and reading the dumped graph: the
/// `SEND` node at the replayed position carries `99`, not the `1` the first entry
/// sent. This test does not assert on that dump, because its node labels are
/// `Debug` renderings of internal types and would break on unrelated output
/// changes; [`stateful_program_suppresses_a_failure_the_reference_reports`] below measures the
/// consequence instead.
#[test]
fn send_value_divergence_is_silent() {
    let c = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let v: u64 = if k == 0 { 1 } else { 99 };
        s.lock().unwrap().push(v);
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, v));
        let _b = named("b", move || send_msg(m, 7u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });
    let sent = seen.lock().unwrap().clone();
    assert!(
        stats.execs > 1,
        "the program must be re-entered for this to measure anything; execs={}",
        stats.execs
    );
    assert!(
        sent.len() > 1 && sent[0] == 1 && sent[1..].contains(&99),
        "a later entry must send a different value - that is the divergence; sent: {sent:?}"
    );
}

/// A send's **destination** differs at a replayed position: on the first entry `a`
/// sends to the main thread, on later entries it sends to a different thread. No
/// diagnostic.
///
/// Nothing in `compare_for_replay` looks at a send's destination. The `SendMsg` arm
/// delegates to `slocs_are_compatible`, which is
/// `loc1.sender_tid == loc2.sender_tid && loc1.tag == loc2.tag` — the recipient
/// (`SendLoc::loc`) is not among the fields it compares, because that field is
/// `#[serde(skip)]` and so is absent in a graph that was deserialised.
///
/// Nor is it restored afterwards. `Must::recover_lost_data` restores a `SendMsg`'s
/// recipient only in counterexample-replay mode, by calling `recover_lost`; in the
/// search that `verify` drives it takes the other branch, `recover_val`, whose body
/// is `self.val = other.val`. So the graph keeps describing a send to a recipient
/// the program is no longer sending to, and neither the comparison nor the recovery
/// notices.
///
/// This is a change in the communication topology rather than in a payload, and it
/// is the least payload-like of the silent classes.
///
/// The test observes that the recipient really did change between entries, that the
/// program was re-entered more than once, and that `verify` returned normally.
#[test]
fn send_destination_divergence_is_silent() {
    let c = Arc::new(AtomicUsize::new(0));
    let dests = Arc::new(Mutex::new(Vec::new()));
    let d = Arc::clone(&dests);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let b = named("b", move || {
            let _z: Option<u64> = recv_msg();
        });
        let other = b.thread().id();
        // Entry 0 sends to main; later entries send the same message to `b`.
        let dest = if k == 0 { m } else { other };
        d.lock().unwrap().push(dest.to_string());
        let _a = named("a", move || send_msg(dest, 1u64));
        let _cc = named("c", move || send_msg(m, 7u64));
        let _x: u64 = recv_msg_block();
    });
    let dests = dests.lock().unwrap().clone();
    assert!(
        stats.execs > 1,
        "the program must be re-entered for this to measure anything; execs={}",
        stats.execs
    );
    assert!(
        dests.len() > 1 && dests[1..].iter().any(|t| *t != dests[0]),
        "a later entry must send to a different thread - that is the divergence; \
         destinations: {dests:?}"
    );
}

/// A thread's **return value** differs at a replayed position: the spawned thread
/// returns `1` on the first entry and `99` on later ones, and main joins it. No
/// diagnostic.
///
/// This is the same dead guard as the send value, one label along. The `End` arm of
/// `compare_for_replay` compares the recorded result only when it is not pending
/// (`if !s.result().is_pending() && s.result() != o.result()`), and
/// `initialize_for_execution` calls `set_pending()` on every thread's trailing
/// `End` result before replay begins, unconditionally — so the guard never opens.
/// There is therefore no execution in which a recorded result reaches the
/// comparison settled.
///
/// The test observes that the joined value really did differ between entries, that
/// the program was re-entered more than once, and that `verify` returned normally.
#[test]
fn thread_return_value_divergence_is_silent() {
    let c = Arc::new(AtomicUsize::new(0));
    let returned = Arc::new(Mutex::new(Vec::new()));
    let r = Arc::clone(&returned);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let h = thread::Builder::new()
            .name("a".to_string())
            .spawn(move || {
                send_msg(m, 1u64);
                if k == 0 {
                    1u64
                } else {
                    99u64
                }
            })
            .unwrap();
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
        r.lock().unwrap().push(h.join().unwrap());
    });
    let returned = returned.lock().unwrap().clone();
    assert!(
        stats.execs > 1,
        "the program must be re-entered for this to measure anything; execs={}",
        stats.execs
    );
    assert!(
        returned.len() > 1 && returned[0] == 1 && returned[1..].contains(&99),
        "a later entry must return a different value - that is the divergence; \
         returned: {returned:?}"
    );
}

/// A receive's **tag predicate** differs at a replayed position: the first entry
/// accepts any tag, later entries accept only tag `1`. No diagnostic.
///
/// `compare_for_replay`'s `RecvMsg` arm is `if let LabelEnum::RecvMsg(_o) = other {
/// return Ok(()) }` — both labels are bound to `_`-prefixed names and nothing is
/// examined. The predicate itself is a closure held in `RecvLoc::tag`, so equality
/// could not be checked even if the arm tried; but the arm also skips the fields
/// that could be compared.
#[test]
fn receive_tag_predicate_divergence_is_silent() {
    let c = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        s.lock().unwrap().push(k);
        let m = main_thread_id();
        let _a = named("a", move || send_tagged_msg(m, 1u32, 1u64));
        let _b = named("b", move || send_tagged_msg(m, 1u32, 2u64));
        // Both predicates admit the same sends, so the recorded reads-from stays
        // feasible - but the predicate is program behaviour, and it changed.
        let _x: u64 = if k == 0 {
            recv_tagged_msg_block(|_, _t| true)
        } else {
            recv_tagged_msg_block(|_, t| t == Some(1u32))
        };
        let _y: u64 = recv_tagged_msg_block(|_, _t| true);
    });
    assert!(
        stats.execs > 1 && seen.lock().unwrap().len() > 1,
        "the program must be re-entered for this to measure anything; execs={}",
        stats.execs
    );
}

/// A receive's **blocking-ness** differs at a replayed position: the first entry
/// blocks, later entries use the non-blocking receive there. No diagnostic.
///
/// This is the sharpest of the three. `RecvMsg::non_blocking` feeds the engine's
/// own analysis of which executions are blocked, and `compare_for_replay` does not
/// look at it — nor at `RecvMsg::comm`, the communication model. Those two are
/// program-supplied and comparable; the arm compares neither.
#[test]
fn receive_blockingness_divergence_is_silent() {
    let c = Arc::new(AtomicUsize::new(0));
    let shape = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&shape);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        if k == 0 {
            s.lock().unwrap().push("blocking");
            let _x: u64 = recv_msg_block();
        } else {
            s.lock().unwrap().push("non_blocking");
            let _x: Option<u64> = recv_msg();
        }
        let _y: u64 = recv_msg_block();
    });
    let shapes = shape.lock().unwrap().clone();
    assert!(
        stats.execs > 1,
        "the program must be re-entered for this to measure anything; execs={}",
        stats.execs
    );
    assert!(
        shapes.len() > 1 && shapes[0] == "blocking" && shapes[1..].contains(&"non_blocking"),
        "a later entry must use the non-blocking receive - that is the divergence; shapes: {shapes:?}"
    );
}

// ===========================================================================
// The consequence: a silent divergence can change what `verify` concludes.
// ===========================================================================

/// The reference: a program with no cross-execution state at all, whose behaviour
/// is exactly what the stateful program below does on its **first** entry (`a`
/// sends `1`). `verify` explores both orders in which main can receive, reaches the
/// one where it receives `7` and then `1`, and the program's own `panic!` fires.
///
/// Observed: `[(1, 7), (7, 1)]`, then the panic.
#[test]
#[should_panic(expected = "main received 7 then 1")]
fn reference_program_reports_its_bug() {
    traceforge::verify(cfg(), move || {
        let v = 1u64;
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, v));
        let _b = named("b", move || send_msg(m, 7u64));
        let x: u64 = recv_msg_block();
        let y: u64 = recv_msg_block();
        if (x, y) == (7u64, 1u64) {
            panic!("main received 7 then 1");
        }
    });
}

/// The same program, with one flaw: the value `a` sends is read from a captured
/// counter, so it is `1` on the first entry and `99` afterwards. That is a silent
/// divergence — nothing compares a send's value — and here it costs a bug.
///
/// The first entry behaves exactly like [`reference_program_reports_its_bug`], so
/// the recorded graph is that program's graph and the engine goes on to explore the
/// second receive order. But by then the recorded send value has been rewritten to
/// `99` by `recover_lost_data`, so main is handed `99` where the graph recorded
/// `1`: observed `[(1, 7), (7, 99)]` instead of `[(1, 7), (7, 1)]`. The pair that
/// trips the bug is never produced, `verify` returns normally, and nothing is
/// reported.
///
/// What this establishes is the comparison, not a property of the stateful program
/// on its own: its first-entry behaviour matches the reference program, and its
/// later rewriting suppresses a failure that the reference reports. Replay
/// validation neither reports that failure nor complains about the divergence.
#[test]
fn stateful_program_suppresses_a_failure_the_reference_reports() {
    let c = Arc::new(AtomicUsize::new(0));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&sent);
    let o = Arc::clone(&observed);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        let v: u64 = if k == 0 { 1 } else { 99 };
        s.lock().unwrap().push(v);
        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, v));
        let _b = named("b", move || send_msg(m, 7u64));
        let x: u64 = recv_msg_block();
        let y: u64 = recv_msg_block();
        o.lock().unwrap().push((x, y));
        if (x, y) == (7u64, 1u64) {
            panic!("main received 7 then 1");
        }
    });
    let sent = sent.lock().unwrap().clone();
    let observed = observed.lock().unwrap().clone();
    // The divergence happened: entry 0 sent 1, a later entry sent 99.
    assert!(
        sent.len() > 1 && sent[0] == 1 && sent[1..].contains(&99),
        "the flaw must be exercised; sent: {sent:?}"
    );
    // The first entry did what the reference program does.
    assert_eq!(
        observed.first(),
        Some(&(1u64, 7u64)),
        "the first entry must match the reference program; observed: {observed:?}"
    );
    // The reference program's failing receive order was explored - with the
    // rewritten value, so the bug was not reached.
    assert!(
        stats.execs > 1 && observed.contains(&(7u64, 99u64)),
        "the second receive order must be explored, carrying the rewritten value; \
         observed: {observed:?}, execs: {}",
        stats.execs
    );
    assert!(
        !observed.contains(&(7u64, 1u64)),
        "the bug the reference program reports must go unreported here; observed: {observed:?}"
    );
}
