use std::sync::Arc;

use traceforge::{sync::notify::Notify, *};
// this file shows some example usage of the Must's `sync::notify` library

#[test]
fn notify_test() {
    let f = || {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notify2 = notify.clone();

            let handle = future::spawn(async move {
                notify2.notified().await;
                println!("received notification");
            });

            println!("sending notification");
            notify.notify_one();

            // Wait for task to receive notification.
            handle.await.unwrap();
        });
    };

    let stats = verify(
        Config::builder()
            .with_verbose(10)
            .with_keep_going_after_error(false)
            .build(),
        f,
    );
    println!("Stats = {}, {}", stats.execs, stats.block);
}

#[test]
fn multiple_waiters_test() {
    let f = || {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notify1 = notify.clone();
            let notify2 = notify.clone();
            let notify3 = notify.clone();

            let handle1 = future::spawn(async move {
                notify1.notified().await;
                println!("waiter 1 received notification");
            });

            let handle2 = future::spawn(async move {
                notify2.notified().await;
                println!("waiter 2 received notification");
            });

            println!("sending first notification");
            notify.notify_one();

            println!("sending second notification");
            notify3.notify_one();

            // Wait for both tasks to receive notifications
            handle1.await.unwrap();
            handle2.await.unwrap();
        });
    };

    let stats = verify(
        Config::builder()
            .with_verbose(5)
            .with_keep_going_after_error(false)
            .build(),
        f,
    );
    println!("Multiple waiters stats = {}, {}", stats.execs, stats.block);
}

#[test]
fn notify_before_wait_test() {
    let f = || {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notify_clone = notify.clone();

            // Send notification before anyone is waiting
            println!("sending notification before waiting");
            notify.notify_one();

            let handle = future::spawn(async move {
                notify_clone.notified().await;
                println!("received pre-sent notification");
            });

            // The waiter should receive the notification immediately
            handle.await.unwrap();
        });
    };

    let stats = verify(
        Config::builder()
            .with_verbose(5)
            .with_keep_going_after_error(false)
            .build(),
        f,
    );
    println!(
        "Notify before wait stats = {}, {}",
        stats.execs, stats.block
    );
}

#[test]
fn two_notifies_one_received() {
    let f = || {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notify_clone = notify.clone();

            // Send first notification - this sets state to 1
            println!("sending first notification");
            notify.notify_one();

            // Send second notification - this should overwrite the first (state stays 1)
            println!("sending second notification (overwrites first)");
            notify.notify_one();

            let handle = future::spawn(async move {
                notify_clone.notified().await;
                println!("received notification (only one despite two sends)");
            });

            // The waiter should only receive one notification, not two
            handle.await.unwrap();

            // Now if we try to wait again, it should block since no new notification was sent
            let notify_clone2 = notify.clone();
            let handle2 = future::spawn(async move {
                println!("waiting for another notification...");
                notify_clone2.notified().await;
                println!("received second notification");
            });

            // Send one more notification to unblock the second waiter
            println!("sending third notification");
            notify.notify_one();

            handle2.await.unwrap();
        });
    };

    let stats = verify(
        Config::builder()
            .with_verbose(5)
            .with_keep_going_after_error(false)
            .build(),
        f,
    );
    println!(
        "Two notifies one received stats = {}, {}",
        stats.execs, stats.block
    );
}
