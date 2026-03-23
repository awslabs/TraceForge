use traceforge::{sync::mpsc::*, *};
// this file shows some example usage of the Must's `sync::mpsc` library

#[test]
fn single_producer() {
    let f = || {
        let (tx, rx) = channel(100);
        let _t = thread::spawn(move || {
            traceforge::future::block_on(async {
                let _ = tx.send(10).await;
                let _ = tx.send(20).await;
                let _ = tx.send(30).await;
            })
        });
        // adding the following line would bring down the finished execution to 1 and blocked execution to 0, why?
        // t.join().unwrap();
        assert(rx.try_recv().unwrap() == 10);
        assert(rx.try_recv().unwrap() == 20);
        assert(rx.try_recv().unwrap() == 30);
    };

    let stats = verify(
        Config::builder().with_keep_going_after_error(false).build(),
        f,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}

#[test]
fn multiple_producer() {
    let f = || {
        // increase n would significantly increase the number of finished executions
        // for exmplae, there's 5040 finished executions when n is 5
        // the program even hangs when n is 10
        let n = 5;

        let (tx, rx) = channel(100);
        for i in 0..n {
            let tx = tx.clone();
            thread::spawn(move || {
                traceforge::future::block_on(async {
                    let _ = tx.send(i).await;
                })
            });
        }

        for _ in 0..n {
            let j = rx.try_recv().unwrap();
            assert(0 <= j && j < 10);
        }
    };

    let stats = verify(
        Config::builder().with_keep_going_after_error(false).build(),
        f,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}
