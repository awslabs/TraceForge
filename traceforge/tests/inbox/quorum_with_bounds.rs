use std::collections::HashMap;
use std::sync::Arc;

use traceforge::{self, thread, Config};

fn make_sender_values(senders: u32, value_nbr: u32, sender_with_some_val: u32) -> Vec<u32> {
    assert!(value_nbr >= 1);
    assert!(sender_with_some_val <= senders);
    let dominant = 0u32;
    let mut vals = Vec::with_capacity(senders as usize);
    for _ in 0..sender_with_some_val {
        vals.push(dominant);
    }
    let mut next = 1u32;
    for _ in sender_with_some_val..senders {
        if value_nbr == 1 {
            vals.push(dominant);
        } else {
            vals.push(next);
            next = if next + 1 < value_nbr { next + 1 } else { 1 };
        }
    }
    vals
}

fn decide_value(messages: Vec<Option<traceforge::Val>>) -> u32 {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for msg in messages.into_iter().flatten() {
        let v = *msg
            .as_any_ref()
            .downcast_ref::<u32>()
            .expect("expected u32 message");
        *counts.entry(v).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|(v1, c1), (v2, c2)| c1.cmp(c2).then_with(|| v2.cmp(v1).reverse()))
        .map(|(v, _)| v)
        .unwrap_or(0)
}

fn run_quorum_scenario(
    inb_tds: u32,
    sender_nbr: u32,
    value_nbr: u32,
    sender_with_some_val: u32,
) -> (usize, usize) {
    let sender_values = Arc::new(make_sender_values(
        sender_nbr,
        value_nbr,
        sender_with_some_val,
    ));

    let stats = traceforge::verify(Config::builder().build(), move || {
        assert!(inb_tds >= 1);

        let thread0 = thread::spawn(move || {
            let msgs = traceforge::inbox_with_tag_and_bounds(
                |_, tag| tag == Some(1),
                sender_nbr as usize,
                None,
            );
            let decided = decide_value(msgs);

            let confirmations = traceforge::inbox_with_tag_and_bounds(
                |_, tag| tag == Some(2),
                (inb_tds - 1) as usize,
                None,
            );
            for msg in confirmations.into_iter().flatten() {
                let v = *msg
                    .as_any_ref()
                    .downcast_ref::<u32>()
                    .expect("expected u32 decision");
                assert_eq!(
                    v, decided,
                    "all confirmations should match the first inbox decision"
                );
            }
            decided
        });
        let tid0 = thread0.thread().id();

        let mut other_inbox_handles = Vec::new();
        let mut inbox_ids = vec![tid0];
        for _ in 1..inb_tds {
            let tid0 = tid0.clone();
            let handle = thread::spawn(move || {
                let msgs = traceforge::inbox_with_tag_and_bounds(
                    |_, tag| tag == Some(1),
                    sender_nbr as usize,
                    None,
                );
                let decided = decide_value(msgs);
                traceforge::send_tagged_msg(tid0, 2, decided);
                decided
            });
            inbox_ids.push(handle.thread().id());
            other_inbox_handles.push(handle);
        }

        let inbox_ids = inbox_ids;

        let mut sender_handles = Vec::new();
        let sender_values = sender_values.clone();
        for (idx, val) in sender_values.iter().cloned().enumerate() {
            let inbox_ids = inbox_ids.clone();
            sender_handles.push(thread::spawn(move || {
                for tid in inbox_ids {
                    traceforge::send_tagged_msg(tid, 1, val);
                }
                (idx, val)
            }));
        }

        for h in sender_handles {
            let _ = h.join();
        }

        for h in other_inbox_handles {
            let _ = h.join();
        }
        let _ = thread0.join();
    });

    (stats.execs, stats.block)
}

macro_rules! quorum_test {
    ($name:ident, $inb_tds:expr, $senders:expr, $values:expr, $dominant:expr) => {
        #[test]
        fn $name() {
            let (execs, blocked) = run_quorum_scenario($inb_tds, $senders, $values, $dominant);
            assert_eq!(execs, 1);
            assert_eq!(blocked, 0);
        }
    };
}

quorum_test!(quorum_i1_s0_v1_d0, 1, 0, 1, 0);
quorum_test!(quorum_i1_s0_v2_d0, 1, 0, 2, 0);
quorum_test!(quorum_i1_s0_v3_d0, 1, 0, 3, 0);
quorum_test!(quorum_i1_s0_v4_d0, 1, 0, 4, 0);
quorum_test!(quorum_i1_s1_v1_d0, 1, 1, 1, 0);
quorum_test!(quorum_i1_s1_v1_d1, 1, 1, 1, 1);
quorum_test!(quorum_i1_s1_v2_d0, 1, 1, 2, 0);
quorum_test!(quorum_i1_s1_v2_d1, 1, 1, 2, 1);
quorum_test!(quorum_i1_s1_v3_d0, 1, 1, 3, 0);
quorum_test!(quorum_i1_s1_v3_d1, 1, 1, 3, 1);
quorum_test!(quorum_i1_s1_v4_d0, 1, 1, 4, 0);
quorum_test!(quorum_i1_s1_v4_d1, 1, 1, 4, 1);
quorum_test!(quorum_i1_s2_v1_d0, 1, 2, 1, 0);
quorum_test!(quorum_i1_s2_v1_d1, 1, 2, 1, 1);
quorum_test!(quorum_i1_s2_v1_d2, 1, 2, 1, 2);
quorum_test!(quorum_i1_s2_v2_d0, 1, 2, 2, 0);
quorum_test!(quorum_i1_s2_v2_d1, 1, 2, 2, 1);
quorum_test!(quorum_i1_s2_v2_d2, 1, 2, 2, 2);
quorum_test!(quorum_i1_s2_v3_d0, 1, 2, 3, 0);
quorum_test!(quorum_i1_s2_v3_d1, 1, 2, 3, 1);
quorum_test!(quorum_i1_s2_v3_d2, 1, 2, 3, 2);
quorum_test!(quorum_i1_s2_v4_d0, 1, 2, 4, 0);
quorum_test!(quorum_i1_s2_v4_d1, 1, 2, 4, 1);
quorum_test!(quorum_i1_s2_v4_d2, 1, 2, 4, 2);
quorum_test!(quorum_i1_s3_v1_d0, 1, 3, 1, 0);
quorum_test!(quorum_i1_s3_v1_d1, 1, 3, 1, 1);
quorum_test!(quorum_i1_s3_v1_d2, 1, 3, 1, 2);
quorum_test!(quorum_i1_s3_v1_d3, 1, 3, 1, 3);
quorum_test!(quorum_i1_s3_v2_d0, 1, 3, 2, 0);
quorum_test!(quorum_i1_s3_v2_d1, 1, 3, 2, 1);
quorum_test!(quorum_i1_s3_v2_d2, 1, 3, 2, 2);
quorum_test!(quorum_i1_s3_v2_d3, 1, 3, 2, 3);
quorum_test!(quorum_i1_s3_v3_d0, 1, 3, 3, 0);
quorum_test!(quorum_i1_s3_v3_d1, 1, 3, 3, 1);
quorum_test!(quorum_i1_s3_v3_d2, 1, 3, 3, 2);
quorum_test!(quorum_i1_s3_v3_d3, 1, 3, 3, 3);
quorum_test!(quorum_i1_s3_v4_d0, 1, 3, 4, 0);
quorum_test!(quorum_i1_s3_v4_d1, 1, 3, 4, 1);
quorum_test!(quorum_i1_s3_v4_d2, 1, 3, 4, 2);
quorum_test!(quorum_i1_s3_v4_d3, 1, 3, 4, 3);
quorum_test!(quorum_i1_s4_v1_d0, 1, 4, 1, 0);
quorum_test!(quorum_i1_s4_v1_d1, 1, 4, 1, 1);
quorum_test!(quorum_i1_s4_v1_d2, 1, 4, 1, 2);
quorum_test!(quorum_i1_s4_v1_d3, 1, 4, 1, 3);
quorum_test!(quorum_i1_s4_v1_d4, 1, 4, 1, 4);
quorum_test!(quorum_i1_s4_v2_d0, 1, 4, 2, 0);
quorum_test!(quorum_i1_s4_v2_d1, 1, 4, 2, 1);
quorum_test!(quorum_i1_s4_v2_d2, 1, 4, 2, 2);
quorum_test!(quorum_i1_s4_v2_d3, 1, 4, 2, 3);
quorum_test!(quorum_i1_s4_v2_d4, 1, 4, 2, 4);
quorum_test!(quorum_i1_s4_v3_d0, 1, 4, 3, 0);
quorum_test!(quorum_i1_s4_v3_d1, 1, 4, 3, 1);
quorum_test!(quorum_i1_s4_v3_d2, 1, 4, 3, 2);
quorum_test!(quorum_i1_s4_v3_d3, 1, 4, 3, 3);
quorum_test!(quorum_i1_s4_v3_d4, 1, 4, 3, 4);
quorum_test!(quorum_i1_s4_v4_d0, 1, 4, 4, 0);
quorum_test!(quorum_i1_s4_v4_d1, 1, 4, 4, 1);
quorum_test!(quorum_i1_s4_v4_d2, 1, 4, 4, 2);
quorum_test!(quorum_i1_s4_v4_d3, 1, 4, 4, 3);
quorum_test!(quorum_i1_s4_v4_d4, 1, 4, 4, 4);
quorum_test!(quorum_i2_s0_v1_d0, 2, 0, 1, 0);
quorum_test!(quorum_i2_s0_v2_d0, 2, 0, 2, 0);
quorum_test!(quorum_i2_s0_v3_d0, 2, 0, 3, 0);
quorum_test!(quorum_i2_s0_v4_d0, 2, 0, 4, 0);
quorum_test!(quorum_i2_s1_v1_d0, 2, 1, 1, 0);
quorum_test!(quorum_i2_s1_v1_d1, 2, 1, 1, 1);
quorum_test!(quorum_i2_s1_v2_d0, 2, 1, 2, 0);
quorum_test!(quorum_i2_s1_v2_d1, 2, 1, 2, 1);
quorum_test!(quorum_i2_s1_v3_d0, 2, 1, 3, 0);
quorum_test!(quorum_i2_s1_v3_d1, 2, 1, 3, 1);
quorum_test!(quorum_i2_s1_v4_d0, 2, 1, 4, 0);
quorum_test!(quorum_i2_s1_v4_d1, 2, 1, 4, 1);
quorum_test!(quorum_i2_s2_v1_d0, 2, 2, 1, 0);
quorum_test!(quorum_i2_s2_v1_d1, 2, 2, 1, 1);
quorum_test!(quorum_i2_s2_v1_d2, 2, 2, 1, 2);
quorum_test!(quorum_i2_s2_v2_d0, 2, 2, 2, 0);
quorum_test!(quorum_i2_s2_v2_d1, 2, 2, 2, 1);
quorum_test!(quorum_i2_s2_v2_d2, 2, 2, 2, 2);
quorum_test!(quorum_i2_s2_v3_d0, 2, 2, 3, 0);
quorum_test!(quorum_i2_s2_v3_d1, 2, 2, 3, 1);
quorum_test!(quorum_i2_s2_v3_d2, 2, 2, 3, 2);
quorum_test!(quorum_i2_s2_v4_d0, 2, 2, 4, 0);
quorum_test!(quorum_i2_s2_v4_d1, 2, 2, 4, 1);
quorum_test!(quorum_i2_s2_v4_d2, 2, 2, 4, 2);
quorum_test!(quorum_i2_s3_v1_d0, 2, 3, 1, 0);
quorum_test!(quorum_i2_s3_v1_d1, 2, 3, 1, 1);
quorum_test!(quorum_i2_s3_v1_d2, 2, 3, 1, 2);
quorum_test!(quorum_i2_s3_v1_d3, 2, 3, 1, 3);
quorum_test!(quorum_i2_s3_v2_d0, 2, 3, 2, 0);
quorum_test!(quorum_i2_s3_v2_d1, 2, 3, 2, 1);
quorum_test!(quorum_i2_s3_v2_d2, 2, 3, 2, 2);
quorum_test!(quorum_i2_s3_v2_d3, 2, 3, 2, 3);
quorum_test!(quorum_i2_s3_v3_d0, 2, 3, 3, 0);
quorum_test!(quorum_i2_s3_v3_d1, 2, 3, 3, 1);
quorum_test!(quorum_i2_s3_v3_d2, 2, 3, 3, 2);
quorum_test!(quorum_i2_s3_v3_d3, 2, 3, 3, 3);
quorum_test!(quorum_i2_s3_v4_d0, 2, 3, 4, 0);
quorum_test!(quorum_i2_s3_v4_d1, 2, 3, 4, 1);
quorum_test!(quorum_i2_s3_v4_d2, 2, 3, 4, 2);
quorum_test!(quorum_i2_s3_v4_d3, 2, 3, 4, 3);
quorum_test!(quorum_i2_s4_v1_d0, 2, 4, 1, 0);
quorum_test!(quorum_i2_s4_v1_d1, 2, 4, 1, 1);
quorum_test!(quorum_i2_s4_v1_d2, 2, 4, 1, 2);
quorum_test!(quorum_i2_s4_v1_d3, 2, 4, 1, 3);
quorum_test!(quorum_i2_s4_v1_d4, 2, 4, 1, 4);
quorum_test!(quorum_i2_s4_v2_d0, 2, 4, 2, 0);
quorum_test!(quorum_i2_s4_v2_d1, 2, 4, 2, 1);
quorum_test!(quorum_i2_s4_v2_d2, 2, 4, 2, 2);
quorum_test!(quorum_i2_s4_v2_d3, 2, 4, 2, 3);
quorum_test!(quorum_i2_s4_v2_d4, 2, 4, 2, 4);
quorum_test!(quorum_i2_s4_v3_d0, 2, 4, 3, 0);
quorum_test!(quorum_i2_s4_v3_d1, 2, 4, 3, 1);
quorum_test!(quorum_i2_s4_v3_d2, 2, 4, 3, 2);
quorum_test!(quorum_i2_s4_v3_d3, 2, 4, 3, 3);
quorum_test!(quorum_i2_s4_v3_d4, 2, 4, 3, 4);
quorum_test!(quorum_i2_s4_v4_d0, 2, 4, 4, 0);
quorum_test!(quorum_i2_s4_v4_d1, 2, 4, 4, 1);
quorum_test!(quorum_i2_s4_v4_d2, 2, 4, 4, 2);
quorum_test!(quorum_i2_s4_v4_d3, 2, 4, 4, 3);
quorum_test!(quorum_i2_s4_v4_d4, 2, 4, 4, 4);
quorum_test!(quorum_i3_s0_v1_d0, 3, 0, 1, 0);
quorum_test!(quorum_i3_s0_v2_d0, 3, 0, 2, 0);
quorum_test!(quorum_i3_s0_v3_d0, 3, 0, 3, 0);
quorum_test!(quorum_i3_s0_v4_d0, 3, 0, 4, 0);
quorum_test!(quorum_i3_s1_v1_d0, 3, 1, 1, 0);
quorum_test!(quorum_i3_s1_v1_d1, 3, 1, 1, 1);
quorum_test!(quorum_i3_s1_v2_d0, 3, 1, 2, 0);
quorum_test!(quorum_i3_s1_v2_d1, 3, 1, 2, 1);
quorum_test!(quorum_i3_s1_v3_d0, 3, 1, 3, 0);
quorum_test!(quorum_i3_s1_v3_d1, 3, 1, 3, 1);
quorum_test!(quorum_i3_s1_v4_d0, 3, 1, 4, 0);
quorum_test!(quorum_i3_s1_v4_d1, 3, 1, 4, 1);
quorum_test!(quorum_i3_s2_v1_d0, 3, 2, 1, 0);
quorum_test!(quorum_i3_s2_v1_d1, 3, 2, 1, 1);
quorum_test!(quorum_i3_s2_v1_d2, 3, 2, 1, 2);
quorum_test!(quorum_i3_s2_v2_d0, 3, 2, 2, 0);
quorum_test!(quorum_i3_s2_v2_d1, 3, 2, 2, 1);
quorum_test!(quorum_i3_s2_v2_d2, 3, 2, 2, 2);
quorum_test!(quorum_i3_s2_v3_d0, 3, 2, 3, 0);
quorum_test!(quorum_i3_s2_v3_d1, 3, 2, 3, 1);
quorum_test!(quorum_i3_s2_v3_d2, 3, 2, 3, 2);
quorum_test!(quorum_i3_s2_v4_d0, 3, 2, 4, 0);
quorum_test!(quorum_i3_s2_v4_d1, 3, 2, 4, 1);
quorum_test!(quorum_i3_s2_v4_d2, 3, 2, 4, 2);
quorum_test!(quorum_i3_s3_v1_d0, 3, 3, 1, 0);
quorum_test!(quorum_i3_s3_v1_d1, 3, 3, 1, 1);
quorum_test!(quorum_i3_s3_v1_d2, 3, 3, 1, 2);
quorum_test!(quorum_i3_s3_v1_d3, 3, 3, 1, 3);
quorum_test!(quorum_i3_s3_v2_d0, 3, 3, 2, 0);
quorum_test!(quorum_i3_s3_v2_d1, 3, 3, 2, 1);
quorum_test!(quorum_i3_s3_v2_d2, 3, 3, 2, 2);
quorum_test!(quorum_i3_s3_v2_d3, 3, 3, 2, 3);
quorum_test!(quorum_i3_s3_v3_d0, 3, 3, 3, 0);
quorum_test!(quorum_i3_s3_v3_d1, 3, 3, 3, 1);
quorum_test!(quorum_i3_s3_v3_d2, 3, 3, 3, 2);
quorum_test!(quorum_i3_s3_v3_d3, 3, 3, 3, 3);
quorum_test!(quorum_i3_s3_v4_d0, 3, 3, 4, 0);
quorum_test!(quorum_i3_s3_v4_d1, 3, 3, 4, 1);
quorum_test!(quorum_i3_s3_v4_d2, 3, 3, 4, 2);
quorum_test!(quorum_i3_s3_v4_d3, 3, 3, 4, 3);
quorum_test!(quorum_i3_s4_v1_d0, 3, 4, 1, 0);
quorum_test!(quorum_i3_s4_v1_d1, 3, 4, 1, 1);
quorum_test!(quorum_i3_s4_v1_d2, 3, 4, 1, 2);
quorum_test!(quorum_i3_s4_v1_d3, 3, 4, 1, 3);
quorum_test!(quorum_i3_s4_v1_d4, 3, 4, 1, 4);
quorum_test!(quorum_i3_s4_v2_d0, 3, 4, 2, 0);
quorum_test!(quorum_i3_s4_v2_d1, 3, 4, 2, 1);
quorum_test!(quorum_i3_s4_v2_d2, 3, 4, 2, 2);
quorum_test!(quorum_i3_s4_v2_d3, 3, 4, 2, 3);
quorum_test!(quorum_i3_s4_v2_d4, 3, 4, 2, 4);
quorum_test!(quorum_i3_s4_v3_d0, 3, 4, 3, 0);
quorum_test!(quorum_i3_s4_v3_d1, 3, 4, 3, 1);
quorum_test!(quorum_i3_s4_v3_d2, 3, 4, 3, 2);
quorum_test!(quorum_i3_s4_v3_d3, 3, 4, 3, 3);
quorum_test!(quorum_i3_s4_v3_d4, 3, 4, 3, 4);
quorum_test!(quorum_i3_s4_v4_d0, 3, 4, 4, 0);
quorum_test!(quorum_i3_s4_v4_d1, 3, 4, 4, 1);
quorum_test!(quorum_i3_s4_v4_d2, 3, 4, 4, 2);
quorum_test!(quorum_i3_s4_v4_d3, 3, 4, 4, 3);
quorum_test!(quorum_i3_s4_v4_d4, 3, 4, 4, 4);
quorum_test!(quorum_i4_s0_v1_d0, 4, 0, 1, 0);
quorum_test!(quorum_i4_s0_v2_d0, 4, 0, 2, 0);
quorum_test!(quorum_i4_s0_v3_d0, 4, 0, 3, 0);
quorum_test!(quorum_i4_s0_v4_d0, 4, 0, 4, 0);
quorum_test!(quorum_i4_s1_v1_d0, 4, 1, 1, 0);
quorum_test!(quorum_i4_s1_v1_d1, 4, 1, 1, 1);
quorum_test!(quorum_i4_s1_v2_d0, 4, 1, 2, 0);
quorum_test!(quorum_i4_s1_v2_d1, 4, 1, 2, 1);
quorum_test!(quorum_i4_s1_v3_d0, 4, 1, 3, 0);
quorum_test!(quorum_i4_s1_v3_d1, 4, 1, 3, 1);
quorum_test!(quorum_i4_s1_v4_d0, 4, 1, 4, 0);
quorum_test!(quorum_i4_s1_v4_d1, 4, 1, 4, 1);
quorum_test!(quorum_i4_s2_v1_d0, 4, 2, 1, 0);
quorum_test!(quorum_i4_s2_v1_d1, 4, 2, 1, 1);
quorum_test!(quorum_i4_s2_v1_d2, 4, 2, 1, 2);
quorum_test!(quorum_i4_s2_v2_d0, 4, 2, 2, 0);
quorum_test!(quorum_i4_s2_v2_d1, 4, 2, 2, 1);
quorum_test!(quorum_i4_s2_v2_d2, 4, 2, 2, 2);
quorum_test!(quorum_i4_s2_v3_d0, 4, 2, 3, 0);
quorum_test!(quorum_i4_s2_v3_d1, 4, 2, 3, 1);
quorum_test!(quorum_i4_s2_v3_d2, 4, 2, 3, 2);
quorum_test!(quorum_i4_s2_v4_d0, 4, 2, 4, 0);
quorum_test!(quorum_i4_s2_v4_d1, 4, 2, 4, 1);
quorum_test!(quorum_i4_s2_v4_d2, 4, 2, 4, 2);
quorum_test!(quorum_i4_s3_v1_d0, 4, 3, 1, 0);
quorum_test!(quorum_i4_s3_v1_d1, 4, 3, 1, 1);
quorum_test!(quorum_i4_s3_v1_d2, 4, 3, 1, 2);
quorum_test!(quorum_i4_s3_v1_d3, 4, 3, 1, 3);
quorum_test!(quorum_i4_s3_v2_d0, 4, 3, 2, 0);
quorum_test!(quorum_i4_s3_v2_d1, 4, 3, 2, 1);
quorum_test!(quorum_i4_s3_v2_d2, 4, 3, 2, 2);
quorum_test!(quorum_i4_s3_v2_d3, 4, 3, 2, 3);
quorum_test!(quorum_i4_s3_v3_d0, 4, 3, 3, 0);
quorum_test!(quorum_i4_s3_v3_d1, 4, 3, 3, 1);
quorum_test!(quorum_i4_s3_v3_d2, 4, 3, 3, 2);
quorum_test!(quorum_i4_s3_v3_d3, 4, 3, 3, 3);
quorum_test!(quorum_i4_s3_v4_d0, 4, 3, 4, 0);
quorum_test!(quorum_i4_s3_v4_d1, 4, 3, 4, 1);
quorum_test!(quorum_i4_s3_v4_d2, 4, 3, 4, 2);
quorum_test!(quorum_i4_s3_v4_d3, 4, 3, 4, 3);
quorum_test!(quorum_i4_s4_v1_d0, 4, 4, 1, 0);
quorum_test!(quorum_i4_s4_v1_d1, 4, 4, 1, 1);
quorum_test!(quorum_i4_s4_v1_d2, 4, 4, 1, 2);
quorum_test!(quorum_i4_s4_v1_d3, 4, 4, 1, 3);
quorum_test!(quorum_i4_s4_v1_d4, 4, 4, 1, 4);
quorum_test!(quorum_i4_s4_v2_d0, 4, 4, 2, 0);
quorum_test!(quorum_i4_s4_v2_d1, 4, 4, 2, 1);
quorum_test!(quorum_i4_s4_v2_d2, 4, 4, 2, 2);
quorum_test!(quorum_i4_s4_v2_d3, 4, 4, 2, 3);
quorum_test!(quorum_i4_s4_v2_d4, 4, 4, 2, 4);
quorum_test!(quorum_i4_s4_v3_d0, 4, 4, 3, 0);
quorum_test!(quorum_i4_s4_v3_d1, 4, 4, 3, 1);
quorum_test!(quorum_i4_s4_v3_d2, 4, 4, 3, 2);
quorum_test!(quorum_i4_s4_v3_d3, 4, 4, 3, 3);
quorum_test!(quorum_i4_s4_v3_d4, 4, 4, 3, 4);
quorum_test!(quorum_i4_s4_v4_d0, 4, 4, 4, 0);
quorum_test!(quorum_i4_s4_v4_d1, 4, 4, 4, 1);
quorum_test!(quorum_i4_s4_v4_d2, 4, 4, 4, 2);
quorum_test!(quorum_i4_s4_v4_d3, 4, 4, 4, 3);
quorum_test!(quorum_i4_s4_v4_d4, 4, 4, 4, 4);
