//! Litmus tests for causal delivery in programs that *mix* communication
//! models, i.e. where causality between two `CausalOrder` (Must: cd) sends
//! runs through a `TotalOrder` (Must: mbox) message.
//!
//! Must, Def. 3.6 defines cd-consistency over the whole graph: with `S_cd`
//! the cd sends, `so ≜ [S_cd]; porf; [S_cd]`, `so|dst` its same-destination
//! restriction, `[US]` the identity on unread sends and `mval` the
//! send/receive value match, a graph is cd-consistent iff (a) it is
//! well-formed, (b) `[US]; so|dst; rf ∩ mval` is empty, and (c)
//! `po; rf⁻¹; (so|dst; rf ∩ mval)` is irreflexive. The brackets restrict only
//! the *endpoints* of `so`: the `porf` path between the two cd sends may pass
//! through events of any model, mailbox included.
//!
//! TraceForge gates cd delivery with a per-send vector clock `sb`, which for a
//! `CausalOrder` send is a copy of the send's `posw` ("po ∪ synchronizes-with")
//! clock. `cons.rs::calc_views` used to build `posw` from the rf edges of every
//! model *except* `TotalOrder`, in both the `RecvMsg` and the `Inbox` arm. A cd
//! send whose causal history crossed a mailbox receive therefore looked
//! concurrent with its cd predecessor, and the exploration admitted deliveries
//! Def. 3.6(b) forbids -- a soundness bug: the checker reports
//! executions, and hence assertion counterexamples, that cannot happen.
//!
//! Each test below builds a program in which a mailbox hop is the *only*
//! carrier of the ordering between two cd sends to one receiver, and asserts
//! the execution count Def. 3.6 permits. All threads are spawned before the
//! first send in every pattern: a spawn creates a lifecycle (`TCreate -> Begin`)
//! edge that `calc_views` merges into `posw` unconditionally, so a spawn placed
//! after the first send would order that send before the spawned thread's
//! events through that edge and mask the bug.
//!
//! Without the `posw` fix each test fails in the direction shown in its own
//! doc comment.

use traceforge::channel::Builder;
use traceforge::loc::CommunicationModel;
use traceforge::thread;
use traceforge::{Config, ConsType};

/// The witness, run with the relay channel at one communication model.
///
/// ```text
/// data channel  d : CausalOrder      relay channel t : `relay_comm`
///
/// thread A (main) : spawn(C); spawn(B); d.send(1) "m1"; t.send(0) "token"
/// thread B        : t.recv_block(); d.send(2) "m2"
/// thread C        : d.recv_block()
/// ```
///
/// `m1` reaches `m2` only along `po(A) ; rf(token) ; po(B)`.
fn relay_witness(relay_comm: CommunicationModel) -> traceforge::Stats {
    traceforge::verify(Config::builder().with_seed(0).build(), move || {
        let (dsend, drecv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::CausalOrder)
            .build();
        let (tsend, trecv) = Builder::<i32>::new().with_comm(relay_comm).build();
        let dsend_b = dsend.clone();

        // Thread C: the cd receive whose reads-from is under test.
        let _c = thread::spawn(move || {
            let _v: i32 = drecv.recv_msg_block();
        });
        // Thread B: relay-receive the token, then cd-send m2.
        let _b = thread::spawn(move || {
            let _token: i32 = trecv.recv_msg_block();
            dsend_b.send_msg(2);
        });

        // Thread A is main.
        dsend.send_msg(1);
        tsend.send_msg(0);
    })
}

/// The witness under all four communication models for the relay channel.
///
/// `m1 ->porf m2` in every graph of this program, and both are cd sends to the
/// same destination, so `m1 ->so|dst-> m2` (Def. 3.6). A graph in which C reads
/// `m2` leaves `m1` unread and value-matching, which Def. 3.6(b) forbids.
/// Exactly one cd-consistent behavior remains -- C reads 1 -- for *every* relay
/// model, since `so` is defined over the full `porf` and does not care which
/// model carries the path: **`execs == 1`, `block == 0`, four times**.
///
/// Without the fix the `TotalOrder` case observes `execs == 2`: the excluded rf
/// edge keeps `m1` out of `sb(m2)`, so the backward revisit re-points C's
/// receive to `m2`. The three other models already pass -- they exercise the
/// `_` arm of the removed `match` -- which localizes the bug to the
/// `TotalOrder` arm.
#[test]
fn mixed_relay_four_models() {
    // `TotalOrder` last: pre-fix, the three control models pass and the loop
    // then fails on `TotalOrder`, which localizes the failure within this test.
    for relay_comm in [
        CommunicationModel::CausalOrder,
        CommunicationModel::LocalOrder,
        CommunicationModel::NoOrder,
        CommunicationModel::TotalOrder,
    ] {
        let stats = relay_witness(relay_comm);
        assert_eq!(stats.execs, 1, "relay = {relay_comm:?}");
        assert_eq!(stats.block, 0, "relay = {relay_comm:?}");
    }
}

/// The same witness with the relay at `TotalOrder` and C *asserting* the value
/// it read -- the soundness-violating form of the bug.
///
/// Def. 3.6 admits exactly one delivery (C reads 1, see
/// `mixed_relay_four_models`), so `traceforge::assert(v == 1)` holds in every
/// execution the cd model allows: **`execs == 1`, `block == 0`, no violation**.
///
/// Without the fix the checker also explores the forbidden delivery and the
/// assertion fails, i.e. the tool reports a counterexample for an execution
/// that cannot happen.
#[test]
fn mixed_relay_no_spurious_cex() {
    let stats = traceforge::verify(Config::builder().with_seed(0).build(), move || {
        let (dsend, drecv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::CausalOrder)
            .build();
        let (tsend, trecv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::TotalOrder)
            .build();
        let dsend_b = dsend.clone();

        let _c = thread::spawn(move || {
            let v: i32 = drecv.recv_msg_block();
            // Only m1 may be delivered: m1 ->so|dst-> m2 and m1 is unread in
            // any graph where C reads m2, which Def. 3.6(b) forbids.
            traceforge::assert(v == 1);
        });
        let _b = thread::spawn(move || {
            let _token: i32 = trecv.recv_msg_block();
            dsend_b.send_msg(2);
        });

        dsend.send_msg(1);
        tsend.send_msg(0);
    });
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

/// Causality through **two** mailbox hops.
///
/// ```text
/// data channel d : CausalOrder     relay channels t1, t2 : TotalOrder
///
/// thread A (main) : d.send(1) "m1"; t1.send(0)
/// thread B1       : t1.recv_block(); t2.send(0)
/// thread B2       : t2.recv_block(); d.send(2) "m2"
/// thread C        : d.recv_block()
/// ```
///
/// `m1 ->porf m2` runs `po(A) ; rf(t1) ; po(B1) ; rf(t2) ; po(B2)`, so the same
/// Def. 3.6(b) argument applies and only C-reads-1 is cd-consistent:
/// **`execs == 1`, `block == 0`**. This checks that the fix propagates
/// causality transitively through a chain of mailbox messages, not just across
/// a single hop.
///
/// Without the fix each hop drops its rf edge from `posw`, `m1` stays out of
/// `sb(m2)`, and the revisit produces `execs == 2`.
#[test]
fn mixed_two_hop_relay() {
    let stats = traceforge::verify(Config::builder().with_seed(0).build(), move || {
        let (dsend, drecv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::CausalOrder)
            .build();
        let (t1send, t1recv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::TotalOrder)
            .build();
        let (t2send, t2recv) = Builder::<i32>::new()
            .with_comm(CommunicationModel::TotalOrder)
            .build();
        let dsend_b2 = dsend.clone();

        let _c = thread::spawn(move || {
            let _v: i32 = drecv.recv_msg_block();
        });
        let _b1 = thread::spawn(move || {
            let _token: i32 = t1recv.recv_msg_block();
            t2send.send_msg(0);
        });
        let _b2 = thread::spawn(move || {
            let _token: i32 = t2recv.recv_msg_block();
            dsend_b2.send_msg(2);
        });

        dsend.send_msg(1);
        t1send.send_msg(0);
    });
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

/// The reverse mixing: a `Mailbox` *configuration* with one `CausalOrder`
/// channel, the token travelling by thread inbox.
///
/// ```text
/// config ConsType::Mailbox  =>  thread-inbox operations are TotalOrder
/// data channel d : with_comm(CausalOrder)
///
/// thread A (main) : d.send(1) "m1"; send_msg(B, 0) "token"
/// thread B        : recv_msg_block(); d.send(2) "m2"
/// thread C        : d.recv_block()
/// ```
///
/// Same Def. 3.6(b) argument, same count: **`execs == 1`, `block == 0`**. The
/// point of this pattern is that the mixed pair can be produced by the global
/// configuration rather than by a second `with_comm` channel -- one channel
/// suffices to hit the bug.
///
/// Without the fix: `execs == 2`.
#[test]
fn mixed_mailbox_config_causal_channel() {
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(ConsType::Mailbox)
            .with_seed(0)
            .build(),
        move || {
            let (dsend, drecv) = Builder::<i32>::new()
                .with_comm(CommunicationModel::CausalOrder)
                .build();
            let dsend_b = dsend.clone();

            let _c = thread::spawn(move || {
                let _v: i32 = drecv.recv_msg_block();
            });
            let b = thread::spawn(move || {
                let _token: i32 = traceforge::recv_msg_block();
                dsend_b.send_msg(2);
            });

            dsend.send_msg(1);
            traceforge::send_msg(b.thread().id(), 0i32);
        },
    );
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

/// The `Inbox` arm of `calc_views`: B collects the token with
/// [`traceforge::inbox`] instead of a receive.
///
/// ```text
/// config ConsType::Mailbox  =>  the Inbox label's model is TotalOrder
/// data channel d : with_comm(CausalOrder)
///
/// thread A (main) : d.send(1) "m1"; send_msg(B, 0) "token"
/// thread B        : inbox(); d.send(2) "m2"
/// thread C        : d.recv_block()
/// ```
///
/// `inbox()` is `inbox_with_bounds(0, None)`: it is non-blocking, so the Inbox
/// label may read the empty set as well as `{token}`, and the count is derived
/// per branch:
///
/// * Inbox reads `{}` -- no `porf` path from `m1` to `m2` exists (A's spawn of
///   B is po-before `m1`), so `m1` and `m2` are genuinely concurrent cd sends
///   and both deliveries are cd-consistent: **2 executions**, in every version
///   of the checker.
/// * Inbox reads `{token}` -- `m1 ->porf m2` through the inbox rf edge, so by
///   Def. 3.6(b) only C-reads-1 is consistent: **1 execution**.
///
/// Total **`execs == 3`, `block == 0`**. Without the fix the `Inbox` arm drops
/// the rf clock from `posw` exactly as the `RecvMsg` arm did, the second branch
/// also explores C-reads-2, and the observed total is 4. The pre/post
/// difference is confined to the `{token}` branch, which is the Inbox arm.
#[test]
fn mixed_inbox_relay() {
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(ConsType::Mailbox)
            .with_seed(0)
            .build(),
        move || {
            let (dsend, drecv) = Builder::<i32>::new()
                .with_comm(CommunicationModel::CausalOrder)
                .build();
            let dsend_b = dsend.clone();

            let _c = thread::spawn(move || {
                let _v: i32 = drecv.recv_msg_block();
            });
            let b = thread::spawn(move || {
                let _tokens = traceforge::inbox();
                dsend_b.send_msg(2);
            });

            dsend.send_msg(1);
            traceforge::send_msg(b.thread().id(), 0i32);
        },
    );
    assert_eq!(stats.execs, 3);
    assert_eq!(stats.block, 0);
}
