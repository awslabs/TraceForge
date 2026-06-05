//! ABD atomic (single-/multi-writer, multi-reader) register.
//!
//! Attiya, Bar-Noy, Dolev: "Sharing Memory Robustly in Message-Passing
//! Systems", PODC 1990. A register is replicated over `n` servers; clients run
//! a two-phase protocol against a *majority* quorum:
//!
//!   - servers store `(val, ts)` with `ts = (logical_clock, process_id)`,
//!     initially `(⊥, (0,0))`. On `Query` they reply with their `(val, ts)`; on
//!     `Update{v,u}` they take it iff `u > ts` and then `ack`.
//!   - `queryPhase`: broadcast a query, wait for a majority of replies, return
//!     the `(val, ts)` with the largest timestamp.
//!   - `updatePhase(v,u)`: broadcast `(v,u)`, wait for a majority of acks.
//!   - `read()  = queryPhase(); updatePhase(v,u) /*write-back*/; return v`.
//!   - `write(v) by process i = (_,(t,_)) = queryPhase(); updatePhase(v,(t+1,i))`.
//!
//! Quorum intersection (any two majorities of `n` share a server) plus the read
//! **write-back** give linearizability and tolerate up to ⌊n/2⌋ slow/failed
//! servers — a client never waits for more than a majority.
//!
//! Messaging uses the top-level `traceforge::send_msg`/`recv_msg_block` API
//! addressed by `ThreadId` (as in `tests/2pc.rs`). Replies/acks are *tagged*
//! with the request's sequence number, and clients receive them with
//! `recv_tagged_msg_block`, so a stale reply from an earlier phase is ignored
//! (left in the mailbox) rather than miscounted toward the current quorum.
//!
//! Correctness is checked with focused, real-time-ordered assertions inside the
//! client threads (rather than a general linearizability monitor):
//!   - read-your-write (quorum intersection), and
//!   - no new/old read inversion (the write-back necessity from the slides:
//!     if the read protocol skipped its write-back, two sequentially ordered
//!     reads could return 42 then ⊥ — `no_new_old_inversion` would then fail).
//!
//! Convention: values are `i32` with **`0` standing for ⊥**; writes only ever
//! use nonzero values (42, 7).

use traceforge::thread::{self, ThreadId};
use traceforge::*;

/// Timestamp: `(logical_clock, process_id)`, ordered lexicographically (the
/// process id breaks ties between concurrent writers). `(0,0)` is the initial.
type Ts = (u32, u32);

/// Messages to a server. Sent untagged, with the sequence number `sn` in a
/// field so the server can echo it back as the reply/ack tag.
#[derive(Clone, Debug, PartialEq)]
enum ToServer {
    Query {
        client: ThreadId,
        sn: u32,
    },
    Update {
        client: ThreadId,
        sn: u32,
        val: i32,
        ts: Ts,
    },
}

/// Server's answer to a `Query`. Sent tagged with the request's `sn`.
#[derive(Clone, Debug, PartialEq)]
struct Reply {
    val: i32,
    ts: Ts,
}

/// Server's answer to an `Update`. Sent tagged with the request's `sn`. A
/// distinct type from `Reply` so a leftover reply can never satisfy an ack
/// receive (and vice versa).
#[derive(Clone, Debug, PartialEq)]
struct Ack;

/// A replica, starting from `(init_val, init_ts)`. Loops forever serving
/// queries and updates; once a run's clients have all finished, the final
/// `recv_msg_block` simply blocks (as in 2pc).
fn server(init_val: i32, init_ts: Ts) {
    let mut val: i32 = init_val;
    let mut ts: Ts = init_ts;
    loop {
        match recv_msg_block::<ToServer>() {
            ToServer::Query { client, sn } => {
                send_tagged_msg(client, sn, Reply { val, ts });
            }
            ToServer::Update {
                client,
                sn,
                val: v,
                ts: u,
            } => {
                if u > ts {
                    val = v;
                    ts = u;
                }
                send_tagged_msg(client, sn, Ack);
            }
        }
    }
}

/// Phase 1: broadcast a query, wait for a `majority` of replies (matching this
/// phase's sequence number), and return the `(val, ts)` with the largest `ts`.
fn query_phase(servers: &[ThreadId], majority: usize, sn: &mut u32) -> (i32, Ts) {
    *sn += 1;
    let s = *sn;
    let me = thread::current_id();
    for &srv in servers {
        send_msg(srv, ToServer::Query { client: me, sn: s });
    }
    let mut best_val = 0;
    let mut best_ts: Ts = (0, 0);
    for _ in 0..majority {
        let r: Reply = recv_tagged_msg_block(move |_tid, tag| tag == Some(s));
        if r.ts >= best_ts {
            best_ts = r.ts;
            best_val = r.val;
        }
    }
    (best_val, best_ts)
}

/// Phase 2: broadcast an update `(v, u)` and wait for a `majority` of acks
/// (matching this phase's sequence number).
fn update_phase(servers: &[ThreadId], majority: usize, sn: &mut u32, v: i32, u: Ts) {
    *sn += 1;
    let s = *sn;
    let me = thread::current_id();
    for &srv in servers {
        send_msg(
            srv,
            ToServer::Update {
                client: me,
                sn: s,
                val: v,
                ts: u,
            },
        );
    }
    for _ in 0..majority {
        let _ack: Ack = recv_tagged_msg_block(move |_tid, tag| tag == Some(s));
    }
}

/// `Read()`: query, then write-back the value found before returning it.
fn read(servers: &[ThreadId], majority: usize, sn: &mut u32) -> i32 {
    let (v, u) = query_phase(servers, majority, sn);
    update_phase(servers, majority, sn, v, u); // write-back
    v
}

/// `Write(v)` for the process with id `id`: query to learn the largest clock
/// `t`, then update with timestamp `(t + 1, id)`.
fn write(servers: &[ThreadId], majority: usize, sn: &mut u32, id: u32, v: i32) {
    let (_v, (t, _pid)) = query_phase(servers, majority, sn);
    update_phase(servers, majority, sn, v, (t + 1, id));
}

/// Spawns one replica (named "server i") starting from `(init_val, init_ts)`.
fn spawn_server(i: usize, init_val: i32, init_ts: Ts) -> ThreadId {
    thread::Builder::new()
        .name(format!("server {}", i))
        .spawn(move || server(init_val, init_ts))
        .unwrap()
        .thread()
        .id()
}

/// Spawns `n` fresh replicas (all initially `(⊥, (0,0))`).
fn spawn_servers(n: usize) -> Vec<ThreadId> {
    (0..n).map(|i| spawn_server(i, 0, (0, 0))).collect()
}

fn majority(n: usize) -> usize {
    n / 2 + 1
}

#[test]
fn read_your_write() {
    // A single client writes 42 then reads. Because the read's query quorum
    // must intersect the write's update quorum, the read always observes 42.
    let stats = verify(
        Config::builder().with_print_all_graphs_dot("abd_a").build(),
        || {
            let servers = spawn_servers(3);
            let m = majority(3);
            let mut sn = 0;
            write(&servers, m, &mut sn, 1, 42);
            let v = read(&servers, m, &mut sn);
            traceforge::assert(v == 42);
        },
    );
    println!("read_your_write: execs={} block={}", stats.execs, stats.block);
}

#[test]
fn no_new_old_inversion() {
    // The slide-5 setup: a write of 42 (timestamp (1,1)) "stalled" after
    // reaching only one replica — server 0 holds (42,(1,1)) while servers 1 and
    // 2 still hold ⊥. A single reader then reads twice.
    //
    // The read write-back gives monotonicity: once the first read returns 42 it
    // has propagated (42,(1,1)) to a majority, so the second read's quorum is
    // guaranteed to intersect it and also return 42. Hence r1==42 ⇒ r2==42.
    //
    // This is the discriminating test for the write-back: were `read` to skip
    // its `update_phase`, the first read could see 42 (quorum {0,1}) and the
    // second ⊥ (quorum {1,2}) — a new/old inversion — and the final assertion
    // would fail.
    //
    // Modelling the stalled writer as a pre-seeded server (rather than a live
    // concurrent writer thread) keeps the state space tractable while remaining
    // faithful to the slide.
    let stats = verify(
        Config::builder().with_print_all_graphs_dot("abd_b").build(),
        || {
            let servers = vec![
                spawn_server(0, 42, (1, 1)), // stalled write reached this replica
                spawn_server(1, 0, (0, 0)),
                spawn_server(2, 0, (0, 0)),
            ];
            let m = majority(3);

            // The driver thread is the reader.
            let mut sn = 0;
            let r1 = read(&servers, m, &mut sn);
            let r2 = read(&servers, m, &mut sn);
            traceforge::assert(r1 == 0 || r1 == 42);
            traceforge::assert(r2 == 0 || r2 == 42);
            traceforge::assert(!(r1 == 42 && r2 == 0));
        },
    );
    println!(
        "no_new_old_inversion: execs={} block={}",
        stats.execs, stats.block
    );
}

#[test]
#[ignore = "bounded random sample; run explicitly with --ignored"]
fn concurrent_writers() {
    // Two writers (distinct, nonzero process ids 1 and 2) write 42 and 7
    // concurrently; a reader reads. The reader may legitimately observe ⊥, 42,
    // or 7 depending on the interleaving and which write won the timestamp
    // tie-break — but never any other value.
    //
    // Two live concurrent writers each running a full two-phase write makes the
    // state space far too large to explore exhaustively, so this is a *bounded
    // random sample* (Arbitrary policy + max_iterations): it checks the
    // assertion on many sampled interleavings rather than proving it for all.
    let stats = verify(
        Config::builder()
            .with_policy(SchedulePolicy::Arbitrary)
            .with_max_iterations(50_000)
            .build(),
        || {
        let servers = spawn_servers(3);
        let m = majority(3);

        for (id, v) in [(1u32, 42i32), (2u32, 7i32)] {
            let s = servers.clone();
            thread::Builder::new()
                .name(format!("writer {}", id))
                .spawn(move || {
                    let mut sn = 0;
                    write(&s, m, &mut sn, id, v);
                })
                .unwrap();
        }

        let mut sn = 0;
        let v = read(&servers, m, &mut sn);
        traceforge::assert(v == 0 || v == 42 || v == 7);
    });
    println!(
        "concurrent_writers: execs={} block={}",
        stats.execs, stats.block
    );
}
