use traceforge::SchedulePolicy::*;
use traceforge::{thread, Config, ConsType};
use std::time::Instant;

const TEST_RUNS: u32 = 10;

#[test]
fn ns_r() {
    #[derive(Clone, PartialEq, Debug)]
    enum Msg {
        Val(u32),
        TID(thread::ThreadId),
    }

    let n: u32 = 100;
    let mode = ConsType::Causal;

    for _i in 0..TEST_RUNS {
        let now = Instant::now();
        let stats = traceforge::verify(
            Config::builder()
                .with_policy(Arbitrary)
                .with_cons_type(mode)
                .build(),
            move || {
                let mut ns = Vec::new();
                for _i in 0..n {
                    ns.push(thread::spawn(move || {
                        let t4 = match traceforge::recv_msg_block() {
                            Msg::TID(t4) => t4,
                            _ => {
                                traceforge::assume!(false);
                                panic!()
                            }
                        };
                        traceforge::send_msg(t4, Msg::Val(n));
                    }));
                }
                let hr = thread::spawn(move || {
                    let _m1: Msg = traceforge::recv_msg_block();
                });

                for i in 0..n {
                    traceforge::send_msg(ns[i as usize].thread().id(), Msg::TID(hr.thread().id()));
                }
            },
        );
        assert_eq!(stats.execs as u32, n);
        let elapsed = now.elapsed();
        println!("Elapsed: {:.2?}", elapsed);
    }
}

fn factorial(n: u32) -> u32 {
    let mut result = 1;
    for i in 1..=n {
        result = result * i;
    }
    result
}
#[test]
fn ns_nr_tag() {
    #[derive(Clone, PartialEq, Debug)]
    enum Msg {
        Val(u32),
        #[allow(unused)]
        TID(thread::ThreadId),
    }
    let n = 7;
    fn scenario(n: u32, tagged: bool) {
        let r = thread::spawn(move || {
            for i in 0..n {
                if tagged {
                    let _r: Msg = traceforge::recv_tagged_msg_block(move |_, tag| tag == Some(i));
                } else {
                    let _r: Msg = traceforge::recv_msg_block();
                }
            }
        });
        for i in 0..n {
            let tid = r.thread().id();
            let _ = thread::spawn(move || {
                traceforge::send_tagged_msg(tid, i, Msg::Val(i));
            });
        }
    }
    for _i in 0..TEST_RUNS {
        for cons in [ConsType::MO, ConsType::FIFO, ConsType::Causal] {
            // tagged
            let now1 = Instant::now();
            let stats = traceforge::verify(
                Config::builder()
                    .with_policy(Arbitrary)
                    .with_cons_type(cons)
                    //.with_verbose(1)
                    .build(),
                move || scenario(n, true),
            );
            assert_eq!(stats.execs, 1);
            let elapsed = now1.elapsed();
            println!("Elapsed: {:?}", elapsed);

            // untagged
            let now2 = Instant::now();
            let stats = traceforge::verify(
                Config::builder()
                    .with_policy(Arbitrary)
                    .with_cons_type(cons)
                    //.with_verbose(1)
                    .build(),
                move || scenario(n, false),
            );
            assert_eq!(stats.execs, factorial(n) as usize);
            let elapsed = now2.elapsed();
            println!("Elapsed: {:?}", elapsed);
        }
    }
}

#[test]

fn ns_r_sel_mk() {
    #[derive(Clone, PartialEq, Debug)]

    enum Msg {
        Val(thread::ThreadId, u32),

        _TID(thread::ThreadId),
    }

    let n: u32 = 8;

    let mode = ConsType::FIFO;

    let now = Instant::now();

    let stats = traceforge::verify(
        Config::builder()
            .with_policy(LTR)
            .with_cons_type(mode)
            .build(),
        move || {
            // if std::thread works on main() the following should compile.

            // otherwise, uncomment the receives in the spawned threads

            let main_id = thread::current().id();

            let mut ns = Vec::new();

            for i in 0..n {
                ns.push(thread::spawn(move || {
                    traceforge::send_tagged_msg(main_id, i, Msg::Val(thread::current().id(), i));

                    // MAYBE UNCOMMENT:

                    // let t4 = match traceforge::recv_msg_block() {

                    // Msg::TID(t4) => t4,

                    // _ => {

                    // traceforge::assume(false);

                    // panic!()

                    // }

                    // };

                    // traceforge::send_tagged_msg(t4, i, Msg::Val(thread::current().id(), i));
                }));
            }

            for i in 0..n {
                let _m: Msg = traceforge::recv_tagged_msg_block(move |_, tag| tag == Some(i));
            }

            // MAYBE UNCOMMENT:

            // let hr = thread::spawn(move || {

            // for i in 0..n {

            // let _m: Msg = traceforge::recv_tagged_msg_block(move |_, tag| tag == Some(i));

            // }

            // });

            // for i in 0..n {

            // traceforge::send_msg(ns[i as usize].thread().id(), Msg::TID(hr.thread().id()));

            // }
        },
    );

    assert_eq!(stats.execs as u32, 1);

    let elapsed = now.elapsed();

    println!("Elapsed: {:.2?}", elapsed);
}

#[test]

fn ns_rn_mk() {
    #[derive(Clone, PartialEq, Debug)]

    enum Msg {
        Val(thread::ThreadId, u32),

        _TID(thread::ThreadId),
    }

    let n: u32 = 8;

    let mode = ConsType::FIFO;

    let now = Instant::now();

    let stats = traceforge::verify(
        Config::builder()
            .with_policy(LTR)
            .with_cons_type(mode)
            .build(),
        move || {
            // if std::thread works on main() the following should compile.

            // otherwise, uncomment the receives in the spawned threads

            let main_id = thread::current().id();

            let mut ns = Vec::new();

            for i in 0..n {
                ns.push(thread::spawn(move || {
                    traceforge::send_msg(main_id, Msg::Val(thread::current().id(), i));

                    // MAYBE UNCOMMENT:

                    // let t4 = match traceforge::recv_msg_block() {

                    // Msg::TID(t4) => t4,

                    // _ => {

                    // traceforge::assume(false);

                    // panic!()

                    // }

                    // };

                    // traceforge::send_msg(t4, Msg::Val(thread::current().id(), n));
                }));
            }

            for _i in 0..n {
                let _m: Msg = traceforge::recv_msg_block();
            }

            // MAYBE UNCOMMENT:

            // let hr = thread::spawn(move || {

            // for i in 0..n {

            // let _m: Msg = traceforge::recv_msg_block();

            // }

            // });

            // for i in 0..n {

            // traceforge::send_msg(ns[i as usize].thread().id(), Msg::TID(hr.thread().id()));

            // }
        },
    );

    assert_eq!(stats.execs as u32, factorial(n));

    let elapsed = now.elapsed();

    println!("Elapsed: {:.2?}", elapsed);
}

#[test]

fn wakeup_stress() {
    #[derive(Clone, PartialEq, Debug)]
    enum Msg {
        Start,
        Done,
        Val(thread::ThreadId, u32),
        #[allow(unused)]
        TID(thread::ThreadId),
    }

    let n: u32 = 8;

    let mode = ConsType::FIFO;

    let now = Instant::now();

    let stats = traceforge::verify(
        Config::builder()
            .with_policy(LTR)
            .with_cons_type(mode)
            .build(),
        move || {
            let main_id = thread::current().id();

            // waiter collects all messages and then messages main

            let waiter_id = thread::spawn(move || {
                for _i in 0..n {
                    let _m: Msg = traceforge::recv_msg_block();
                }

                traceforge::send_msg(main_id, Msg::Done);
            })
            .thread()
            .id();

            // senders message the waiter

            for i in 0..n {
                thread::spawn(move || {
                    traceforge::send_msg(waiter_id, Msg::Val(thread::current().id(), i));
                });
            }

            // main messages self and then receives a message (either start/done)

            traceforge::send_msg(main_id, Msg::Start);

            let _m: Msg = traceforge::recv_msg_block();
        },
    );
    assert_eq!(stats.execs as u32, 2 * factorial(n));
    let elapsed = now.elapsed();
    println!("Elapsed: {:.2?}", elapsed);
}
