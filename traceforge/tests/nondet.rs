use traceforge::*;
use traceforge::{thread, Nondet, Config};
use std::collections::HashMap;

#[test]
fn test_nondet() {
    verify(Config::builder().build(), || {
        let start = 4;
        let end = 6;
        let numbers = start..end;
        let n1 = numbers.nondet();

        assert!(n1 >= start);
        assert!(n1 < end);

        // Show that the type system allows us to use the same range to extract
        // another nondet value. This is possible because the Nondet<T> type
        // from TraceForge takes a reference.
        let _n2 = numbers.nondet();
    });
}

#[test]
fn test_named_nondet_without_predetermined() {
    // Test that named_nondet works like regular nondet when no predetermined values are set
    let stats = verify(Config::builder().build(), || {
        let choice = named_nondet("test_choice");
        // Should explore both true and false
        if choice {
            // one path
        } else {
            // another path
        }
    });
    // Should explore 2 executions (true and false)
    assert_eq!(stats.execs, 2);
}

#[test]
fn test_named_nondet_with_predetermined_single_thread() {
    // Test predetermined values in a single thread
    let mut choices = HashMap::new();
    choices.insert(
        "my_choice".to_string(),
        vec![
            vec![true, false, true], // Thread 0 (main): occurrences 0, 1, 2
        ],
    );

    let stats = verify(
        Config::builder()
            .with_predetermined_choices(choices)
            .build(),
        || {
            let c1 = named_nondet("my_choice"); // Should be true
            let c2 = named_nondet("my_choice"); // Should be false
            let c3 = named_nondet("my_choice"); // Should be true

            assert!(c1);
            assert!(!c2);
            assert!(c3);
        },
    );
    // Should explore only 1 execution (predetermined path)
    assert_eq!(stats.execs, 1);
}

#[test]
fn test_named_nondet_with_predetermined_multiple_threads() {
    // Test predetermined values across multiple threads
    let mut choices = HashMap::new();
    choices.insert(
        "worker_choice".to_string(),
        vec![
            vec![true],  // First thread to call named_nondet
            vec![false], // Second thread to call named_nondet
        ],
    );

    let stats = verify(
        Config::builder()
            .with_predetermined_choices(choices)
            .build(),
        || {
            let h1 = thread::spawn(|| {
                let choice = named_nondet("worker_choice");
                choice
            });

            let h2 = thread::spawn(|| {
                let choice = named_nondet("worker_choice");
                choice
            });

            let r1 = h1.join().unwrap();
            let r2 = h2.join().unwrap();

            // One thread should get true, the other false
            // (order depends on which calls named_nondet first)
            assert!(r1 != r2);
        },
    );
    assert!(stats.execs >= 1);
}

#[test]
fn test_named_nondet_mixed_predetermined_and_free() {
    // Test that unspecified choices fall back to nondeterministic exploration
    let mut choices = HashMap::new();
    choices.insert(
        "fixed_choice".to_string(),
        vec![vec![true]], // Only thread 0, only occurrence 0
    );

    let stats = verify(
        Config::builder()
            .with_predetermined_choices(choices)
            .build(),
        || {
            let fixed = named_nondet("fixed_choice"); // Predetermined: true
            let free = named_nondet("free_choice"); // Not predetermined: should explore both

            assert!(fixed); // Always true

            if free {
                // Explore this path
            } else {
                // And this path
            }
        },
    );
    // Should explore 2 executions (free choice explores both true and false)
    assert_eq!(stats.execs, 2);
}

#[test]
fn test_named_nondet_occurrence_tracking() {
    // Test that occurrence counting works correctly (e.g., in a loop)
    let mut choices = HashMap::new();
    choices.insert(
        "loop_choice".to_string(),
        vec![vec![true, true, false]], // Three occurrences
    );

    let stats = verify(
        Config::builder()
            .with_predetermined_choices(choices)
            .build(),
        || {
            let mut count = 0;
            for _ in 0..3 {
                if named_nondet("loop_choice") {
                    count += 1;
                }
            }
            // Should execute as: true, true, false → count = 2
            assert_eq!(count, 2);
        },
    );
    assert_eq!(stats.execs, 1);
}

#[test]
fn test_named_nondet_two_threads_full_nondeterminism() {
    // Test with two threads, each making choices in a loop, fully nondeterministic
    // Messages are sent only when choice is true
    use traceforge::channel;
    use traceforge::SchedulePolicy;

    for _ in 0..10 {
        let stats = verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .build(),
            || {
                let (tx, rx) = channel::Builder::<String>::new().build();

                // Worker function that both threads will execute
                fn worker(tx: channel::Sender<String>, thread_name: &str) {
                    for i in 0..2 {
                        if named_nondet("worker_choice") {
                            tx.send_msg(format!("{}_iter{}", thread_name, i));
                        }
                    }
                }

                // Spawn the worker threads
                // Move tx into threads directly without keeping copy in main thread
                let tx2 = tx.clone();

                let h1 = thread::spawn(move || worker(tx, "thread1"));
                let h2 = thread::spawn(move || worker(tx2, "thread2"));

                h1.join().unwrap();
                h2.join().unwrap();

                // Receive all messages (max 4 possible)
                for _ in 0..4 {
                    rx.recv_msg_block();
                }
            },
        );
        // Each thread sends at most two messages and they are symmetric
        // When both threads send two messages => 6 completed executions: 6 (receive order)
        // When one thread sends no message and the other two => 2 blocked executions: 1 (receive order) x 2 (which thread sends no message)
        // When one thread sends 1 message and the other 2 messages => 12 blocked executions: 3 (receive order) x 2 (which message is dropped) x 2 (which thread sends one message)
        // When each thread sends 1 message => 8 blocked executions: 2 (receive order) x 2 (which message is dropped in one thread) x 2 (which message is dropped in the other thread)
        // When one thread sends 1 message and the other none => 4 blocked executions: 1 (receive order) x 2 (which message is dropped in one thread) x 2 (which thread sends one message)
        // When no thread sends a message => 1 blocked executions
        assert_eq!(stats.execs, 6);
        assert_eq!(stats.block, 27);
    }
}

#[test]
fn test_named_nondet_two_threads_one_predetermined() {
    // Test with one thread predetermined and one thread free
    use traceforge::channel;
    use traceforge::SchedulePolicy;

    // init_log();
    let mut choices = HashMap::new();
    choices.insert(
        "worker_choice".to_string(),
        vec![
            vec![true, false], // Thread 0 (first to call): true then false
                               // Thread 1 (second to call): not predetermined, will explore all options
        ],
    );

    for _ in 0..10 {
        let stats = verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(0)
                .with_predetermined_choices(choices.clone())
                .build(),
            || {
                let (tx, rx) = channel::Builder::<String>::new().build();

                // Worker function that both threads will execute
                fn worker(tx: channel::Sender<String>, thread_name: &str) {
                    for i in 0..2 {
                        if named_nondet("worker_choice") {
                            tx.send_msg(format!("{}_iter{}", thread_name, i));
                        }
                    }
                }

                // Spawn the worker threads
                // Move tx into threads directly without keeping copy in main thread
                let tx2 = tx.clone();

                let h1 = thread::spawn(move || worker(tx, "thread1"));
                let h2 = thread::spawn(move || worker(tx2, "thread2"));

                h1.join().unwrap();
                h2.join().unwrap();

                // Receive all messages (max 4 possible)
                for i in 0..4 {
                    rx.recv_msg_block();
                    traceforge::assert(i < 3);
                }
            },
        );

        // One thread is predetermined
        // When one thread sends 1 message and the other 2 messages => 3 blocked executions: 3 (receive order)
        // When each thread sends 1 message => 4 blocked executions: 2 (receive order) x 2 (which message is dropped in one thread)
        // When one thread sends 1 message and the other none => 1 blocked executions: 1 (receive order)
        assert_eq!(stats.execs, 0);
        assert_eq!(stats.block, 8);
    }
}

#[test]
fn test_named_nondet_two_threads_both_predetermined() {
    // Test with both threads having predetermined values
    use traceforge::channel;
    use traceforge::SchedulePolicy;

    let mut choices = HashMap::new();
    choices.insert(
        "worker_choice".to_string(),
        vec![
            vec![true, false], // Thread 0: send on iter 0, don't send on iter 1
            vec![false, true], // Thread 1: don't send on iter 0, send on iter 1
        ],
    );

    for _ in 0..10 {
        let stats = verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_predetermined_choices(choices.clone())
                .build(),
            || {
                let (tx, rx) = channel::Builder::<String>::new().build();

                // Worker function that both threads will execute
                fn worker(tx: channel::Sender<String>, thread_name: &str) {
                    for i in 0..2 {
                        if named_nondet("worker_choice") {
                            tx.send_msg(format!("{}_iter{}", thread_name, i));
                        }
                    }
                }

                // Spawn the worker threads
                // Move tx into threads directly without keeping copy in main thread
                let tx2 = tx.clone();

                let h1 = thread::spawn(move || worker(tx, "thread1"));
                let h2 = thread::spawn(move || worker(tx2, "thread2"));

                h1.join().unwrap();
                h2.join().unwrap();

                // Receive all messages (max 4 possible)
                for i in 0..4 {
                    rx.recv_msg_block();
                    traceforge::assert(i < 2);
                }
            },
        );

        // Two threads are predetermined
        // Each thread sends 1 message => 2 blocked executions: 2 (receive order)
        assert_eq!(stats.execs, 0);
        assert_eq!(stats.block, 2);
    }
}

#[test]
fn test_named_nondet_two_threads_both_predetermined_with_blocked_executions() {
    // Test with both threads having predetermined values
    use traceforge::channel;
    use traceforge::SchedulePolicy;

    // init_log();
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
                .with_policy(SchedulePolicy::Arbitrary)
                .with_predetermined_choices(choices.clone())
                .build(),
            || {
                let (tx, rx) = channel::Builder::<String>::new().build();

                // Worker function that both threads will execute
                fn worker(tx: channel::Sender<String>, thread_name: &str) {
                    assume!(<bool>::nondet());
                    for i in 0..2 {
                        if named_nondet("worker_choice") {
                            tx.send_msg(format!("{}_iter{}", thread_name, i));
                        }
                    }
                }

                // Spawn the worker threads
                // Move tx into threads directly without keeping copy in main thread
                let tx2 = tx.clone();

                let h1 = thread::spawn(move || worker(tx, "thread1"));
                let h2 = thread::spawn(move || worker(tx2, "thread2"));

                h1.join().unwrap();
                h2.join().unwrap();

                // Receive all messages (max 4 possible)
                for i in 0..4 {
                    rx.recv_msg_block();
                    traceforge::assert(i < 2);
                }
            },
        );

        // Two threads are predetermined, once they pass their non-determinstic assume
        // Each thread sends 1 message => 2 blocked executions: 2 (receive order)
        // Only one thread is unblocked => 2 blocked executions: 2 (which thread is unblocked)
        // Both threads are blocked
        assert_eq!(stats.execs, 0);
        assert_eq!(stats.block, 5);
    }
}

#[test]
fn test_named_nondet_two_threads_mixed_names() {
    // Test with two threads using different named choices
    use traceforge::channel;
    use traceforge::SchedulePolicy;

    //init_log();
    let mut choices = HashMap::new();
    // Provide predetermined values
    choices.insert(
        "choice_a".to_string(),
        vec![
            vec![true, true], // always send
        ],
    );
    choices.insert(
        "choice_b".to_string(),
        vec![
            vec![false, false], // never send
        ],
    );

    for _ in 0..10 {
        let stats = verify(
            Config::builder()
                .with_policy(SchedulePolicy::Arbitrary)
                .with_verbose(0)
                .with_predetermined_choices(choices.clone())
                .build(),
            || {
                let (tx, rx) = channel::Builder::<String>::new().build();

                // Spawn the worker threads
                // Move tx into threads directly without keeping copy in main thread
                let tx2 = tx.clone();

                let h1 = thread::spawn(move || {
                    for i in 0..2 {
                        if named_nondet("choice_a") {
                            tx.send_msg(format!("thread1_iter{}", i));
                        }
                    }
                });

                let h2 = thread::spawn(move || {
                    for i in 0..2 {
                        if named_nondet("choice_b") {
                            tx2.send_msg(format!("thread2_iter{}", i));
                        }
                    }
                });

                h1.join().unwrap();
                h2.join().unwrap();

                // Collect exactly 2 messages (choice_a always true: thread1 sends iter0 and iter1)
                let msg1 = rx.recv_msg_block();
                let msg2 = rx.recv_msg_block();
                let messages = vec![msg1, msg2];

                // Thread 1 sends 2 messages (choice_a is always true)
                // Thread 2 sends 0 messages (choice_b is always false)
                assert_eq!(messages.len(), 2);

                assert!(
                    messages.iter().all(|m| m.contains("thread1")),
                    "All messages should be from thread1: {:?}",
                    messages
                );
            },
        );

        // Both threads are predetermined, exploring only one interleaving
        assert!(stats.execs == 1);
    }
}