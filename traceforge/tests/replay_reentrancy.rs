//! **A program that keeps state across executions is not re-entrant**, and this
//! fixture's divergence is one the engine's replay check does not cover, so it
//! completes with no diagnostic.
//!
//! The engine explores by *re-executing the program from its entry point* once
//! per execution (`explore`: `Execution::new(..)`, `Must::begin_execution(..)`,
//! `execution.run(|| f())`, in a loop) and replaying the recorded prefix.
//! `begin_execution` is the call that clears the recorded values before each
//! replay, which is why the comparison below never sees them. Replay restores the engine's own decisions
//! — interleaving, `nondet()`, which send a receive reads from. It does **not**
//! restore memory the program captured: those lines are ordinary Rust and simply
//! run again.
//!
//! Run with:
//! ```text
//! cargo test -j 2 -p traceforge --test replay_reentrancy -- --nocapture
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use traceforge::thread;
use traceforge::thread::main_thread_id;
use traceforge::{recv_msg_block, send_msg, Config, ConsType};

fn cfg() -> Config {
    Config::builder().with_cons_type(ConsType::FIFO).build()
}

fn named<F: FnOnce() + Send + 'static>(n: &str, f: F) -> thread::JoinHandle<()> {
    thread::Builder::new().name(n.to_string()).spawn(f).unwrap()
}

/// **1. Captured state is not reset between executions.**
///
/// Two senders and two receives, so the engine explores more than one execution.
/// The program records, on each entry, the value a captured counter gives it. If
/// each execution started from a *program* initial state the log would read
/// `[0, 0, …]`; it does not.
#[test]
fn a_captured_counter_is_not_reset_between_executions() {
    let counter = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));

    let c = Arc::clone(&counter);
    let l = Arc::clone(&log);
    let stats = traceforge::verify(cfg(), move || {
        // Ordinary Rust: not an engine operation, so replay does not replay it.
        let k = c.fetch_add(1, Ordering::SeqCst);
        l.lock().unwrap().push(k);

        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, 1u64));
        let _b = named("b", move || send_msg(m, 2u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });

    let seen = log.lock().unwrap().clone();
    println!(
        "\n  executions explored : {}\n  counter seen on each entry: {:?}\n  total entries       : {}",
        stats.execs,
        seen,
        seen.len()
    );

    assert!(
        stats.execs > 1,
        "need more than one execution for the point to be visible; got {}",
        stats.execs
    );
    assert_ne!(
        seen,
        vec![0usize; seen.len()],
        "if captured state were reset per execution every entry would read 0"
    );
    assert!(
        seen.windows(2).all(|w| w[0] < w[1]),
        "the counter only climbs across entries: replay restores engine state, \
         not program state; entries: {seen:?}"
    );
    println!("  => captured state survives every re-entry.\n");
}

/// **2. The harm: the program's behaviour changes under the engine's feet.**
///
/// Here the *value sent* depends on the captured counter, so the program attempts
/// different behaviour on each entry: `1` on the first, `99` on a later one. What
/// this fixture observes is those attempts and a normal return, not what the
/// recorded graph ended up holding.
///
/// **What this pins, and what it deliberately does not.** It asserts the observable
/// facts: the program was entered more than once, the first entry attempted `1`, a
/// later entry attempted `99`, and `verify` returned normally rather than panicking.
///
/// It does *not* assert what the recorded graph holds at the replayed position,
/// because `verify` returns `Stats` and this fixture has no handle on the graph to
/// assert against.
///
/// Why it is not caught: `compare_for_replay` accepts a `RecvMsg` unconditionally,
/// and compares a `SendMsg` value only when the recorded value is **not pending**
/// — but `initialize_for_execution` sets *every* send value pending before replay
/// begins, unconditionally, so that branch never opens. A send-value divergence is
/// therefore not detected here and cannot be made detectable by changing the
/// fixture: send values are not compared at all. `verify`'s rustdoc states which
/// divergences are detected and which are not; `replay_detection.rs` pins
/// representative cases of each.
#[test]
fn a_program_whose_sends_depend_on_captured_state_diverges_from_its_own_replay() {
    let counter = Arc::new(AtomicUsize::new(0));
    let sends = Arc::new(Mutex::new(Vec::new()));

    let c = Arc::clone(&counter);
    let s = Arc::clone(&sends);
    let stats = traceforge::verify(cfg(), move || {
        let k = c.fetch_add(1, Ordering::SeqCst);
        // Entry 0 sends 1; every later entry sends 99.
        let v: u64 = if k == 0 { 1 } else { 99 };
        s.lock().unwrap().push(v);

        let m = main_thread_id();
        let _a = named("a", move || send_msg(m, v));
        let _b = named("b", move || send_msg(m, 7u64));
        let _x: u64 = recv_msg_block();
        let _y: u64 = recv_msg_block();
    });

    let attempted = sends.lock().unwrap().clone();
    println!(
        "\n  executions explored : {}\n  value `a` sent on each entry: {:?}",
        stats.execs, attempted
    );

    assert!(
        attempted.len() > 1,
        "the program must be entered more than once for the point to exist; entries: {attempted:?}"
    );
    assert_eq!(
        attempted[0], 1,
        "the first entry sees counter 0 and must attempt 1; entries: {attempted:?}"
    );
    assert!(
        attempted[1..].contains(&99),
        "a later entry must attempt 99 — that is the divergence; entries: {attempted:?}"
    );
    assert!(
        stats.execs > 1,
        "verify must have explored more than one execution; got {}",
        stats.execs
    );
    // Reaching here at all is the silent half: `verify` returned rather than
    // panicking, although the program contradicted its own recorded prefix.
    println!(
        "  => captured state changed what the program attempted across executions, \
         and this\n     divergence completed with no diagnostic. Send values are never \
         compared on\n     replay, so no send-value divergence is reported.\n"
    );
}
