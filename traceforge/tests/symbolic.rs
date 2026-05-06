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

    assert_eq!((stats.execs, stats.block), (3, 0));
}

#[test]
fn symbolic_forall_reflexive_equality_is_valid() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");

        symbolic::assert(symbolic::forall([("x", node)], |vars| {
            let x = vars.get("x");
            x.clone().equals(x)
        }));
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn symbolic_exists_constant_equality_is_satisfiable() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let root = symbolic::constant("root", node.clone());

        symbolic::assume(symbolic::exists([("x", node)], |vars| {
            vars.get("x").equals(root)
        }));
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn symbolic_quantified_predicate_explores_both_branch_outcomes() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let marked = symbolic::predicate("marked", &[node.clone()]);

        let all_marked = symbolic::forall([("x", node)], |vars| marked.apply([vars.get("x")]));

        if symbolic::eval(all_marked) {
            // true branch
        } else {
            // false branch
        }
    });

    assert_eq!((stats.execs, stats.block), (2, 0));
}

#[test]
fn symbolic_quantifier_detects_invalid_assertion() {
    let stats = verify(symbolic_keep_going_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let marked = symbolic::predicate("marked", &[node.clone()]);
        let root = symbolic::constant("root", node.clone());

        symbolic::assert(
            symbolic::forall([("x", node)], |vars| marked.apply([vars.get("x")]))
                .and(marked.apply([root]).not()),
        );
    });

    assert_eq!((stats.execs, stats.block), (0, 1));
}

#[test]
fn symbolic_nested_quantifier_can_reference_outer_variable() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let same = symbolic::predicate("same", &[node.clone(), node.clone()]);

        symbolic::assert(symbolic::forall([("x", node.clone())], |outer| {
            outer.exists([("y", node)], |inner| {
                let x = inner.get("x");
                let y = inner.get("y");
                x.clone()
                    .equals(y.clone())
                    .and(same.apply([x, y]).implies(symbolic::bool_val(true)))
            })
        }));
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
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

    assert_eq!(stats.execs, 1);
}

#[test]
fn symbolic_assume_unsatisfiable_blocks_execution() {
    let stats = verify(symbolic_config(), || {
        let x = symbolic::fresh_int();
        symbolic::assume(x.clone().gt(0).and(x.lt(0)));
    });

    assert_eq!(stats.execs, 0);
    assert_eq!(stats.block, 1);
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
    assert!(stats.execs >= 2,);
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

    assert!(stats.block > 0,);
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

    assert!(stats.block > 0);
}

#[test]
fn symbolic_uninterpreted_sort_supports_equality() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let x = symbolic::fresh(node.clone());
        let y = symbolic::fresh(node);

        symbolic::assert(x.clone().equals(y.clone()).implies(y.equals(x)));
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn symbolic_uninterpreted_function_preserves_congruence() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let parent = symbolic::uf("parent", &[node.clone()], node.clone());

        let x = symbolic::fresh(node.clone());
        let y = symbolic::fresh(node);

        symbolic::assert(
            x.clone()
                .equals(y.clone())
                .implies(parent.apply([x]).equals(parent.apply([y]))),
        );
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn symbolic_uninterpreted_predicate_explores_both_branch_outcomes() {
    let stats = verify(symbolic_config(), || {
        let node = symbolic::uninterpreted_sort("Node");
        let marked = symbolic::predicate("marked", &[node.clone()]);
        let x = symbolic::fresh(node);

        if symbolic::eval(marked.apply([x])) {
            // true branch
        } else {
            // false branch
        }
    });

    assert_eq!((stats.execs, stats.block), (2, 0));
}

#[test]
fn symbolic_uninterpreted_function_application_can_be_sent() {
    let stats = verify(symbolic_config(), || {
        let main_id = thread::current_id();
        let node = symbolic::uninterpreted_sort("Node");
        let parent = symbolic::uf("parent", &[node.clone()], node.clone());

        let worker = thread::spawn(move || {
            let v: symbolic::SymExpr = recv_msg_block();
            send_msg(main_id, v);
        });

        let x = symbolic::fresh(node);
        let px = parent.apply([x]);
        send_msg(worker.thread().id(), px.clone());

        let echoed: symbolic::SymExpr = recv_msg_block();
        symbolic::assert(echoed.equals(px));

        worker.join().unwrap();
    });

    assert_eq!((stats.execs, stats.block), (1, 0));
}

#[test]
fn symbolic_backward_revisit_with_uninterpreted_predicate_is_optimal() {
    let stats = verify(symbolic_ltr_config(), || {
        let main_id = thread::current_id();
        let node = symbolic::uninterpreted_sort("Node");
        let ready = symbolic::predicate("ready", &[node.clone()]);
        let ready_for_worker = ready.clone();

        let worker = thread::spawn(move || {
            let x = symbolic::fresh(node);
            if symbolic::eval(ready_for_worker.apply([x.clone()])) {
                send_msg(main_id, x);
            }
        });

        let received: Option<symbolic::SymExpr> = recv_msg();
        if let Some(x) = received {
            symbolic::assert(ready.apply([x]));
        }

        worker.join().unwrap();
    });

    assert_eq!((stats.execs, stats.block), (3, 0));
}
