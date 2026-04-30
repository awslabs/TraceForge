use traceforge::{
    recv_msg, recv_msg_block, send_msg, symbolic, thread, verify, Config, SchedulePolicy,
};

fn symbolic_config() -> Config {
    Config::builder().with_symbolic(true).build()
}

fn symbolic_ltr_config() -> Config {
    Config::builder()
        .with_policy(SchedulePolicy::LTR)
        .with_symbolic(true)
        .build()
}

fn symbolic_keep_going_config() -> Config {
    Config::builder()
        .with_symbolic(true)
        .with_keep_going_after_error(true)
        .build()
}

#[test]
fn symbolic_backward_revisit_for_right_side_send_is_optimal() {
    let stats = verify(symbolic_ltr_config(), || {
        let main_id = thread::current_id();

        let worker = thread::spawn(move || {
            let x = symbolic::fresh_int();
            if symbolic::eval(x.clone().gt(symbolic::int_val(0))) {
                send_msg(main_id, x);
            }
        });

        let received: Option<symbolic::SymExpr> = recv_msg();
        if let Some(x) = received {
            symbolic::assert(x.gt(symbolic::int_val(0)));
        }

        worker.join().unwrap();
    });

    assert_eq!(
        (stats.execs, stats.block),
        (3, 0),
        "expected one false branch plus the two optimal true-branch receive outcomes, got {stats:?}"
    );
}

#[test]
fn symbolic_echo_transports_formula() {
    let stats = verify(symbolic_config(), || {
        let main_id = thread::current_id();

        let worker1 = thread::spawn(move || {
            let v: symbolic::SymExpr = recv_msg_block();
            send_msg(main_id, v);
        });

        let worker2 = thread::spawn(move || {
            let v: symbolic::SymExpr = recv_msg_block();
            send_msg(main_id, v);
        });

        let b = symbolic::fresh_bool();
        send_msg(worker1.thread().id(), b.clone());
        send_msg(worker2.thread().id(), b);

        let x1: symbolic::SymExpr = recv_msg_block();
        let x2: symbolic::SymExpr = recv_msg_block();

        assert_eq!(x1, x2);
        symbolic::assert(x1.equals(x2));

        worker1.join().unwrap();
        worker2.join().unwrap();
    });

    assert_eq!(stats.block, 0);
    assert!(stats.execs > 0);
}

#[test]
fn symbolic_expr_supports_arithmetic_and_boolean_helpers() {
    let stats = verify(symbolic_config(), || {
        let x = symbolic::fresh_int();
        let y = symbolic::fresh_int();
        let guard = x
            .clone()
            .ge(4)
            .and(y.clone().le(10))
            .and(((x.clone() * 2) - 1).lt(y.clone().div(2) + 20))
            .and(x.clone().sub(1).mul(3).ge(0))
            .implies((y / 2).ge(x % 3));

        symbolic::assume(guard);
    });

    assert!(stats.execs > 0);
}

#[test]
fn symbolic_forward_revisit_explores_both_branch_outcomes() {
    let stats = verify(symbolic_config(), || {
        let main_id = thread::current_id();

        let worker = thread::spawn(move || {
            let v: symbolic::SymExpr = recv_msg_block();
            let even = symbolic::eval((v % 2).equals(symbolic::int_val(0)));
            send_msg(main_id, even);
        });

        let i = symbolic::fresh_int();
        send_msg(worker.thread().id(), i.clone());

        let even: bool = recv_msg_block();
        if even {
            symbolic::assert((i % 2).equals(symbolic::int_val(0)));
        }

        worker.join().unwrap();
    });

    assert_eq!(stats.block, 0);
    assert!(
        stats.execs >= 2,
        "expected true and false symbolic branch executions, got {stats:?}"
    );
}

fn buggy_control_flow_program() {
    let main_id = thread::current_id();

    let worker = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();
        let positive = symbolic::eval(v.gt(symbolic::int_val(0)));
        send_msg(main_id, positive);
    });

    let i = symbolic::fresh_int();
    send_msg(worker.thread().id(), i.clone());

    let positive: bool = recv_msg_block();
    if positive {
        symbolic::assert((i % 2).equals(symbolic::int_val(0)));
    }

    worker.join().unwrap();
}

#[test]
#[should_panic]
fn symbolic_buggy_control_flow_panics_on_feasible_assert_failure() {
    verify(symbolic_config(), buggy_control_flow_program);
}

#[test]
fn symbolic_buggy_control_flow_records_failure_when_keep_going() {
    let stats = verify(symbolic_keep_going_config(), buggy_control_flow_program);

    assert!(
        stats.block > 0,
        "expected a feasible assertion failure for i = 1, got {stats:?}"
    );
}

#[test]
fn symbolic_receive_order_revisit_can_expose_order_sensitive_bug() {
    let stats = verify(symbolic_keep_going_config(), || {
        let main_id = thread::current_id();

        let worker1 = thread::spawn(move || {
            let x: symbolic::SymExpr = recv_msg_block();
            send_msg(main_id, x);
        });

        let worker2 = thread::spawn(move || {
            let y: symbolic::SymExpr = recv_msg_block();
            send_msg(main_id, y);
        });

        let base = symbolic::fresh_int();
        send_msg(worker1.thread().id(), base.clone());
        send_msg(worker2.thread().id(), base + 1);

        let first: symbolic::SymExpr = recv_msg_block();
        let second: symbolic::SymExpr = recv_msg_block();

        symbolic::assert(second.equals(first + 1));

        worker1.join().unwrap();
        worker2.join().unwrap();
    });

    assert!(
        stats.block > 0,
        "expected reversed receive order to violate second == first + 1, got {stats:?}"
    );
}
