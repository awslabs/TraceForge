use std::pin::pin;
use std::usize;

use traceforge::channel::{Builder, Sender};
use traceforge::coverage::ExecutionObserver;
use traceforge::future::spawn;
use traceforge::monitor_types::EndCondition;
use traceforge::msg::Message;
use traceforge::thread::{main_thread_id, ThreadId};
use traceforge::{cover, future, nondet, recv_msg_block, send_msg, thread, Config, SchedulePolicy};
use futures::future::{join_all, pending, select, select_all, Either};
use futures::prelude::*;
use futures::stream::FuturesUnordered;

const TEST_RUNS: i32 = 20;

macro_rules! run_must {
    ($b:block) => {
        for _ in 0..TEST_RUNS {
            let stats = traceforge::verify(
                Config::builder()
                    .with_policy(SchedulePolicy::Arbitrary)
                    .with_verbose(1)
                    .build(),
                move || $b,
            );
            println!("Execs: {}", stats.execs);
            println!("Blocks: {}", stats.block);
        }
    };
}

async fn async_recv_msg_block<T>() -> T
where
    T: Message + 'static + Send,
{
    let r: T = traceforge::recv_msg_block();
    r
}

async fn add(a: u32, b: u32) -> u32 {
    a + b
}

// Temporarily here
async fn async_send_msg<T: Message + 'static>(t: ThreadId, v: T) {
    send_msg(t, v);
}

async fn async_send_msg_ch<T: Message + 'static>(ch: &Sender<T>, v: T) {
    ch.send_msg(v)
}

#[test]
fn async_basic_future_operations() {
    run_must!({
        let main_id = thread::current_id();
        let sum = add(3, 5);
        let a = future::spawn(async move {
            let r = sum.await;
            assert_eq!(r, 8u32);
            traceforge::send_msg(main_id, 992u32);
            r
        });
        let j = future::block_on(future::spawn(async move {
            assert_eq!(a.await.unwrap(), 8u32);
            42u32
        }))
        .unwrap();
        assert_eq!(j, 42u32);
        let r: u32 = traceforge::recv_msg_block();
        assert_eq!(r, 992u32);
    });
}

#[test]
fn async_join_all() {
    run_must!({
        let main = thread::current_id();
        future::block_on(async move {
            async fn foo(t: ThreadId, i: u32) -> u32 {
                async_send_msg(t, i).await;
                i
            }

            let _ = future::spawn(async move {
                let futures = vec![foo(main, 1), foo(main, 2), foo(main, 3)];
                assert_eq!(join_all(futures).await, [1, 2, 3]);
            })
            .await;
        });
        let r1: u32 = traceforge::recv_msg_block();
        let r2: u32 = traceforge::recv_msg_block();
        let r3: u32 = traceforge::recv_msg_block();
        println!("{r1} {r2} {r3}");
    });
}

#[test]
fn async_select() {
    run_must!({
        let main = thread::current_id();
        future::block_on(async move {
            async fn foo(t: ThreadId, i: u32) -> u32 {
                async_send_msg(t, i).await;
                i
            }
            async fn pend() -> u32 {
                pending::<()>().await;
                1u32
            }
            let p = pend();
            let f2 = foo(main, 2);
            let _ = future::spawn(async move {
                match select(pin!(p), pin!(f2)).await {
                    Either::Left((_v1, _)) => assert!(false), // pend does not finish
                    Either::Right((v2, _)) => assert_eq!(v2, 2),
                }
            })
            .await;
            let p = pend();
            let f1 = foo(main, 1);
            let _ = future::spawn(async move {
                match select(pin!(f1), pin!(p)).await {
                    Either::Left((v1, _)) => assert_eq!(v1, 1),
                    Either::Right((_v2, _)) => assert!(false),
                }
            })
            .await;
            let f1 = foo(main, 1);
            let f2 = foo(main, 2);
            match select(pin!(f1), pin!(f2)).await {
                Either::Left((v1, _)) => assert_eq!(v1, 1),
                Either::Right((_v2, _)) => unreachable!(),
            }
        });
        let r: u32 = traceforge::recv_msg_block();
        println!("{r}");
    });
}

#[test]
fn async_not_consumed() {
    for _ in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(traceforge::ConsType::Causal)
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(1)
                .build(),
            || {
                let _ = future::spawn(async move {}); // not awaited
            },
        );
        println!("Execs: {}", stats.execs);
        println!("Blocks: {}", stats.block);
        assert!(stats.execs == 1 && stats.block == 0);
    }
}

// Same behaviors as causal
#[test]
fn async_thread_yield() {
    for _ in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(traceforge::ConsType::Mailbox)
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(1)
                .build(),
            || {
                let main = thread::current_id();

                let j1 = future::spawn(async move {
                    async_send_msg(main, 0u32).await;
                    async_send_msg(main, 0u32).await;
                });
                let j2 = future::spawn(async move {
                    println!("2");
                    async_send_msg(main, 1u32).await;
                });
                future::block_on(async {
                    j1.await.unwrap();
                    j2.await.unwrap();
                    let r1: u32 = async_recv_msg_block().await;
                    let r2: u32 = async_recv_msg_block().await;
                    let r3: u32 = async_recv_msg_block().await;
                    cover!("001", r1 == 0 && r2 == 0 && r3 == 1);
                    cover!("100", r1 == 1 && r2 == 0 && r3 == 0);
                    cover!("010", r1 == 0 && r2 == 1 && r3 == 0);
                });
            },
        );
        println!("Execs: {}", stats.execs);
        assert!(stats.coverage.is_covered("001".into()));
        assert!(stats.coverage.is_covered("100".into()));
        assert!(stats.coverage.is_covered("010".into()));
    }
}

#[test]
fn async_with_threads() {
    for _ in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(1)
                .build(),
            move || {
                thread::spawn(|| {
                    let v1 = async { 3u32 };
                    let v2 = async { 2u32 };
                    future::block_on(future::spawn(async move {
                        cover!("Spawn1");
                        assert_eq!(5u32, v1.await + v2.await);
                    }))
                    .unwrap_or_default();
                });
                thread::spawn(|| {
                    future::block_on(future::spawn(async move {
                        let v1 = async { 5u32 };
                        let v2 = async { 6u32 };
                        future::spawn(async move {
                            cover!("Spawn2");
                            assert_eq!(11u32, v1.await + v2.await);
                        })
                        .await
                        .unwrap();
                    }))
                    .unwrap();
                });
            },
        );
        assert!(stats.coverage.is_covered("Spawn1".into()));
        assert!(stats.coverage.is_covered("Spawn2".into()));
    }
}

#[test]
fn async_communication() {
    for _ in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(1)
                .build(),
            || {
                future::block_on(async {
                    let myid = thread::current_id();
                    let _ = thread::spawn(move || {
                        future::block_on(async {
                            async_send_msg(myid, 10u32).await;
                            async_send_msg(myid, 11u32).await;
                        });
                    });
                    let r: u32 = async_recv_msg_block().await;
                    assert_eq!(r, 10);
                    let r: u32 = async_recv_msg_block().await;
                    assert_eq!(r, 11);
                });
            },
        );
        println!("Execs = {}", stats.execs);
        println!("Blocked = {}", stats.block);
    }
}

#[test]
fn async_communication_spawn() {
    run_must!({
        future::block_on(async {
            let myid = thread::current_id();
            let _ = thread::spawn(move || {
                future::block_on(future::spawn(async move {
                    async_send_msg(myid, 10u32).await;
                    async_send_msg(myid, 11u32).await;
                }))
                .unwrap();
            });
            let r: u32 = async_recv_msg_block().await;
            assert_eq!(r, 10);
            let r: u32 = async_recv_msg_block().await;
            assert_eq!(r, 11);
        });
    });
}

#[test]
fn async_communication_sr_sr() {
    for _ in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(1)
                .build(),
            || {
                future::block_on(async {
                    let myid = thread::current_id();
                    let _ = thread::spawn(move || {
                        future::block_on(async {
                            async_send_msg(myid, thread::current_id()).await;
                            future::spawn(async move {
                                let mytid = thread::current_id();
                                async_send_msg(myid, mytid).await;
                                let r: u32 = async_recv_msg_block().await;
                                assert!(r == 65u32 || r == 96u32);
                            })
                            .await
                            .ok();
                            let r: u32 = async_recv_msg_block().await;
                            assert!(r == 65u32 || r == 96u32);
                        });
                    });
                    let r: ThreadId = async_recv_msg_block().await;
                    async_send_msg(r, 65u32).await;
                    let r: ThreadId = async_recv_msg_block().await;
                    async_send_msg(r, 96u32).await;
                });
            },
        );
        println!("Execs = {}", stats.execs);
        println!("Blocked = {}", stats.block);
        assert_eq!(stats.execs, 2);
    }
}

// async 2PC
// Messages to coordinator
#[derive(Clone, PartialEq, Debug)]
pub enum CoordinatorMsg {
    Init(Vec<ThreadId>),
    Yes,
    No,
}

// Messages to participants
#[derive(Clone, PartialEq, Debug)]
pub enum ParticipantMsg {
    Prepare(ThreadId),
    Commit,
    Abort,
}

fn coordinator() {
    println!("COORD");
    let ps = if let CoordinatorMsg::Init(ids) = traceforge::recv_msg_block() {
        ids
    } else {
        panic!()
    };
    let _ = future::block_on(async move {
        let ps = ps.clone();
        println!("RUN");

        let num_ps = ps.len();

        let mut jhandles = Vec::new();
        for p in ps.iter() {
            let pclone = p.clone();
            jhandles.push(future::spawn(async move {
                println!("SENDING Prepare");
                let myid = thread::current_id();
                async_send_msg(pclone, ParticipantMsg::Prepare(myid)).await;
                let vote = {
                    let vote: CoordinatorMsg = async_recv_msg_block().await;
                    println!("Vote = {:?}", vote);
                    match vote {
                        CoordinatorMsg::Yes => 1u32,
                        CoordinatorMsg::No => 0u32,
                        _ => panic!("Init should not be received"),
                    }
                };
                vote
            }));
        }
        println!("{:?}", jhandles);
        let mut count = 0u32;
        for j in jhandles {
            count += j.await.unwrap();
            println!("Count = {count}");
        }
        if count == num_ps as u32 {
            ps.iter()
                .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Commit));
        } else {
            ps.iter()
                .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Abort));
        }
    });
}

fn participant() {
    println!("In Participant");
    let cid = if let ParticipantMsg::Prepare(id) = traceforge::recv_msg_block() {
        id
    } else {
        panic!();
    };

    println!("Participant: Prepare received from Tid {:?}", cid);
    // P1
    let response = traceforge::nondet();
    if response {
        traceforge::send_msg(cid, CoordinatorMsg::Yes);
    } else {
        traceforge::send_msg(cid, CoordinatorMsg::No);
    }

    // P2
    let action = traceforge::recv_msg_block();
    match action {
        ParticipantMsg::Commit => assert!(response),
        ParticipantMsg::Abort => (),
        _ => panic!(),
    }
}

fn scenario(num_ps: u32) {
    println!("=================== New iteration ===================");
    let c = thread::spawn(coordinator);

    let mut ps = Vec::new();
    for _i in 0..num_ps {
        ps.push(thread::spawn(participant));
    }
    traceforge::send_msg(
        c.thread().id(),
        CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
    );
}

mod utils;
#[test]
fn two_pc_async() {
    for _ in 0..TEST_RUNS {
        let num_ps = 5;
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(0)
                .build(),
            move || scenario(num_ps),
        );
        println!("Execs: {}", stats.execs);
        assert_eq!(stats.execs as u32, 2_u32.pow(num_ps));
    }
}

/// The next protocol is a "quorum" version of 2PC where it is sufficient if at least half the participants agree
/// The protocol uses `FuturesUnordered` to cycle through async procedures waiting to hear back from participants
///
async fn interact(p: ThreadId) -> u32 {
    //println!("SENDING Prepare");
    let myid = thread::current_id();
    async_send_msg(p, ParticipantMsg::Prepare(myid)).await;
    let vote = {
        let vote: CoordinatorMsg = async_recv_msg_block().await;
        //println!("Vote = {:?}", vote);
        match vote {
            CoordinatorMsg::Yes => 1u32,
            CoordinatorMsg::No => 0u32,
            _ => panic!("Init should not be received"),
        }
    };
    vote
}

fn coordinator_quorum() {
    //println!("COORD");
    let ps = if let CoordinatorMsg::Init(ids) = traceforge::recv_msg_block() {
        ids
    } else {
        panic!()
    };
    let _ = future::block_on(async move {
        let ps = ps.clone();
        //println!("RUN");

        let num_ps = ps.len();

        let mut tasks = FuturesUnordered::new();
        let quorum = (num_ps + 1) / 2;
        // Send proposals to the quorum of nodes
        for node in ps.iter() {
            tasks.push(interact(*node));
        }
        let mut success: usize = 0;
        while success < quorum {
            match tasks.next().await {
                Some(u) => {
                    success += u as usize;
                }
                None => {
                    // quorum not reached
                    cover!("C::Abort");
                    ps.iter()
                        .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Abort));
                    return false;
                }
            }
        }
        cover!("C::Commit");
        ps.iter()
            .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Commit));
        true
    });
}

const PCOMMIT: &'static str = "PCommit";
const PABORT: &'static str = "PAbort";

fn participant_quorum() {
    println!("In Participant");
    loop {
        match traceforge::recv_msg_block() {
            ParticipantMsg::Prepare(id) => {
                //println!("Participant: Prepare received from Tid {:?}", id);
                // P1
                let response = traceforge::nondet();
                if response {
                    traceforge::send_msg(id, CoordinatorMsg::Yes);
                } else {
                    traceforge::send_msg(id, CoordinatorMsg::No);
                }
            }
            // unlike the usual 2PC, commit and aborts can come even before the prepare message
            // arrives, because a quorum has been found and the coordinator does not wait for the
            // async processes sending the prepare
            ParticipantMsg::Commit => {
                //println!("Committed");
                cover!(PCOMMIT);
            }
            ParticipantMsg::Abort => {
                cover!(PABORT);
            }
        }
    }
}

fn quorum_scenario(num_ps: u32) {
    println!("=================== New iteration ===================");
    let c = thread::Builder::new()
        .name("Coordinator".into())
        .spawn(coordinator_quorum)
        .unwrap();

    let mut ps = Vec::new();
    for _i in 0..num_ps {
        ps.push(thread::spawn_daemon(participant_quorum));
    }
    traceforge::send_msg(
        c.thread().id(),
        CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
    );
}

#[derive(Clone, Default)]
struct QuorumCheck {
    num_ps: u32,
}
impl QuorumCheck {
    pub fn new(num_ps: u32) -> Self {
        Self { num_ps }
    }
}

impl ExecutionObserver for QuorumCheck {
    fn after(
        &mut self,
        _eid: traceforge::ExecutionId,
        econdition: &EndCondition,
        c: traceforge::CoverageInfo,
    ) -> () {
        let quorum = (self.num_ps + 1) / 2;
        match econdition {
            EndCondition::AllThreadsCompleted => {
                println!(
                    "Status: {}| Participants: {} | {}",
                    c.covered("C::Commit".into()),
                    c.covered(PCOMMIT.into()),
                    c.covered(PABORT.into())
                );
                // if coordinator committed then at least quorum of participants committed
                if c.is_covered("C::Commit".into()) {
                    assert!(c.covered(PCOMMIT.into()) >= quorum as u64);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn two_pc_quorum_async() {
    for _ in 0..TEST_RUNS {
        let num_ps = 6;
        let stats = traceforge::verify(
            Config::builder()
                .with_verbose(0)
                .with_policy(traceforge::SchedulePolicy::Arbitrary)
                .with_callback(Box::new(QuorumCheck::new(num_ps)))
                .build(),
            move || quorum_scenario(num_ps),
        );
        println!("Execs: {}", stats.execs);
        println!(
            "Commits/Aborts: {} {}",
            stats.coverage.covered("C::Commit".into()),
            stats.coverage.covered("C::Abort".into()),
        );
        assert_eq!(
            stats.coverage.covered("C::Commit".into()),
            num_integer::binomial(num_ps, (num_ps + 1) / 2) as u64
        );
    }
}
struct LeakyFuture<F: Future> {
    i: i32,
    f: F,
}

impl<F: Future> LeakyFuture<F> {
    fn new(i: i32, f: F) -> Self {
        Self { i, f }
    }
}

impl<F: Future + Unpin> Future for LeakyFuture<F> {
    type Output = F::Output;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let res = self.f.poll_unpin(cx);
        if res.is_pending() {
            for _ in 0..self.i {
                send_msg(main_thread_id(), ());
            }
        }
        res
    }
}

#[test]
fn unstable_future() {
    let (mut execs, mut block) = (None, None);
    for i in 0..TEST_RUNS {
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(traceforge::SchedulePolicy::Arbitrary)
                .build(),
            || {
                future::block_on(async {
                    let f = future::spawn(async move {
                        let f1 = future::spawn(async move { return });
                        let f2 = future::spawn(async move { return });
                        let _ = futures::join!(LeakyFuture::new(1, f1), LeakyFuture::new(2, f2));
                    });
                    let _ = f.await;
                });
                // Make it even worse by injecting some backtracking
                // which can make TraceForge complain about program being incorrect
                nondet();
            },
        );
        if i == 0 {
            execs = Some(stats.execs);
            block = Some(stats.block);
        } else {
            assert_eq!(execs.unwrap(), stats.execs);
            assert_eq!(block.unwrap(), stats.block);
        }
        println!("{}", stats.execs);
    }
}

#[test]
fn test_async_block() {
    traceforge::verify(Config::builder().with_verbose(3).build(), move || {
        future::block_on(async {
            let _ = future::spawn(async move {
                traceforge::assume!(false);
            })
            .await;
        })
    });
}

// The main behavior we cannot get with a naive blocking approach
#[test]
fn simple_async_recv() {
    let stats = traceforge::verify(
        Config::builder()
            .with_policy(SchedulePolicy::Arbitrary)
            .with_verbose(2)
            .build(),
        move || {
            let v = future::block_on(async {
                let (sender, receiver) = traceforge::channel::Builder::new().build();
                let frecv = receiver.async_recv_msg();
                let fsend = async_send_msg_ch(&sender, 42);
                let (v, ()) = futures::join!(frecv, fsend);
                v
            });
            assert_eq!(v, 42);
        },
    );
    // The send in executed in the same thread as the receive,
    // therefore the receive will first return pending, before succeeding
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

// async_sends that are not awaited have no effect
#[test]
fn lazy_async_sends() {
    run_must!({
        future::block_on(async {
            let _ = async_send_msg(thread::current_id(), ());
            assert!(traceforge::recv_msg::<()>().is_none());
        });
    });
}

// A smoke test for async_recv, checking for termination
#[test]
fn async_recv_terminates() {
    traceforge::verify(
        Config::builder()
            .with_verbose(2)
            .with_policy(SchedulePolicy::LTR)
            .build(),
        move || {
            let (sender1, receiver1) = traceforge::channel::Builder::new().build();
            traceforge::thread::spawn(move || sender1.send_msg(()));
            future::block_on(receiver1.async_recv_msg());
        },
    );
}

// The async recv can (always) return Pending when Polled,
// allowing select to explore the case where the second receive fires.
#[test]
fn async_recv_pending() {
    for _ in 0..2000 {
        for left in [false, true] {
            let stats = traceforge::verify(
                Config::builder()
                    .with_cons_type(traceforge::ConsType::FIFO)
                    .with_policy(SchedulePolicy::Arbitrary)
                    .with_verbose(0)
                    .build(),
                move || {
                    let (sender1, receiver1) = traceforge::channel::Builder::new().build();
                    let (sender2, receiver2) = traceforge::channel::Builder::new().build();
                    let (sender3, receiver3) = traceforge::channel::Builder::new().build();

                    future::block_on(async {
                        let f = future::spawn(async move {
                            let recv1 = receiver1.async_recv_msg();
                            let recv2 = receiver2.async_recv_msg();

                            match select(recv1, recv2).await {
                                Either::Left(((), _)) => {
                                    sender3.send_msg(true);
                                }
                                Either::Right(((), _)) => {
                                    sender3.send_msg(false);
                                }
                            }
                        });

                        sender1.send_msg(());
                        sender2.send_msg(());
                        let _ = f.await;
                    });
                    // Asume left/right branch succeeded
                    traceforge::assume!(left == receiver3.recv_msg_block());
                },
            );
            // Both branches have at least one execution
            assert!(stats.execs >= 1);
            // In fact there are quite a few executions, arising from the possible
            // ways the two futures can fail when polled, the way they awaken the block_on future
            // when they both fail, as well as the whether the first future succeeds when polled again,
            // even when it was not the one that actually woke the block_on future up!
            // Additionally, the cancellation mechanism adds more behaviors
            // (putting back the message/cancelling the async_recv introduces more sends).
            // The asymmetry is due to the select always polling the left first,
            // irrespective of who woke them up.
            if left {
                assert_eq!((stats.execs, stats.block), (8, 6));
            } else {
                assert_eq!((stats.execs, stats.block), (6, 8));
            }
        }
    }
}

// Async sends are a) executed in the same thread and b) always ready.
// Since we do not explore permutations that `join` can `poll` the Futures,
// only the first send can be observed
#[test]
fn incomplete_join() {
    run_must!({
        future::block_on(async {
            let fsend1 = async_send_msg(thread::current_id(), 1);
            let fsend2 = async_send_msg(thread::current_id(), 2);
            let ((), ()) = futures::join!(fsend1, fsend2);
        });
        assert_ne!(recv_msg_block::<i32>(), 2);
    });
}

#[test]
fn simple_cancel() {
    let stats = traceforge::verify(Config::builder().with_verbose(3).build(), || {
        future::block_on(async {
            let (_, receiver1) = traceforge::channel::Builder::<()>::new().build();
            let _ = receiver1.async_recv_msg();
        });
    });
    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn nested_cancel() {
    let stats = traceforge::verify(Config::builder().with_verbose(3).build(), || {
        future::block_on(async {
            let (_, receiver1) = traceforge::channel::Builder::<()>::new().build();
            let _ = spawn(async move {
                receiver1.async_recv_msg().await;
            });
        });
    });
    assert_eq!((stats.execs, stats.block), (1, 0));
}

// https://oeis.org/A268586, shifted by two
fn weird_seq(n: u32) -> u32 {
    let t = n * (n + 7);
    if n >= 3 {
        t*(2 as u32).pow(n-3)
    } else {
        t/(2 as u32).pow(3-n)
    }
}

#[test]
fn select_collect_n() {
    // Repetitions with arbitrary scheduling
    let reps = 5;
    for n in 1..5 {
        let corrects : Vec<_> = (1..=n as u32).rev().map(weird_seq).collect();
        for iter in 1..n {
            if n == 5 && iter > 2 { break; } // Too long
            let correct : u32 = corrects.iter().take(iter).product();
            for _ in 0..reps {
                let stats = traceforge::verify(
                    Config::builder()
                        .with_policy(SchedulePolicy::Arbitrary)
                        .build(),
                    move || {
                        future::block_on(async {
                            let mut sends = Vec::new();
                            let mut recvs = Vec::new();
                            for _ in 0..n {
                                let (s, r) = traceforge::channel::Builder::new().build();
                                sends.push(s);
                                recvs.push(r);
                            }
                            thread::spawn(move || {
                                for i in 0..n { sends[i].send_msg(i); }
                            });

                            let mut res: usize = 0;
                            for _ in 0 .. iter {
                                let (ret, _, _) = select_all(
                                        recvs.iter().map(|r|r.async_recv_msg())
                                    ).await;
                                res += ret;
                            }

                            // Everything was collected
                            if iter == n {
                                assert_eq!(res, n*(n-1)/2);
                            }
                        });
                    },
                );
                assert_eq!(stats.execs, correct as usize);
                assert_eq!(stats.block, 0);
                println!("{} executions with {} blocked", stats.execs, stats.block);
            }
        }
    }
}

#[test]
fn no_await() {
    let stats = traceforge::verify(
        Config::builder()
            .with_cons_type(traceforge::ConsType::Bag)
            .with_verbose(1)
            .build(),
        || {
            let ch = Builder::<()>::new().build();
            let _ = future::spawn(ch.1.async_recv_msg());
        },
    );
    // the async_recv is dropped, it receives the cancellation message and
    // terminates successfully.
    assert_eq!(stats.execs, 1);
}
