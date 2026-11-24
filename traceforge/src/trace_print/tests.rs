//! Tests of the pretty printer.
//!
//! Each golden test runs a small program, takes the text that the pretty printer printed, and
//! compares it with `golden/<name>.txt`. Run with `TRACEFORGE_BLESS=1` to write the files.
//!
//! Every print made during the tests also runs the structural checks below; the tests assert
//! that they find nothing.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex as StdMutex};

use crate::event::Event;
use crate::monitor_types::{ExecutionEnd, Monitor, MonitorResult};
use crate::sync::atomic::AtomicU64;
use crate::sync::{mpsc, Mutex, RwLock};
use crate::thread::{self, ThreadId};
use crate::{future, recv_msg, recv_msg_block, send_msg, Config};

use super::analysis::{Analysis, Peer};

// ------------------------------------------------------------------------------------------
// Capture

#[derive(Clone)]
pub(crate) struct Captured {
    pub(crate) text: String,
    pub(crate) problems: Vec<String>,
}

thread_local! {
    static PRINTS: RefCell<Vec<Captured>> = const { RefCell::new(Vec::new()) };
}

/// Every print of every thread. In partitioned parallel mode, the exploration runs on rayon
/// threads, so the prints do not reach the test thread's `PRINTS`.
static ALL_PRINTS: StdMutex<Vec<Captured>> = StdMutex::new(Vec::new());

pub(super) fn record(text: &str, problems: Vec<String>) {
    let c = Captured {
        text: text.to_string(),
        problems,
    };
    ALL_PRINTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(c.clone());
    PRINTS.with(|p| p.borrow_mut().push(c));
}

fn take() -> Vec<Captured> {
    PRINTS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Takes the prints of all threads whose text contains `marker`.
fn take_all_with(marker: &str) -> Vec<Captured> {
    let mut all = ALL_PRINTS.lock().unwrap_or_else(|e| e.into_inner());
    let (mine, others): (Vec<Captured>, Vec<Captured>) =
        all.drain(..).partition(|c| c.text.contains(marker));
    *all = others;
    mine
}

/// What must never appear in a pretty print: the runtime's threads and messages.
const RUNTIME_WORDS: &[&str] = &[
    "traceforge_runtime",
    "async_recv",
    "<future-",
    "PollerMsg",
    "Waker",
    "WakeMsg",
    "MsgRequest",
    "MsgResponse",
    "LockRequest",
    "opaque_id",
    "Loc(",
    "type_name",
];

// ------------------------------------------------------------------------------------------
// Structural checks

pub(super) fn structural_problems(a: &Analysis, order: Option<&[usize]>) -> Vec<String> {
    let mut problems = Vec::new();
    let at: HashMap<Event, usize> = a
        .lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.pos, i))
        .collect();

    // Every RECV has a matching SEND with the same message, and the reverse.
    for l in &a.lines {
        match (&l.peer, l.op) {
            (Peer::From(s), "RECV") => match at.get(s).map(|&i| &a.lines[i]) {
                Some(sl) if sl.op == "SEND" => {
                    if !matches!(sl.peer, Peer::To(r) if r == l.pos) {
                        problems.push(format!(
                            "SEND {:?} does not point back to RECV {:?}",
                            s, l.pos
                        ));
                    }
                    if sl.message != l.message {
                        problems.push(format!("RECV {:?} and SEND {:?} differ in value", l.pos, s));
                    }
                }
                _ => problems.push(format!("RECV {:?} has no SEND line at {:?}", l.pos, s)),
            },
            (Peer::To(r), "SEND") => match at.get(r).map(|&i| &a.lines[i]) {
                Some(rl) if rl.op == "RECV" => {
                    if !matches!(rl.peer, Peer::From(s) if s == l.pos) {
                        problems.push(format!(
                            "RECV {:?} does not point back to SEND {:?}",
                            r, l.pos
                        ));
                    }
                }
                _ => problems.push(format!("SEND {:?} has no RECV line at {:?}", l.pos, r)),
            },
            (Peer::To(r), "ABORT") => {
                if !at.get(r).is_some_and(|&i| a.lines[i].op == "ABORTED") {
                    problems.push(format!("ABORT {:?} has no ABORTED line at {:?}", l.pos, r));
                }
            }
            _ => {}
        }
    }

    // Indexes increase inside a thread block.
    let mut last: BTreeMap<ThreadId, u32> = BTreeMap::new();
    for l in &a.lines {
        if let Some(&prev) = last.get(&l.pos.thread) {
            if prev >= l.pos.index {
                problems.push(format!("indexes do not increase at {:?}", l.pos));
            }
        }
        last.insert(l.pos.thread, l.pos.index);
    }

    let Some(order) = order else {
        return problems;
    };
    let mut sorted = order.to_vec();
    sorted.sort();
    if sorted != (0..a.lines.len()).collect::<Vec<_>>() {
        problems.push("the interleaving is not a permutation of the lines".to_string());
        return problems;
    }
    let step: HashMap<Event, usize> = order
        .iter()
        .enumerate()
        .map(|(n, &i)| (a.lines[i].pos, n))
        .collect();

    // In the interleaving: program order, and a receive after its send.
    let mut last: BTreeMap<ThreadId, u32> = BTreeMap::new();
    for &i in order {
        let l = &a.lines[i];
        if last.get(&l.pos.thread).is_some_and(|&p| p >= l.pos.index) {
            problems.push(format!("interleaving breaks program order at {:?}", l.pos));
        }
        last.insert(l.pos.thread, l.pos.index);
        if let Peer::From(s) = l.peer {
            if let Some(&n) = step.get(&s) {
                if n >= step[&l.pos] {
                    problems.push(format!("{:?} comes before its peer {:?}", l.pos, s));
                }
            }
        }
    }

    // Accesses to one object are numbered 1, 2, 3, ... in the interleaving; a load returns the
    // value of the last store before it.
    let initial: HashMap<ThreadId, String> = a
        .objects
        .iter()
        .filter_map(|o| o.initial.clone().map(|v| (o.tid, v)))
        .collect();
    let mut count: HashMap<ThreadId, usize> = HashMap::new();
    let mut value: HashMap<ThreadId, String> = initial;
    for &i in order {
        let l = &a.lines[i];
        let Some(acc) = &l.access else { continue };
        let Some(k) = acc.k else { continue };
        let n = count.entry(acc.object).or_default();
        *n += 1;
        if *n != k {
            problems.push(format!(
                "access {:?} is numbered {} but is the {}th",
                l.pos, k, n
            ));
        }
        if let Some(read) = &acc.read {
            if value.get(&acc.object) != Some(read) {
                problems.push(format!(
                    "{:?} reads {} but the last value stored is {:?}",
                    l.pos,
                    read,
                    value.get(&acc.object)
                ));
            }
        }
        if let Some(w) = &acc.written {
            value.insert(acc.object, w.clone());
        }
    }
    problems
}

// ------------------------------------------------------------------------------------------
// Helpers

fn config() -> Config {
    Config::builder()
        .with_pretty_graph_printing(true)
        .with_seed(1)
        .build()
}

/// Runs a program that fails, and returns the text printed for the failure.
fn failing<F: Fn() + Send + Sync + 'static>(config: Config, f: F) -> String {
    take();
    let r = catch_unwind(AssertUnwindSafe(|| crate::verify(config, f)));
    assert!(r.is_err(), "the program was expected to fail");
    last_print()
}

/// Runs a program that does not fail, and returns the last text printed.
fn passing<F: Fn() + Send + Sync + 'static>(config: Config, f: F) -> String {
    take();
    crate::verify(config, f);
    last_print()
}

fn last_print() -> String {
    let prints = take();
    for p in &prints {
        assert!(
            p.problems.is_empty(),
            "structural problems: {:#?}\n{}",
            p.problems,
            p.text
        );
    }
    prints.last().expect("nothing was printed").text.clone()
}

fn golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/trace_print/golden")
        .join(format!("{}.txt", name));
    let actual = format!("{}\n", actual);
    if std::env::var_os("TRACEFORGE_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {}; run with TRACEFORGE_BLESS=1 to create it",
            path.display()
        )
    });
    if expected != actual {
        panic!(
            "the pretty print differs from {} (run with TRACEFORGE_BLESS=1 to update it)\n\
             ---- expected\n{}---- actual\n{}",
            path.display(),
            expected,
            actual
        );
    }
}

/// A monitor that rejects every execution that ends.
struct RejectAll;

impl Monitor for RejectAll {
    fn on_stop(&mut self, end: &ExecutionEnd) -> MonitorResult {
        Err(format!("the execution ended with {:?}", end.condition))
    }
}

fn start_reject_all() {
    let m: Arc<StdMutex<dyn Monitor>> = Arc::new(StdMutex::new(RejectAll));
    crate::spawn_monitor(
        || loop {
            let _: () = recv_msg_block();
        },
        |_, _, _| None,
        |_, _, _| false,
        m,
    );
}

// ------------------------------------------------------------------------------------------
// Golden tests

/// Threads that exchange messages, with a spawn and a join; an assertion fails.
fn ping_pong() {
    let main = thread::current().id();
    let pong = thread::Builder::new()
        .name("pong".into())
        .spawn(move || {
            let (from, n): (ThreadId, u32) = recv_msg_block();
            send_msg(from, n + 1);
        })
        .unwrap();
    let pong_id = pong.thread().id();
    let ping = thread::Builder::new()
        .name("ping".into())
        .spawn(move || {
            send_msg(pong_id, (thread::current().id(), 1u32));
            let n: u32 = recv_msg_block();
            send_msg(main, n);
        })
        .unwrap();
    let n: u32 = recv_msg_block();
    ping.join().unwrap();
    pong.join().unwrap();
    crate::assert(n == 3);
}

#[test]
fn golden_messages() {
    golden("messages", &failing(config(), ping_pong));
}

#[test]
fn golden_messages_not_a_failure() {
    // Printed because of `with_verbose`: no VIOLATION block and no interleaving.
    let text = passing(config_verbose(), || {
        let t = thread::spawn(|| {
            let n: u32 = recv_msg_block();
            n
        });
        send_msg(t.thread().id(), 7u32);
        t.join().unwrap();
    });
    assert!(!text.contains("VIOLATION"));
    assert!(!text.contains("interleaving"));
    golden("messages_not_a_failure", &text);
}

fn config_verbose() -> Config {
    Config::builder()
        .with_pretty_graph_printing(true)
        .with_seed(1)
        .with_verbose(1)
        .build()
}

/// `sync::mpsc` between tasks, with awaited receives, a `select!` and an abort.
#[test]
fn golden_tasks() {
    let text = failing(config(), || {
        future::block_on(async {
            let (tx, rx) = mpsc::unbounded_channel::<u32>();
            let (tx2, rx2) = mpsc::unbounded_channel::<u32>();
            let consumer =
                future::spawn_with_attributes(false, Some("consumer".into()), async move {
                    let mut sum = 0;
                    for _ in 0..2 {
                        tokio::select! {
                            biased;
                            Some(v) = rx.recv() => sum += v,
                            Some(v) = rx2.recv() => sum += 10 * v,
                        }
                    }
                    sum
                });
            let idle = future::spawn(async move {
                let (_keep, never) = mpsc::unbounded_channel::<u32>();
                never.recv().await
            });
            tx.send(1).unwrap();
            tx2.send(2).unwrap();
            let sum = consumer.await.unwrap();
            idle.abort();
            crate::assert(sum == 0);
        });
    });
    golden("tasks", &text);
}

/// Atomics: a load of the initial value, stores from two threads, compare-exchanges.
#[test]
fn golden_atomics() {
    let text = failing(config(), || {
        let a = Arc::new(AtomicU64::new(5));
        let a1 = a.clone();
        let w = thread::Builder::new()
            .name("writer".into())
            .spawn(move || a1.store(1, SeqCst))
            .unwrap();
        let first = a.load(SeqCst);
        a.store(2, SeqCst);
        w.join().unwrap();
        let _ = a.compare_exchange(2, 3, SeqCst, SeqCst);
        let _ = a.compare_exchange(7, 8, SeqCst, SeqCst);
        crate::assert(first == 100);
    });
    golden("atomics", &text);
}

/// A mutex: lock, unlock, a try_lock that succeeds, one that fails, a lock that blocks.
/// The monitor reports the blocked execution.
#[test]
fn golden_mutex() {
    let text = failing(config(), || {
        start_reject_all();
        let m = Arc::new(Mutex::new(0));
        {
            let mut g = m.blocking_lock();
            *g += 1;
        }
        drop(m.try_lock().unwrap());
        let held = m.blocking_lock();
        let m2 = m.clone();
        let w = thread::Builder::new()
            .name("worker".into())
            .spawn(move || {
                assert!(m2.try_lock().is_err());
                let _g = m2.blocking_lock();
            })
            .unwrap();
        std::mem::forget(held);
        w.join().unwrap();
    });
    golden("mutex", &text);
}

/// A rwlock: two readers, then a writer that blocks. The monitor reports the blocked execution.
#[test]
fn golden_rwlock() {
    let text = failing(config(), || {
        start_reject_all();
        let l = Arc::new(RwLock::new(0u32));
        let first = future::block_on(l.read());
        let l2 = l.clone();
        let r = thread::Builder::new()
            .name("reader".into())
            .spawn(move || {
                let g = future::block_on(l2.read());
                drop(g);
            })
            .unwrap();
        r.join().unwrap();
        let l3 = l.clone();
        let w = thread::Builder::new()
            .name("writer".into())
            .spawn(move || {
                let _g = future::block_on(l3.write());
            })
            .unwrap();
        std::mem::forget(first);
        w.join().unwrap();
    });
    golden("rwlock", &text);
}

/// A send that is never received, a receive that times out, a nondet choice.
#[test]
fn golden_misc() {
    let text = failing(config(), || {
        let sink = thread::Builder::new()
            .name("sink".into())
            .spawn(|| ())
            .unwrap();
        send_msg(sink.thread().id(), 5u32);
        let nothing: Option<u32> = recv_msg();
        let coin = crate::nondet();
        sink.join().unwrap();
        crate::assert(nothing.is_some() && coin);
    });
    golden("misc", &text);
}

/// A thread panics.
#[test]
fn golden_panic() {
    let text = failing(config(), || {
        let w = thread::Builder::new()
            .name("worker".into())
            .spawn(|| {
                let n: u32 = recv_msg_block();
                if n == 1 {
                    panic!("boom: n is {}", n);
                }
            })
            .unwrap();
        send_msg(w.thread().id(), 1u32);
        let _ = w.join();
    });
    // Hide the panic's location: it moves with this file's lines, and is missing when the panic
    // hook was bypassed.
    let text = text
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("panicked") {
                "  panicked at <location>:".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    golden("panic", &text);
}

// ------------------------------------------------------------------------------------------
// Partitioned parallel exploration
//
// The exploration runs on rayon threads, and graphs move between rayon tasks. Which worker finds
// which execution varies between runs, so instead of golden files the prints are checked for
// their structure and content.

fn partitioned_config() -> crate::ConfigBuilder {
    Config::builder()
        .with_partitioned_parallelization(true)
        .with_partitioned_num_threads(3)
        .with_warmup(2)
        .with_iterations_until_split(1)
        .with_state_batch_size(1)
        .with_pretty_graph_printing(true)
        .with_seed(1)
}

/// Three senders store to an atomic and send on an `mpsc` channel; `main` awaits the three
/// messages. If `fail`, an assertion fails when they arrive in the order 2, 1, 0.
fn senders(marker: &'static str, fail: bool) -> impl Fn() + Send + Sync + 'static {
    move || {
        let last = Arc::new(AtomicU64::new(9));
        let (tx, rx) = mpsc::unbounded_channel::<u64>();
        for i in 0..3u64 {
            let tx = tx.clone();
            let last = last.clone();
            thread::Builder::new()
                .name(format!("{}{}", marker, i))
                .spawn(move || {
                    last.store(i, SeqCst);
                    tx.send(i).unwrap();
                })
                .unwrap();
        }
        let got: Vec<u64> = future::block_on(async move {
            let mut got = Vec::new();
            for _ in 0..3 {
                got.push(rx.recv().await.unwrap());
            }
            got
        });
        if fail {
            crate::assert(got != vec![2, 1, 0]);
        }
    }
}

/// The number of event lines (they start with an index or a step number) with operation `op`.
fn event_lines(text: &str, op: &str) -> usize {
    text.lines()
        .filter(|l| {
            let mut words = l.split_whitespace();
            words.next().is_some_and(|w| w.parse::<u32>().is_ok()) && words.any(|w| w == op)
        })
        .count()
}

/// The checks common to every print of `senders`.
fn check_senders_print(c: &Captured) -> Vec<String> {
    assert!(
        c.problems.is_empty(),
        "structural problems: {:#?}\n{}",
        c.problems,
        c.text
    );
    for w in RUNTIME_WORDS {
        assert!(!c.text.contains(w), "`{}` appears in\n{}", w, c.text);
    }
    let text = &c.text;
    // The marks travelled with the graphs: the channel and the atomic are typed, and the
    // helpers of the awaited receives are folded into `main`.
    assert!(
        text.contains("NEW CHANNEL") && text.contains("of u64"),
        "{}",
        text
    );
    assert!(text.contains("of u64, initially 9"), "{}", text);
    let main_block = text
        .split("thread \"main\"")
        .nth(1)
        .and_then(|s| s.split("\n\n").next())
        .unwrap_or_default()
        .to_string();
    let received: Vec<String> = main_block
        .lines()
        .filter(|l| l.contains(" RECV "))
        .map(|l| l.trim_end().rsplit(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(received.len(), 3, "main receives the 3 messages:\n{}", text);
    let mut sorted = received.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["0", "1", "2"], "{}", text);
    received
}

#[test]
fn partitioned_parallelization_prints_every_execution() {
    const MARKER: &str = "pp-every-";
    // `with_verbose(1)` prints every complete execution, on the worker that explored it.
    let stats = crate::verify(
        partitioned_config().with_verbose(1).build(),
        senders(MARKER, false),
    );
    let prints = take_all_with(MARKER);
    assert_eq!(
        prints.len(),
        stats.execs,
        "one print per complete execution"
    );
    let mut orders = std::collections::BTreeSet::new();
    for c in &prints {
        assert!(!c.text.contains("VIOLATION"), "{}", c.text);
        assert_eq!(event_lines(&c.text, "STORE"), 3, "{}", c.text);
        orders.insert(check_senders_print(c));
    }
    // The 6 orders of arrival were all explored, and printed.
    assert_eq!(orders.len(), 6, "{:?}", orders);
}

#[test]
fn partitioned_parallelization_failure() {
    const MARKER: &str = "pp-fail-";
    // The failure makes `verify` panic, whether the root worker or a rayon task finds it.
    let result = catch_unwind(AssertUnwindSafe(|| {
        crate::verify(partitioned_config().build(), senders(MARKER, true))
    }));
    assert!(result.is_err(), "the program was expected to fail");
    let prints = take_all_with(MARKER);
    let failures: Vec<&Captured> = prints
        .iter()
        .filter(|c| c.text.starts_with("VIOLATION"))
        .collect();
    assert!(!failures.is_empty(), "no failure was printed");
    for c in failures {
        assert!(
            c.text.contains("VIOLATION  an assertion failed"),
            "{}",
            c.text
        );
        assert!(
            c.text.contains("the same execution as one interleaving"),
            "{}",
            c.text
        );
        // 3 stores, in the thread blocks and in the interleaving.
        assert_eq!(event_lines(&c.text, "STORE"), 6, "{}", c.text);
        assert_eq!(check_senders_print(c), vec!["2", "1", "0"], "{}", c.text);
    }
}

// ------------------------------------------------------------------------------------------
// Outside the printer, `Debug` is unchanged.

#[test]
fn debug_is_unchanged_outside_the_printer() {
    let tid = crate::thread::construct_thread_id(3);
    assert_eq!(format!("{:?}", tid), "ThreadId { opaque_id: 3 }");
    assert_eq!(format!("{:#?}", tid), "ThreadId {\n    opaque_id: 3,\n}");
    crate::verify(Config::builder().build(), || {
        let (tx, rx) = mpsc::unbounded_channel::<u32>();
        let s = format!("{:?}", rx);
        assert!(
            s.starts_with("UnboundedReceiver { receiver: Receiver { inner: Loc(Event { thread: ThreadId { opaque_id: 0 }, index: "),
            "{}",
            s
        );
        assert!(
            s.ends_with(" }), comm: LocalOrder, _t: PhantomData<u32> } }"),
            "{}",
            s
        );
        let s = format!("{:?}", tx);
        assert!(
            s.starts_with("UnboundedSender { sender: Sender { inner: Loc(Event {"),
            "{}",
            s
        );
    });
}
