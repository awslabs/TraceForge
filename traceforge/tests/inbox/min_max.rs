use traceforge::{self, thread, Config};

fn combinations(n: u32, k: u32) -> usize {
    if k > n {
        return 0;
    }
    if k == 0 || k == n {
        return 1;
    }
    let k = k.min(n - k);
    let mut num: usize = 1;
    let mut den: usize = 1;
    for i in 0..k {
        num *= (n - i) as usize;
        den *= (i + 1) as usize;
    }
    num / den
}

fn blocking_min_counts(senders: u32, min: u32) -> (usize, usize) {
    if senders < min {
        (0, 1)
    } else {
        let mut execs = 0usize;
        for k in min..=senders {
            execs += combinations(senders, k);
        }
        (execs, 0)
    }
}

fn max_one_counts(senders: u32) -> (usize, usize) {
    (1 + combinations(senders, 1), 0)
}

fn two_blocking_one_each_counts(senders: u32) -> (usize, usize) {
    if senders < 2 {
        (0, 1)
    } else {
        (senders as usize * (senders.saturating_sub(1)) as usize, 0)
    }
}

fn two_nonblocking_max_one_counts(senders: u32) -> (usize, usize) {
    // First inbox: 1 + n subsets of size <=1. If it takes 0, second sees n; else n-1.
    let n = senders as usize;
    let execs = (1 + n) + n * (1 + n.saturating_sub(1));
    (execs, 0)
}

fn run_blocking_min(senders: u32, min: u32) -> (usize, usize) {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let _ = traceforge::inbox_with_bounds(min as usize, None);
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for _ in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, 1u32);
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });
    (stats.execs, stats.block)
}

fn run_max_one(senders: u32) -> (usize, usize) {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let _ = traceforge::inbox_with_bounds(0, Some(1));
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for _ in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, 1u32);
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });
    (stats.execs, stats.block)
}

fn run_two_blocking_one_each(senders: u32) -> (usize, usize) {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let _ = traceforge::inbox_with_bounds(1, Some(1));
            let _ = traceforge::inbox_with_bounds(1, Some(1));
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for _ in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, 1u32);
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });
    (stats.execs, stats.block)
}

fn run_two_nonblocking_max_one(senders: u32) -> (usize, usize) {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let inbox_thread = thread::spawn(move || {
            let _ = traceforge::inbox_with_bounds(0, Some(1));
            let _ = traceforge::inbox_with_bounds(0, Some(1));
        });

        let inbox_tid = inbox_thread.thread().id();
        let mut sender_threads = Vec::new();

        for _ in 0..senders {
            let tid = inbox_tid.clone();
            sender_threads.push(thread::spawn(move || {
                traceforge::send_msg(tid, 1u32);
            }));
        }

        for sender_thread in sender_threads {
            let _ = sender_thread.join();
        }

        let _ = inbox_thread.join();
    });
    (stats.execs, stats.block)
}

macro_rules! blocking_min_test {
    ($name:ident, $senders:expr, $min:expr) => {
        #[test]
        fn $name() {
            let (execs, blocked) = run_blocking_min($senders, $min);
            let (expected_execs, expected_blocked) = blocking_min_counts($senders, $min);
            assert_eq!(execs, expected_execs);
            assert_eq!(blocked, expected_blocked);
        }
    };
}

macro_rules! max_one_test {
    ($name:ident, $senders:expr) => {
        #[test]
        fn $name() {
            let (execs, blocked) = run_max_one($senders);
            let (expected_execs, expected_blocked) = max_one_counts($senders);
            assert_eq!(execs, expected_execs);
            assert_eq!(blocked, expected_blocked);
        }
    };
}

macro_rules! two_blocking_one_each_test {
    ($name:ident, $senders:expr) => {
        #[test]
        fn $name() {
            let (execs, blocked) = run_two_blocking_one_each($senders);
            let (expected_execs, expected_blocked) = two_blocking_one_each_counts($senders);
            assert_eq!(execs, expected_execs);
            assert_eq!(blocked, expected_blocked);
        }
    };
}

macro_rules! two_nonblocking_max_one_test {
    ($name:ident, $senders:expr) => {
        #[test]
        fn $name() {
            let (execs, blocked) = run_two_nonblocking_max_one($senders);
            let (expected_execs, expected_blocked) = two_nonblocking_max_one_counts($senders);
            assert_eq!(execs, expected_execs);
            assert_eq!(blocked, expected_blocked);
        }
    };
}

// Blocking inbox (min > 0): expect block when not enough messages, otherwise combinations of size >= min.
blocking_min_test!(blocking_min_0_senders, 0, 2);
blocking_min_test!(blocking_min_1_sender, 1, 2);
blocking_min_test!(blocking_min_2_senders, 2, 2);
blocking_min_test!(blocking_min_3_senders, 3, 2);

// Non-blocking min=0, max=1: 1 (empty) + choose-one per sender.
max_one_test!(max_one_0_senders, 0);
max_one_test!(max_one_1_sender, 1);
max_one_test!(max_one_2_senders, 2);
max_one_test!(max_one_3_senders, 3);

// Two inboxes, each needs exactly one.
two_blocking_one_each_test!(two_blocking_one_each_0_senders, 0);
two_blocking_one_each_test!(two_blocking_one_each_1_sender, 1);
two_blocking_one_each_test!(two_blocking_one_each_2_senders, 2);
two_blocking_one_each_test!(two_blocking_one_each_3_senders, 3);

// Two inboxes, each max=1 non-blocking.
two_nonblocking_max_one_test!(two_nonblocking_max_one_0_senders, 0);
two_nonblocking_max_one_test!(two_nonblocking_max_one_1_sender, 1);
two_nonblocking_max_one_test!(two_nonblocking_max_one_2_senders, 2);