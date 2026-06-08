use traceforge::{self, thread};

const ACCEPTED_TAG: u32 = 2;
const INBOX_CALLS: u32 = 1;

#[test]
fn inbox_filters_on_tag_predicate() {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let msgs = traceforge::inbox_with_tag(|_, tag| tag == Some(ACCEPTED_TAG));

            assert!(
                msgs.len() <= 1,
                "expected at most one tagged message, got {}",
                msgs.len()
            );

            if let Some(Some(val)) = msgs.get(0) {
                let v = val
                    .as_any_ref()
                    .downcast_ref::<u32>()
                    .expect("expected u32 payload");
                assert_eq!(*v, ACCEPTED_TAG);
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let other_tag = ACCEPTED_TAG + 1;

        let mut senders = Vec::new();
        senders.push(thread::spawn({
            let inbox_tid = inbox_tid.clone();
            move || traceforge::send_tagged_msg(inbox_tid, ACCEPTED_TAG, ACCEPTED_TAG)
        }));
        senders.push(thread::spawn(move || {
            traceforge::send_tagged_msg(inbox_tid, other_tag, other_tag)
        }));

        for sender in senders {
            let _ = sender.join();
        }

        let _ = inbox_thread.join();
    });

    let matching_senders: u32 = 1; // only the ACCEPTED_TAG sender matches the predicate
    let expected_execs = (INBOX_CALLS + 1).pow(matching_senders) as usize;
    assert_eq!(stats.execs, expected_execs);
}

#[test]
fn inbox_vec_tag_lexicographic_filter_exec_count() {
    let pivot = vec![2, 0];
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn({
            let pivot = pivot.clone();
            move || {
                let _ = traceforge::inbox_with_vec_tag_and_bounds(
                    move |_, tag| tag.is_some_and(|t| t < pivot),
                    1,
                    None,
                );
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let s1 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![1, 5], 15u32);
        });
        let s2 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![2, 0], 20u32);
        });
        let s3 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![0, 9], 9u32);
        });

        let _ = s1.join();
        let _ = s2.join();
        let _ = s3.join();
        let _ = inbox_thread.join();
    });

    // Matching tags are [1,5] and [0,9], so all non-empty subsets are explored.
    assert_eq!(stats.execs, 3);
    assert_eq!(stats.block, 0);
}

#[test]
fn inbox_vec_tag_max1_replacement_revisit() {
    let target = vec![7, 7];
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn({
            let target = target.clone();
            move || {
                let _ = traceforge::inbox_with_vec_tag_and_bounds(
                    move |_, tag| tag == Some(target.clone()),
                    1,
                    Some(1),
                );
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let s1 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![7, 7], 1u32);
        });
        let s2 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![7, 7], 2u32);
        });
        let s3 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![8, 0], 3u32);
        });

        let _ = s1.join();
        let _ = s2.join();
        let _ = s3.join();
        let _ = inbox_thread.join();
    });

    // Exactly one of the two matching sends should be chosen.
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn inbox_vec_tag_empty_normalizes_to_none() {
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let msgs = traceforge::inbox_with_vec_tag_and_bounds(|_, tag| tag.is_none(), 1, Some(1));
            assert_eq!(msgs.len(), 1);
            let Some(Some(v)) = msgs.first() else {
                panic!("expected one received message");
            };
            let n = v
                .as_any_ref()
                .downcast_ref::<u32>()
                .expect("expected u32 payload");
            assert_eq!(*n, 0);
        });

        let inbox_tid = inbox_thread.thread().id();
        let s1 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![], 0u32);
        });
        let s2 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![0], 1u32);
        });

        let _ = s1.join();
        let _ = s2.join();
        let _ = inbox_thread.join();
    });

    // Only the empty vector tag should match tag.is_none().
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

#[test]
fn inbox_vec_tag_nonmatching_does_not_increase_states() {
    let target = vec![1, 2];
    let stats = traceforge::verify(traceforge::Config::builder().build(), move || {
        let inbox_thread = thread::spawn({
            let target = target.clone();
            move || {
                let _ = traceforge::inbox_with_vec_tag_and_bounds(
                    move |_, tag| tag == Some(target.clone()),
                    0,
                    None,
                );
            }
        });

        let inbox_tid = inbox_thread.thread().id();
        let s1 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![1, 2], 12u32);
        });
        let s2 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![1, 3], 13u32);
        });
        let s3 = thread::spawn(move || {
            traceforge::send_vec_tagged_msg(inbox_tid, vec![], 0u32);
        });

        let _ = s1.join();
        let _ = s2.join();
        let _ = s3.join();
        let _ = inbox_thread.join();
    });

    // min=0: either timeout (empty) or the single matching send.
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}
