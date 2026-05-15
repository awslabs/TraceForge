use traceforge::{self, thread, ConsType};

#[derive(Clone, Debug, PartialEq)]
struct Msg {
    id: u32,
}

// Verifies the exploration count for inbox across different inbox/sender counts.
fn verify_inbox_exec_counts(inboxes: u32, senders: u32) {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            for _ in 0..inboxes {
                let _ = traceforge::inbox();
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for _ in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, Msg { id: 0 });
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });
    assert_eq!(stats.execs, (inboxes + 1).pow(senders) as usize);
}

// Ensures inbox deliveries contain unique, expected IDs across multiple inbox calls.
fn verify_unique_message_ids(inboxes: u32, senders: u32) {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let expected_ids: Vec<u32> = (0..senders).collect();
            let mut remaining_ids = expected_ids.clone();
            let mut seen_ids: Vec<u32> = Vec::new();

            for _ in 0..inboxes {
                let msgs = traceforge::inbox();
                let received_count = msgs.iter().flatten().count();

                assert!(
                    received_count <= remaining_ids.len(),
                    "received {} messages but only {} ids remain: {:?}",
                    received_count,
                    remaining_ids.len(),
                    remaining_ids
                );

                for val in msgs.into_iter().flatten() {
                    let msg = val
                        .as_any_ref()
                        .downcast_ref::<Msg>()
                        .expect("inbox should yield Msg values");
                    let id = msg.id;

                    assert!(
                        expected_ids.contains(&id),
                        "received id {} which is outside the expected set {:?}",
                        id,
                        expected_ids
                    );
                    assert!(
                        !seen_ids.contains(&id),
                        "received duplicate id {} in {:?}",
                        id,
                        seen_ids
                    );

                    seen_ids.push(id);
                    remaining_ids.retain(|existing| *existing != id);
                }
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for id in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, Msg { id });
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });

    let expected_execs = (inboxes + 1).pow(senders) as usize;
    assert_eq!(
        stats.execs, expected_execs,
        "unexpected exec count for inboxes={}, senders={}",
        inboxes, senders
    );
}

macro_rules! inbox_exec_count_test {
    ($name:ident, $inboxes:expr, $senders:expr) => {
        #[test]
        fn $name() {
            verify_inbox_exec_counts($inboxes, $senders);
        }
    };
}

macro_rules! inbox_unique_ids_test {
    ($name:ident, $inboxes:expr, $senders:expr) => {
        #[test]
        fn $name() {
            verify_unique_message_ids($inboxes, $senders);
        }
    };
}

// Execution count coverage (powerset of sender deliveries)
inbox_exec_count_test!(exec_count_0_inbox_0_sender, 0, 0);
inbox_exec_count_test!(exec_count_0_inbox_1_sender, 0, 1);
inbox_exec_count_test!(exec_count_0_inbox_2_sender, 0, 2);
inbox_exec_count_test!(exec_count_0_inbox_3_sender, 0, 3);
inbox_exec_count_test!(exec_count_0_inbox_4_sender, 0, 4);
inbox_exec_count_test!(exec_count_0_inbox_5_sender, 0, 5);
inbox_exec_count_test!(exec_count_0_inbox_6_sender, 0, 6);
inbox_exec_count_test!(exec_count_0_inbox_7_sender, 0, 7);
inbox_exec_count_test!(exec_count_0_inbox_8_sender, 0, 8);
inbox_exec_count_test!(exec_count_0_inbox_9_sender, 0, 9);
inbox_exec_count_test!(exec_count_1_inbox_0_sender, 1, 0);
inbox_exec_count_test!(exec_count_1_inbox_1_sender, 1, 1);
inbox_exec_count_test!(exec_count_1_inbox_2_sender, 1, 2);
inbox_exec_count_test!(exec_count_1_inbox_3_sender, 1, 3);
inbox_exec_count_test!(exec_count_1_inbox_4_sender, 1, 4);
inbox_exec_count_test!(exec_count_1_inbox_5_sender, 1, 5);
inbox_exec_count_test!(exec_count_1_inbox_6_sender, 1, 6);
inbox_exec_count_test!(exec_count_1_inbox_7_sender, 1, 7);
inbox_exec_count_test!(exec_count_1_inbox_8_sender, 1, 8);
inbox_exec_count_test!(exec_count_1_inbox_9_sender, 1, 9);
inbox_exec_count_test!(exec_count_2_inbox_0_sender, 2, 0);
inbox_exec_count_test!(exec_count_2_inbox_1_sender, 2, 1);
inbox_exec_count_test!(exec_count_2_inbox_2_sender, 2, 2);
inbox_exec_count_test!(exec_count_2_inbox_3_sender, 2, 3);
inbox_exec_count_test!(exec_count_2_inbox_4_sender, 2, 4);
inbox_exec_count_test!(exec_count_2_inbox_5_sender, 2, 5);
inbox_exec_count_test!(exec_count_2_inbox_6_sender, 2, 6);
inbox_exec_count_test!(exec_count_2_inbox_7_sender, 2, 7);
inbox_exec_count_test!(exec_count_2_inbox_8_sender, 2, 8);
inbox_exec_count_test!(exec_count_2_inbox_9_sender, 2, 9);
inbox_exec_count_test!(exec_count_3_inbox_0_sender, 3, 0);
inbox_exec_count_test!(exec_count_3_inbox_1_sender, 3, 1);
inbox_exec_count_test!(exec_count_3_inbox_2_sender, 3, 2);
inbox_exec_count_test!(exec_count_3_inbox_3_sender, 3, 3);
inbox_exec_count_test!(exec_count_3_inbox_4_sender, 3, 4);
inbox_exec_count_test!(exec_count_3_inbox_5_sender, 3, 5);
inbox_exec_count_test!(exec_count_3_inbox_6_sender, 3, 6);
inbox_exec_count_test!(exec_count_3_inbox_7_sender, 3, 7);
inbox_exec_count_test!(exec_count_3_inbox_8_sender, 3, 8);
inbox_exec_count_test!(exec_count_3_inbox_9_sender, 3, 9);
inbox_exec_count_test!(exec_count_4_inbox_0_sender, 4, 0);
inbox_exec_count_test!(exec_count_4_inbox_1_sender, 4, 1);
inbox_exec_count_test!(exec_count_4_inbox_2_sender, 4, 2);
inbox_exec_count_test!(exec_count_4_inbox_3_sender, 4, 3);
inbox_exec_count_test!(exec_count_4_inbox_4_sender, 4, 4);
inbox_exec_count_test!(exec_count_4_inbox_5_sender, 4, 5);
inbox_exec_count_test!(exec_count_4_inbox_6_sender, 4, 6);
inbox_exec_count_test!(exec_count_4_inbox_7_sender, 4, 7);
inbox_exec_count_test!(exec_count_4_inbox_8_sender, 4, 8);
inbox_exec_count_test!(exec_count_4_inbox_9_sender, 4, 9);
inbox_exec_count_test!(exec_count_5_inbox_0_sender, 5, 0);
inbox_exec_count_test!(exec_count_5_inbox_1_sender, 5, 1);
inbox_exec_count_test!(exec_count_5_inbox_2_sender, 5, 2);
inbox_exec_count_test!(exec_count_5_inbox_3_sender, 5, 3);
inbox_exec_count_test!(exec_count_5_inbox_4_sender, 5, 4);
inbox_exec_count_test!(exec_count_5_inbox_5_sender, 5, 5);
inbox_exec_count_test!(exec_count_5_inbox_6_sender, 5, 6);
inbox_exec_count_test!(exec_count_6_inbox_0_sender, 6, 0);
inbox_exec_count_test!(exec_count_6_inbox_1_sender, 6, 1);
inbox_exec_count_test!(exec_count_6_inbox_2_sender, 6, 2);
inbox_exec_count_test!(exec_count_6_inbox_3_sender, 6, 3);
inbox_exec_count_test!(exec_count_6_inbox_4_sender, 6, 4);
inbox_exec_count_test!(exec_count_6_inbox_5_sender, 6, 5);
inbox_exec_count_test!(exec_count_6_inbox_6_sender, 6, 6);

// Message uniqueness/validity coverage
inbox_unique_ids_test!(unique_ids_0_inbox_0_sender, 0, 0);
inbox_unique_ids_test!(unique_ids_0_inbox_1_sender, 0, 1);
inbox_unique_ids_test!(unique_ids_0_inbox_2_sender, 0, 2);
inbox_unique_ids_test!(unique_ids_0_inbox_3_sender, 0, 3);
inbox_unique_ids_test!(unique_ids_0_inbox_4_sender, 0, 4);
inbox_unique_ids_test!(unique_ids_0_inbox_5_sender, 0, 5);
inbox_unique_ids_test!(unique_ids_0_inbox_6_sender, 0, 6);
inbox_unique_ids_test!(unique_ids_0_inbox_7_sender, 0, 7);
inbox_unique_ids_test!(unique_ids_0_inbox_8_sender, 0, 8);
inbox_unique_ids_test!(unique_ids_0_inbox_9_sender, 0, 9);
inbox_unique_ids_test!(unique_ids_1_inbox_0_sender, 1, 0);
inbox_unique_ids_test!(unique_ids_1_inbox_1_sender, 1, 1);
inbox_unique_ids_test!(unique_ids_1_inbox_2_senders, 1, 2);
inbox_unique_ids_test!(unique_ids_1_inbox_3_senders, 1, 3);
inbox_unique_ids_test!(unique_ids_1_inbox_4_senders, 1, 4);
inbox_unique_ids_test!(unique_ids_1_inbox_5_senders, 1, 5);
inbox_unique_ids_test!(unique_ids_1_inbox_6_senders, 1, 6);
inbox_unique_ids_test!(unique_ids_1_inbox_7_senders, 1, 7);
inbox_unique_ids_test!(unique_ids_1_inbox_8_senders, 1, 8);
inbox_unique_ids_test!(unique_ids_1_inbox_9_senders, 1, 9);
inbox_unique_ids_test!(unique_ids_2_inboxes_0_sender, 2, 0);
inbox_unique_ids_test!(unique_ids_2_inboxes_1_sender, 2, 1);
inbox_unique_ids_test!(unique_ids_2_inboxes_2_senders, 2, 2);
inbox_unique_ids_test!(unique_ids_2_inboxes_3_senders, 2, 3);
inbox_unique_ids_test!(unique_ids_2_inboxes_4_senders, 2, 4);
inbox_unique_ids_test!(unique_ids_2_inboxes_5_senders, 2, 5);
inbox_unique_ids_test!(unique_ids_2_inboxes_6_senders, 2, 6);
inbox_unique_ids_test!(unique_ids_2_inboxes_7_senders, 2, 7);
inbox_unique_ids_test!(unique_ids_2_inboxes_8_senders, 2, 8);
inbox_unique_ids_test!(unique_ids_2_inboxes_9_senders, 2, 9);
inbox_unique_ids_test!(unique_ids_3_inboxes_0_senders, 3, 0);
inbox_unique_ids_test!(unique_ids_3_inboxes_1_senders, 3, 1);
inbox_unique_ids_test!(unique_ids_3_inboxes_2_senders, 3, 2);
inbox_unique_ids_test!(unique_ids_3_inboxes_3_senders, 3, 3);
inbox_unique_ids_test!(unique_ids_3_inboxes_4_senders, 3, 4);
inbox_unique_ids_test!(unique_ids_3_inboxes_5_senders, 3, 5);
inbox_unique_ids_test!(unique_ids_3_inboxes_6_senders, 3, 6);
inbox_unique_ids_test!(unique_ids_3_inboxes_7_senders, 3, 7);
inbox_unique_ids_test!(unique_ids_3_inboxes_8_senders, 3, 8);
inbox_unique_ids_test!(unique_ids_3_inboxes_9_senders, 3, 9);
inbox_unique_ids_test!(unique_ids_4_inboxes_0_senders, 4, 0);
inbox_unique_ids_test!(unique_ids_4_inboxes_1_senders, 4, 1);
inbox_unique_ids_test!(unique_ids_4_inboxes_2_senders, 4, 2);
inbox_unique_ids_test!(unique_ids_4_inboxes_3_senders, 4, 3);
inbox_unique_ids_test!(unique_ids_4_inboxes_4_senders, 4, 4);
inbox_unique_ids_test!(unique_ids_4_inboxes_5_senders, 4, 5);
inbox_unique_ids_test!(unique_ids_4_inboxes_6_senders, 4, 6);
inbox_unique_ids_test!(unique_ids_5_inboxes_0_senders, 5, 0);
inbox_unique_ids_test!(unique_ids_5_inboxes_1_senders, 5, 1);
inbox_unique_ids_test!(unique_ids_5_inboxes_2_senders, 5, 2);
inbox_unique_ids_test!(unique_ids_5_inboxes_3_senders, 5, 3);
inbox_unique_ids_test!(unique_ids_5_inboxes_4_senders, 5, 4);
inbox_unique_ids_test!(unique_ids_5_inboxes_5_senders, 5, 5);
inbox_unique_ids_test!(unique_ids_5_inboxes_6_senders, 5, 6);
inbox_unique_ids_test!(unique_ids_6_inboxes_0_senders, 6, 0);
inbox_unique_ids_test!(unique_ids_6_inboxes_1_senders, 6, 1);
inbox_unique_ids_test!(unique_ids_6_inboxes_2_senders, 6, 2);
inbox_unique_ids_test!(unique_ids_6_inboxes_3_senders, 6, 3);
inbox_unique_ids_test!(unique_ids_6_inboxes_4_senders, 6, 4);
inbox_unique_ids_test!(unique_ids_6_inboxes_5_senders, 6, 5);
inbox_unique_ids_test!(unique_ids_6_inboxes_6_senders, 6, 6);