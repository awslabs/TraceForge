use traceforge::{
    cover, recv_msg_block, send_msg,
    thread::{self, ThreadId},
    Config,
};
use rand::distr::{Bernoulli, Uniform};

//mod utils;

#[test]
fn sample_basic() {
    let n = 10;
    let stats = traceforge::verify(Config::builder().build(), move || {
        let s: u32 = traceforge::sample(Uniform::new(0, 100).unwrap(), n);
        println!("HERE {s}");
    });
    println!("Stats: {} {}", stats.execs, stats.block);
    assert_eq!(stats.execs, n);
}
#[test]
fn sample_with_nondet_choice() {
    let n = 10;
    let stats = traceforge::verify(Config::builder().with_verbose(1).build(), move || {
        if traceforge::nondet() {
            let s: u32 = traceforge::sample(Uniform::new(0, 100).unwrap(), n);
            println!("HERE {s}");
        }
    });
    println!("Stats: {} {}", stats.execs, stats.block);
    assert_eq!(stats.execs, n + 1);
}
#[test]
fn sample_with_comm() {
    let n = 10;
    let stats = traceforge::verify(Config::builder().with_verbose(1).build(), move || {
        let tid = thread::spawn(|| {
            let r: u32 = recv_msg_block();
            let t: u32 = traceforge::sample(Uniform::new(0, 1).unwrap(), r as usize);
            println!("t = {t}");
        })
        .thread()
        .id();
        let s: u32 = traceforge::sample(Uniform::new(1, 6).unwrap(), n);
        send_msg(tid, s);
    });
    println!("Stats: {} {}", stats.execs, stats.block);
    // assert_eq!(stats.execs, n);
}

#[test]
fn sample_with_bernoulli() {
    let n = 10;
    let stats = traceforge::verify(Config::builder().with_verbose(1).build(), move || {
        let tid = thread::spawn(|| {
            let r: bool = recv_msg_block();
            cover!("R", r);
        })
        .thread()
        .id();
        let s: bool = traceforge::sample(Bernoulli::new(0.6).unwrap(), n);
        send_msg(tid, s);
    });
    println!(
        "Stats: {} {} r={}",
        stats.execs,
        stats.block,
        stats.coverage.covered("R".to_owned())
    );
    assert_eq!(stats.execs, n);
}

/// T1: send(t2); recv(); send(T3);
/// T2: recv(); send(T1); send(T3);
/// T3: recv(); recv();
///
/// The state space is 2: in which order T3 receives its messages
#[test]
fn estimate_srs_rss_rr() {
    fn foo() {
        let t1 = thread::spawn(move || {
            let _: u32 = traceforge::recv_msg_block();
            let _: u32 = traceforge::recv_msg_block();
        });
        let t1id = t1.thread().id();
        let t2 = thread::spawn(move || {
            let m: (ThreadId, u32) = traceforge::recv_msg_block();
            traceforge::send_msg(m.0, 2u32);
            traceforge::send_msg(t1id.clone(), 3u32);
        });
        let t2id = t2.thread().id();
        let t3 = thread::spawn(move || {
            traceforge::send_msg(t2id, (thread::current().id(), 1u32));
            let _: u32 = traceforge::recv_msg_block();
            traceforge::send_msg(t1id, 4u32);
        });
        let _ = t1.join();
        let _ = t2.join();
        let _ = t3.join();
    }
    let states = traceforge::estimate_execs_with_samples(foo, 10);
    assert_eq!(states, 2.0);
}

#[test]
fn estimate_sr_ncopies() {
    fn foo(n_test: usize) {
        let mut tids = Vec::new();
        for _i in 0..n_test {
            let tr = thread::spawn(move || {
                let _: u32 = traceforge::recv_msg_block();
            });
            let tid = tr.thread().id();
            let ts = thread::spawn(move || {
                let _ = traceforge::send_msg(tid, 1u32);
            });
            tids.push(ts);
            tids.push(tr);
        }
        for tid in tids {
            let _ = tid.join();
        }
    }
    let n_test = 5;
    let states = traceforge::estimate_execs_with_samples(move || foo(n_test), 10);
    assert_eq!(states, 1.0);
}

#[test]
fn estimate_ns_nseqr() {
    fn foo(n_test: usize) {
        let mut tids = Vec::new();
        let receiver = thread::spawn(move || {
            for _i in 0..n_test {
                let _: u32 = traceforge::recv_msg_block();
            }
        });
        let receiver_id = receiver.thread().id();
        tids.push(receiver);
        for _i in 0..n_test {
            let rid = receiver_id.clone();
            let ts = thread::spawn(move || {
                let _ = traceforge::send_msg(rid, 1u32);
            });
            tids.push(ts);
        }
        for tid in tids {
            let _ = tid.join();
        }
    }
    let n_test = 5;
    let states = traceforge::estimate_execs_with_samples(move || foo(n_test), 1);
    // The number of execs is 1 under FIFO and n_test! under MO
    assert_eq!(states, 120.0);
}

#[test]
fn estimate_a_nstepsb() {
    fn foo(n_test: usize) {
        let mut tids = Vec::new();

        let sink = thread::Builder::new()
            .name("sink".to_owned())
            .spawn(|| {
                let _: u32 = traceforge::recv_msg_block();
                let _: u32 = traceforge::recv_msg_block();
            })
            .expect("Could not create sink");
        let sink_id = sink.thread().id();
        tids.push(sink);

        let receiver = thread::Builder::new()
            .name("receiver".to_owned())
            .spawn(move || {
                for _i in 0..n_test {
                    let _: u32 = traceforge::recv_msg_block();
                }
            })
            .expect("Could not create receiver");
        let receiver_id = receiver.thread().id();
        tids.push(receiver);

        let a = thread::Builder::new()
            .name("sender-a".to_owned())
            .spawn(move || {
                for _i in 0..n_test {
                    traceforge::send_msg(receiver_id, 0u32);
                }
                traceforge::send_msg(sink_id, 1u32);
            })
            .expect("Could not create sender-a");
        tids.push(a);

        let b = thread::Builder::new()
            .name("sender-b".to_owned())
            .spawn(move || {
                traceforge::send_msg(sink_id, 2u32);
            })
            .expect("Could not create sender-b");
        tids.push(b);

        for tid in tids {
            let _ = tid.join();
        }
    }
    let n_test = 5;
    let states = traceforge::estimate_execs_with_samples(move || foo(n_test), 5);
    assert_eq!(states, 2.0);
    let stats = traceforge::verify(Config::builder().build(), move || foo(n_test));
    println!("Stats = {}, {}", stats.execs, stats.block);
    assert_eq!(stats.execs, 2);
}
