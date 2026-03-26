use traceforge::*;
use traceforge::{channel, thread, Config};
use std::collections::HashMap;

fn program() {
    let (tx, rx) = channel::Builder::<String>::new().build();

    // Worker function that both threads will execute
    fn worker(tx: channel::Sender<String>, new_tx: channel::Sender<String>, thread_name: &str) {
        for i in 0..2 {
            if named_nondet("worker_choice") {
                tx.send_msg(format!("{}_iter{}", thread_name, i));
            }
        }
        let h3 = thread::spawn(move || additional_worker(new_tx, "thread1"));
        h3.join().unwrap();
    }

    let (new_tx, new_rx) = channel::Builder::<String>::new().build();

    // Worker function that both threads will execute
    fn additional_worker(tx: channel::Sender<String>, thread_name: &str) {
        for i in 0..1 {
            tx.send_msg(format!("{}_iter{}", thread_name, i));
        }
    }

    // Spawn the worker threads
    let tx2 = tx.clone();
    let new_tx2 = new_tx.clone();
    let new_tx3 = new_tx.clone();

    let h1 = thread::spawn(move || worker(tx, new_tx, "thread1"));
    let h2 = thread::spawn(move || worker(tx2, new_tx2, "thread2"));

    let h5 = thread::spawn(move || additional_worker(new_tx3, "thread1"));

    // Receive all messages (max 4 possible)
    for _ in 0..2 {
        new_rx.recv_msg_block();
    }
    // Receive all messages (max 4 possible)
    for _ in 0..4 {
        rx.recv_msg_block();
    }

    h1.join().unwrap();
    h2.join().unwrap();
    h5.join().unwrap();
}

#[test]
fn parallel_revisit_queue_rayon_named_nondet_two_threads_both_predetermined() {
    // Test RevisitQueueRayon strategy: rayon work-stealing with persistent Must reuse

    let mut choices = HashMap::new();
    choices.insert(
        "worker_choice".to_string(),
        vec![
            vec![true, false], // Thread 0: send on iter 0, don't send on iter 1
            vec![false, true], // Thread 1: don't send on iter 0, send on iter 1
        ],
    );

    for _ in 0..100 {
        let stats = verify(
            Config::builder()
                .with_verbose(0)
                .with_policy(SchedulePolicy::Arbitrary)
                .with_predetermined_choices(choices.clone())
                .with_partitioned_parallelization(true)
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon)
                .with_revisit_eager_interval(5)
                .with_partitioned_warmup(2)
                .build(),
            || program(),
        );
        assert!(
            stats.execs + stats.block == 12,
            "Expected 12 total executions, got {} (execs={}, block={})",
            stats.execs + stats.block,
            stats.execs,
            stats.block
        );
    }
}

#[test]
fn parallel_revisit_queue_rayon_batched_named_nondet_two_threads_both_predetermined() {
    // Test RevisitQueueRayon with batch_size=3

    let mut choices = HashMap::new();
    choices.insert(
        "worker_choice".to_string(),
        vec![
            vec![true, false],
            vec![false, true],
        ],
    );

    for _ in 0..100 {
        let stats = verify(
            Config::builder()
                .with_verbose(0)
                .with_policy(SchedulePolicy::Arbitrary)
                .with_predetermined_choices(choices.clone())
                .with_partitioned_parallelization(true)
                .with_partitioned_branching(BranchingStrategy::RevisitQueueRayon)
                .with_revisit_eager_interval(5)
                .with_revisit_queue_batch_size(3)
                .with_partitioned_warmup(2)
                .build(),
            || program(),
        );
        assert!(
            stats.execs + stats.block == 12,
            "Expected 12 total executions, got {} (execs={}, block={})",
            stats.execs + stats.block,
            stats.execs,
            stats.block
        );
    }
}
