use traceforge::{recv_msg_block, send_msg, symbolic, thread, verify, Config};

fn example() {
    let main_id = thread::current_id();

    let worker1 = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();
        send_msg(main_id, v);
    });

    let worker2 = thread::spawn(move || {
        let v: symbolic::SymExpr = recv_msg_block();
        send_msg(main_id, v);
    });

    let worker1_id = worker1.thread().id();
    let worker2_id = worker2.thread().id();

    let b = symbolic::fresh_bool();
    send_msg(worker1_id, b.clone());
    send_msg(worker2_id, b);

    let x1: symbolic::SymExpr = recv_msg_block();
    let x2: symbolic::SymExpr = recv_msg_block();

    assert_eq!(x1, x2);
    symbolic::assert(x1.equals(x2));

    worker1.join().unwrap();
    worker2.join().unwrap();
}

fn main() {
    let stats = verify(Config::builder().with_symbolic(true).build(), example);
    println!("Stats = {}, {}", stats.execs, stats.block);
}
