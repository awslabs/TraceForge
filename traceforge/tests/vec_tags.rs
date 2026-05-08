extern crate traceforge;

use traceforge::thread;
use traceforge::*;
use SchedulePolicy::*;

#[derive(Clone, PartialEq, Debug)]
enum Msg {
    Val(i32),
}

#[test]
fn vec_tag_basic_and_empty_is_none() {
    let stats = traceforge::verify(
        Config::builder()
            .with_policy(LTR)
            .with_cons_type(ConsType::FIFO)
            .build(),
        || {
            let receiver = thread::spawn(move || {
                let v_exact = match traceforge::recv_vec_tagged_msg_block(move |_, tag| {
                    tag == Some(vec![7, 1])
                }) {
                    Msg::Val(v) => v,
                };
                assert_eq!(v_exact, 71);

                let v_none = match traceforge::recv_vec_tagged_msg_block(move |_, tag| tag.is_none())
                {
                    Msg::Val(v) => v,
                };
                assert_eq!(v_none, 0);
            });

            traceforge::send_vec_tagged_msg(receiver.thread().id(), vec![], Msg::Val(0));
            traceforge::send_vec_tagged_msg(receiver.thread().id(), vec![7, 1], Msg::Val(71));

            let _ = receiver.join();
        },
    );

    assert_eq!(stats.execs, 1);
}

#[test]
fn vec_tag_lexicographic_predicate() {
    let stats = traceforge::verify(
        Config::builder()
            .with_policy(LTR)
            .with_cons_type(ConsType::FIFO)
            .build(),
        || {
            let receiver = thread::spawn(move || {
                let low: Msg = traceforge::recv_vec_tagged_msg_block(move |_, tag| {
                    tag.is_some_and(|t| t < vec![2, 0])
                });
                assert_eq!(low, Msg::Val(15));

                let high: Msg = traceforge::recv_vec_tagged_msg_block(move |_, tag| {
                    tag.is_some_and(|t| t >= vec![2, 0])
                });
                assert_eq!(high, Msg::Val(20));
            });

            traceforge::send_vec_tagged_msg(receiver.thread().id(), vec![2, 0], Msg::Val(20));
            traceforge::send_vec_tagged_msg(receiver.thread().id(), vec![1, 5], Msg::Val(15));

            let _ = receiver.join();
        },
    );

    assert_eq!(stats.execs, 1);
}
