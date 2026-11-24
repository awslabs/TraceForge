//! From the visible events to the text.

use std::collections::{BTreeMap, HashMap};

use crate::event::Event;
use crate::monitor_types::EndCondition;
use crate::thread::ThreadId;

use super::analysis::{Analysis, Line, ObjectKind, Part, Peer};
use super::{Failure, FailureReport};

const RULE: usize = 110;

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{} {}", n, word)
    } else {
        format!("{} {}s", n, word)
    }
}

/// How an event is named: `(thread, index)` in the thread blocks, `#n` in the interleaving.
struct Naming<'a> {
    a: &'a Analysis,
    steps: Option<&'a HashMap<Event, usize>>,
}

impl Naming<'_> {
    fn event(&self, e: Event) -> String {
        if let Some(n) = self.steps.and_then(|s| s.get(&e)) {
            return format!("#{}", n);
        }
        format!("({}, {})", self.a.name(e.thread), e.index)
    }

    fn peer(&self, p: &Peer) -> String {
        match p {
            Peer::None => String::new(),
            Peer::To(e) => format!("-> {}", self.event(*e)),
            Peer::From(e) => format!("<- {}", self.event(*e)),
            Peer::Text(s) => s.clone(),
        }
    }

    fn text(&self, parts: &[Part]) -> String {
        parts
            .iter()
            .map(|p| match p {
                Part::Str(s) => s.clone(),
                Part::Ref(e) => self.event(*e),
            })
            .collect()
    }
}

struct Columns {
    op: usize,
    peer: usize,
    object: usize,
}

fn width(max: usize) -> usize {
    if max == 0 {
        0
    } else {
        max + 2
    }
}

fn row(out: &mut Vec<String>, left: String, l: &Line, naming: &Naming, w: &Columns) {
    let s = format!(
        "{}{:<op$}{:<peer$}{:<object$}{}",
        left,
        l.op,
        naming.peer(&l.peer),
        l.object,
        naming.text(&l.text),
        op = w.op,
        peer = w.peer,
        object = w.object,
    );
    out.push(s.trim_end().to_string());
}

pub(super) fn render(
    a: &Analysis,
    failure: Option<&FailureReport>,
    order: Option<&[usize]>,
) -> String {
    let mut out: Vec<String> = Vec::new();
    if let Some(f) = failure {
        violation(&mut out, a, f);
        out.push(String::new());
    }
    legend(&mut out, a);
    out.push(String::new());
    hidden(&mut out, a);
    out.push(String::new());
    how_to_read(&mut out, a);
    out.push(String::new());
    out.push("=".repeat(RULE));
    out.push(String::new());

    let by_thread = Naming { a, steps: None };
    let op = width(a.lines.iter().map(|l| l.op.len()).max().unwrap_or(0));
    let object = width(a.lines.iter().map(|l| l.object.len()).max().unwrap_or(0));
    let w = Columns {
        op,
        peer: width(
            a.lines
                .iter()
                .map(|l| by_thread.peer(&l.peer).len())
                .max()
                .unwrap_or(0),
        ),
        object,
    };
    let index_width = a
        .lines
        .iter()
        .map(|l| l.pos.index.to_string().len())
        .max()
        .unwrap_or(0)
        .max(5);
    let mut per_thread: BTreeMap<ThreadId, Vec<&Line>> = BTreeMap::new();
    for l in &a.lines {
        per_thread.entry(l.pos.thread).or_default().push(l);
    }
    let visible: Vec<_> = a.visible_threads().collect();
    for (i, t) in visible.iter().enumerate() {
        let daemon = if t.daemon { ", daemon" } else { "" };
        out.push(format!(
            "thread \"{}\"  (t{}{})",
            t.name,
            t.tid.to_number(),
            daemon
        ));
        for l in per_thread.get(&t.tid).map(|v| v.as_slice()).unwrap_or(&[]) {
            let left = format!("{:>iw$}  ", l.pos.index, iw = index_width);
            row(&mut out, left, l, &by_thread, &w);
        }
        if i + 1 < visible.len() || order.is_some() {
            out.push(String::new());
        }
    }

    if let Some(order) = order {
        let steps: HashMap<Event, usize> = order
            .iter()
            .enumerate()
            .map(|(n, &li)| (a.lines[li].pos, n + 1))
            .collect();
        let by_step = Naming {
            a,
            steps: Some(&steps),
        };
        let w = Columns {
            op,
            peer: width(
                a.lines
                    .iter()
                    .map(|l| by_step.peer(&l.peer).len())
                    .max()
                    .unwrap_or(0),
            ),
            object,
        };
        let step_width = order.len().to_string().len().max(4);
        let name_width = a
            .visible_threads()
            .map(|t| t.name.len())
            .max()
            .unwrap_or(0)
            .max("thread".len())
            + 2;
        out.push("=".repeat(RULE));
        out.push(String::new());
        out.push(
            "the same execution as one interleaving  \
             (one of the orders that this behavior allows; #n is a step of this list)"
                .to_string(),
        );
        out.push(String::new());
        out.push(format!(
            "{:>sw$}  {:<nw$}event",
            "#",
            "thread",
            sw = step_width,
            nw = name_width
        ));
        let mut prev: Option<ThreadId> = None;
        for (n, &li) in order.iter().enumerate() {
            let l = &a.lines[li];
            let who = if prev == Some(l.pos.thread) {
                String::new()
            } else {
                a.name(l.pos.thread)
            };
            prev = Some(l.pos.thread);
            let left = format!(
                "{:>sw$}  {:<nw$}",
                n + 1,
                who,
                sw = step_width,
                nw = name_width
            );
            row(&mut out, left, l, &by_step, &w);
        }
    }
    out.join("\n")
}

fn violation(out: &mut Vec<String>, a: &Analysis, f: &FailureReport) {
    let at = |e: Event, word: &str| {
        let name = a.name(e.thread);
        format!("in {}, {} ({}, {})", name, word, name, e.index)
    };
    let (what, mut details): (&str, Vec<String>) = match &f.failure {
        Failure::Monitor { message, condition } => (
            if *condition == EndCondition::Deadlock {
                "reported by a monitor, at the end of a blocked execution"
            } else {
                "reported by a monitor, at the end of the execution"
            },
            message.lines().map(str::to_string).collect(),
        ),
        Failure::Assertion { pos } => ("an assertion failed", vec![at(*pos, "at")]),
        Failure::Panic { pos, message } => {
            let mut d: Vec<String> = pos.iter().map(|p| at(*p, "after")).collect();
            d.extend(message.lines().map(str::to_string));
            ("a thread panicked", d)
        }
    };
    details.push(format!("seed {}", f.seed));
    out.push(format!("VIOLATION  {}", what));
    for d in details {
        out.push(format!("  {}", d));
    }
}

/// The list of user threads, each with the tasks and objects it created.
fn legend(out: &mut Vec<String>, a: &Analysis) {
    let owner = |mut t: ThreadId| {
        while let Some(v) = a.thread(t) {
            match (v.task, v.parent) {
                (true, Some(p)) => t = p,
                _ => break,
            }
        }
        t
    };
    let owners: Vec<_> = a.visible_threads().filter(|t| !t.task).collect();
    let rows: Vec<(String, String, Vec<Option<String>>)> = owners
        .iter()
        .map(|t| {
            let tasks: Vec<String> = a
                .visible_threads()
                .filter(|c| c.task && c.parent.map(owner) == Some(t.tid))
                .map(|c| format!("{} (t{})", c.name, c.tid.to_number()))
                .collect();
            let objects = |kind: ObjectKind| -> Vec<String> {
                a.objects
                    .iter()
                    .filter(|o| o.kind == kind && owner(o.creator) == t.tid)
                    .map(|o| o.name.clone())
                    .collect()
            };
            let cell = |label: &str, items: Vec<String>| {
                (!items.is_empty()).then(|| format!("{}: {}", label, items.join(", ")))
            };
            let cells = vec![
                t.daemon.then(|| "daemon".to_string()),
                cell("tasks", tasks),
                cell("atomics", objects(ObjectKind::Atomic)),
                cell("mutexes", objects(ObjectKind::Mutex)),
                cell("rwlocks", objects(ObjectKind::RwLock)),
            ];
            (t.name.clone(), format!("t{}", t.tid.to_number()), cells)
        })
        .collect();
    let name_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0) + 2;
    let tid_width = rows.iter().map(|r| r.1.len()).max().unwrap_or(0) + 3;
    let cell_width: Vec<usize> = (0..5)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.2[c].as_ref().map(|s| s.len()))
                .max()
                .unwrap_or(0)
                + 3
        })
        .collect();
    out.push("threads".to_string());
    for (name, tid, cells) in rows {
        let mut s = format!(
            "  {:<nw$}{:<tw$}",
            name,
            tid,
            nw = name_width,
            tw = tid_width
        );
        for (c, cell) in cells.iter().enumerate() {
            if let Some(cell) = cell {
                s.push_str(&format!("{:<w$}", cell, w = cell_width[c]));
            }
        }
        out.push(s.trim_end().to_string());
    }
}

fn hidden(out: &mut Vec<String>, a: &Analysis) {
    let hidden: Vec<_> = a.threads.iter().filter(|t| t.hidden).collect();
    let hidden_events: usize = hidden.iter().map(|t| t.len).sum();
    let visible_events: usize = a.visible_threads().map(|t| t.len).sum();
    out.push(format!(
        "hidden  {} ({}), and {} in the threads above",
        plural(hidden.len(), "runtime thread"),
        plural(hidden_events, "event"),
        plural(
            visible_events.saturating_sub(a.lines.len()),
            "runtime event"
        ),
    ));
}

fn how_to_read(out: &mut Vec<String>, a: &Analysis) {
    let has = |ops: &[&str]| a.lines.iter().any(|l| ops.contains(&l.op));
    out.push("how to read a line".to_string());
    if has(&["SEND"]) {
        out.push("  index  SEND         -> (thread, index)  c3.8   value     the event that receives it; the channel, if the message goes through one".into());
    }
    if has(&["RECV"]) {
        out.push(
            "  index  RECV         <- (thread, index)  c3.8   value     the event that sent it"
                .into(),
        );
    }
    if has(&["NEW CHANNEL"]) {
        out.push("  index  NEW CHANNEL                      c3.8   of u64    c3.8 is the channel created by t3 at its index 8".into());
    }
    if has(&["NEW ATOMIC", "STORE", "LOAD", "CAS"]) {
        let cas = has(&["CAS"]);
        out.push("  index  NEW ATOMIC                       a5     of u64, initially 0".into());
        out.push(format!(
            "  index  STORE                            a5.1   value 10  the 1st access to a5, counting the {} of all threads",
            if cas { "loads, stores and compare-exchanges" } else { "loads and stores" }
        ));
        out.push("  index  LOAD                             a5.3   value 20, stored at a5.2 (thread, index)".into());
        if cas {
            out.push("  index  CAS                              a5.4   expected 20, new 30: succeeded, found 20, stored at a5.2 (thread, index)".into());
        }
    }
    if has(&["NEW MUTEX", "LOCK", "UNLOCK", "TRY_LOCK"]) {
        out.push("  index  NEW MUTEX                        m9".into());
        out.push("  index  LOCK                             m9.1             the 1st request served by m9, counting the locks, unlocks and try_locks of all threads".into());
        if has(&["TRY_LOCK"]) {
            out.push("  index  TRY_LOCK                         m9.4   failed, held since m9.3 (thread, index)".into());
        }
    }
    if has(&[
        "NEW RWLOCK",
        "READ_LOCK",
        "WRITE_LOCK",
        "READ_UNLOCK",
        "WRITE_UNLOCK",
    ]) {
        out.push("  index  NEW RWLOCK                       r4".into());
        out.push("  index  READ_LOCK                        r4.1             the 1st request served by r4, counting the locks and unlocks of all threads".into());
    }
    out.push("  indexes are those of the full trace: a gap means hidden runtime events".into());
}
