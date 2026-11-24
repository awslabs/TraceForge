//! From the execution graph to the list of visible events.
//!
//! The analysis reads the graph and the marks left by the libraries. It decides, for every event
//! of a user thread, whether it is printed and how. Protocols of the runtime (awaited receives,
//! polling of tasks, atomics, mutexes, rwlocks) are recognised through the marks and by
//! downcasting messages to the runtime's own types, never by matching strings.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::channel::Thread as Mailbox;
use crate::event::Event;
use crate::event_label::{AsEventLabel, BlockType, LabelEnum, SendMsg};
use crate::exec_graph::ExecutionGraph;
use crate::future::PollerMsg;
use crate::loc::Loc;
use crate::msg::Val;
use crate::thread::{main_thread_id, ThreadId};

use super::marks::*;
use super::names::NameCtx;

/// Prefix of the names of the threads that belong to the runtime.
const RUNTIME_PREFIX: &str = "traceforge_runtime::";

pub(super) struct ThreadView {
    pub(super) tid: ThreadId,
    pub(super) name: String,
    pub(super) daemon: bool,
    pub(super) hidden: bool,
    pub(super) task: bool,
    pub(super) parent: Option<ThreadId>,
    pub(super) kind: Option<ThreadKind>,
    pub(super) len: usize,
}

/// A piece of a line's text. An event reference prints as `(thread, index)` in the thread
/// blocks and as `#n` in the interleaving.
#[derive(Clone, Debug)]
pub(super) enum Part {
    Str(String),
    Ref(Event),
}

#[derive(Clone, Debug)]
pub(super) enum Peer {
    None,
    To(Event),
    From(Event),
    Text(String),
}

/// What a line says about an access to an atomic or a lock, for the structural checks.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct AccessFacts {
    pub(super) object: ThreadId,
    pub(super) k: Option<usize>,
    /// The value read (LOAD, CAS).
    pub(super) read: Option<String>,
    /// The value written (STORE, successful CAS).
    pub(super) written: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Line {
    pub(super) pos: Event,
    pub(super) op: &'static str,
    pub(super) peer: Peer,
    pub(super) object: String,
    pub(super) text: Vec<Part>,
    /// The event of the full graph where this line is placed in the interleaving.
    pub(super) anchor: Event,
    /// The message, for SEND and RECV lines.
    pub(super) message: Option<String>,
    pub(super) access: Option<AccessFacts>,
}

/// An atomic, mutex or rwlock, for the legend.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct ObjectView {
    pub(super) tid: ThreadId,
    pub(super) name: String,
    pub(super) kind: ObjectKind,
    pub(super) creator: ThreadId,
    pub(super) initial: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectKind {
    Atomic,
    Mutex,
    RwLock,
}

pub(super) struct Analysis {
    pub(super) threads: Vec<ThreadView>,
    pub(super) index: BTreeMap<ThreadId, usize>,
    pub(super) lines: Vec<Line>,
    pub(super) objects: Vec<ObjectView>,
}

impl Analysis {
    pub(super) fn thread(&self, tid: ThreadId) -> Option<&ThreadView> {
        self.index.get(&tid).map(|&i| &self.threads[i])
    }

    pub(super) fn name(&self, tid: ThreadId) -> String {
        self.thread(tid)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| format!("t{}", tid.to_number()))
    }

    pub(super) fn visible_threads(&self) -> impl Iterator<Item = &ThreadView> {
        self.threads.iter().filter(|t| !t.hidden)
    }
}

/// Formats a message value, without the `Val` wrapper.
pub(super) fn fmt_val(v: &Val) -> String {
    if v.is_pending() {
        return "?".to_string();
    }
    #[cfg(feature = "print_vals_custom")]
    {
        format!("{}", v.val)
    }
    #[cfg(not(feature = "print_vals_custom"))]
    {
        format!("{:?}", v.val)
    }
}

/// `core::result::Result<a::b::Opened, u64>` becomes `Result<Opened, u64>`.
pub(super) fn short_type(name: &str) -> String {
    let mut out = String::new();
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        let short = token.rsplit("::").next().unwrap_or("");
        out.push_str(short);
        token.clear();
    };
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' || c == ':' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn channel_label(e: Event) -> String {
    format!("c{}.{}", e.thread.to_number(), e.index)
}

fn poller(v: &Val) -> Option<&PollerMsg> {
    v.as_any_ref().downcast_ref::<PollerMsg>()
}

/// The default printer's text of a label, without its `(t, i): ` prefix.
fn todays_text(lab: &LabelEnum) -> String {
    let full = format!("{}", lab);
    match full.find(": ") {
        Some(i) => full[i + 2..].to_string(),
        None => full,
    }
}

/// The threads, with the names to print.
pub(super) fn thread_views(g: &ExecutionGraph) -> Vec<ThreadView> {
    let mut views: Vec<ThreadView> = Vec::new();
    let mut names: BTreeMap<ThreadId, String> = BTreeMap::new();
    for t in g.threads.iter() {
        let tid = t.tid;
        let is_main = tid == main_thread_id();
        let kind = if is_main {
            None
        } else {
            match g.mark(t.tclab.pos()) {
                Some(Mark::Thread(k)) => Some(k.clone()),
                _ => None,
            }
        };
        let raw = t.tclab.name().clone();
        let hidden = kind.as_ref().is_some_and(|k| k.is_runtime())
            || raw.as_ref().is_some_and(|n| n.starts_with(RUNTIME_PREFIX));
        let parent = if is_main {
            None
        } else {
            Some(t.tclab.pos().thread)
        };
        let n = tid.to_number();
        let name = match (&kind, &raw) {
            (Some(ThreadKind::Atomic(_)), _) => format!("a{}", n),
            (Some(ThreadKind::Lock(l)), _) if l.kind == LockKind::Mutex => format!("m{}", n),
            (Some(ThreadKind::Lock(_)), _) => format!("r{}", n),
            (Some(ThreadKind::Task { named: false }), _) => {
                let p = parent
                    .and_then(|p| names.get(&p).cloned())
                    .unwrap_or_else(|| "?".to_string());
                format!("{}/future-{}", p, n)
            }
            (_, Some(name)) => name.clone(),
            (_, None) => format!("t{}", n),
        };
        names.insert(tid, name.clone());
        views.push(ThreadView {
            tid,
            name,
            daemon: t.tclab.daemon(),
            hidden,
            task: matches!(kind, Some(ThreadKind::Task { .. })),
            parent,
            kind,
            len: t.labels.len(),
        });
    }
    // Two visible threads with the same name: append their ids.
    let mut count: HashMap<String, usize> = HashMap::new();
    for v in views.iter().filter(|v| !v.hidden) {
        *count.entry(v.name.clone()).or_default() += 1;
    }
    for v in views.iter_mut().filter(|v| !v.hidden) {
        if count[&v.name] > 1 {
            v.name = format!("{} t{}", v.name, v.tid.to_number());
        }
    }
    views
}

/// The names used while formatting values.
pub(super) fn name_ctx(g: &ExecutionGraph, views: &[ThreadView]) -> NameCtx {
    let mut ctx = NameCtx::default();
    for v in views {
        ctx.threads.insert(v.tid, v.name.clone());
    }
    for t in g.threads.iter() {
        for lab in &t.labels {
            if let LabelEnum::Unique(u) = lab {
                ctx.channels.insert(u.get_loc(), channel_label(lab.pos()));
            }
        }
    }
    ctx
}

/// An access to an atomic or a lock, served by its thread.
struct Access {
    serve: Event,
    op: &'static str,
    object: String,
    text: Vec<Part>,
    facts: AccessFacts,
}

/// An awaited receive.
struct Helper {
    /// The send whose message the helper took.
    payload: Option<Event>,
    /// The owner's event that receives `Ready`.
    ready_recv: Option<Event>,
}

struct Builder<'g> {
    g: &'g ExecutionGraph,
    views: Vec<ThreadView>,
    index: BTreeMap<ThreadId, usize>,
    unique: HashMap<Loc, Event>,
    mailbox: HashMap<Loc, ThreadId>,
    readers: HashMap<Event, Vec<Event>>,
    accesses: HashMap<Event, Access>,
    /// For each lock, who holds it at the end of the execution.
    lock_state: HashMap<ThreadId, Vec<Part>>,
    helpers: HashMap<ThreadId, Helper>,
    /// Owner's event receiving `Ready` -> the send whose message the helper took.
    folded: HashMap<Event, Event>,
    joined: HashSet<(ThreadId, ThreadId)>,
    blocked_on_lock: HashSet<ThreadId>,
}

pub(super) fn analyse(g: &ExecutionGraph, views: Vec<ThreadView>) -> Analysis {
    let index = views.iter().enumerate().map(|(i, v)| (v.tid, i)).collect();
    let mut b = Builder {
        g,
        views,
        index,
        unique: HashMap::new(),
        mailbox: HashMap::new(),
        readers: HashMap::new(),
        accesses: HashMap::new(),
        lock_state: HashMap::new(),
        helpers: HashMap::new(),
        folded: HashMap::new(),
        joined: HashSet::new(),
        blocked_on_lock: HashSet::new(),
    };
    b.scan();
    b.scan_objects();
    b.scan_helpers();
    let lines = b.lines();
    let objects = b.objects();
    Analysis {
        threads: b.views,
        index: b.index,
        lines,
        objects,
    }
}

impl<'g> Builder<'g> {
    fn view(&self, tid: ThreadId) -> Option<&ThreadView> {
        self.index.get(&tid).map(|&i| &self.views[i])
    }

    fn hidden(&self, tid: ThreadId) -> bool {
        self.view(tid).is_some_and(|v| v.hidden)
    }

    fn kind(&self, tid: ThreadId) -> Option<&ThreadKind> {
        self.view(tid).and_then(|v| v.kind.as_ref())
    }

    fn name(&self, tid: ThreadId) -> String {
        self.view(tid)
            .map(|v| v.name.clone())
            .unwrap_or_else(|| format!("t{}", tid.to_number()))
    }

    fn labels(&self, tid: ThreadId) -> &'g [LabelEnum] {
        match self.g.get_thr_opt(&tid) {
            Some(t) => &t.labels,
            None => &[],
        }
    }

    fn send(&self, e: Event) -> Option<&'g SendMsg> {
        if self.g.contains(e) {
            self.g.send_label(e)
        } else {
            None
        }
    }

    fn scan(&mut self) {
        let g = self.g;
        for t in g.threads.iter() {
            self.mailbox.insert(Loc::new(Mailbox(t.tid)), t.tid);
            for lab in &t.labels {
                match lab {
                    LabelEnum::Unique(u) => {
                        self.unique.insert(u.get_loc(), lab.pos());
                    }
                    LabelEnum::RecvMsg(r) => {
                        if let Some(s) = r.rf() {
                            self.readers.entry(s).or_default().push(lab.pos());
                        }
                    }
                    LabelEnum::Inbox(i) => {
                        for s in i.rfs().unwrap_or_default() {
                            self.readers.entry(s).or_default().push(lab.pos());
                        }
                    }
                    LabelEnum::TJoin(j) => {
                        self.joined.insert((t.tid, j.cid()));
                    }
                    _ => {}
                }
            }
        }
    }

    /// The receive that takes the message of a send, ignoring the monitors that observe it.
    fn reader(&self, s: Event) -> Option<Event> {
        let slab = self.send(s)?;
        self.readers
            .get(&s)?
            .iter()
            .copied()
            .find(|&r| match self.g.label(r) {
                LabelEnum::RecvMsg(rlab) => !rlab.monitors(slab),
                _ => true,
            })
    }

    /// The channel a send goes through, if it was created by `UNIQUE`.
    fn channel_of(&self, s: &SendMsg) -> Option<Event> {
        s.send_loc()
            .loc_opt()
            .and_then(|l| self.unique.get(l).copied())
    }

    fn is_runtime_channel(&self, c: Event) -> bool {
        matches!(self.g.mark(c), Some(Mark::Channel(m)) if m.runtime)
    }

    fn is_runtime_loc(&self, l: &Loc) -> bool {
        self.unique
            .get(l)
            .is_some_and(|&c| self.is_runtime_channel(c))
    }

    /// The thread whose mailbox a send goes to, if any.
    fn mailbox_of(&self, s: &SendMsg) -> Option<ThreadId> {
        s.send_loc()
            .loc_opt()
            .and_then(|l| self.mailbox.get(l).copied())
    }

    /// The object column of a send: the channel, or nothing for a mailbox.
    fn object_of(&self, s: &SendMsg) -> String {
        if let Some(c) = self.channel_of(s) {
            return channel_label(c);
        }
        match s.send_loc().loc_opt() {
            Some(l) if !self.mailbox.contains_key(l) => format!("{}", l),
            _ => String::new(),
        }
    }

    /// The send goes through a channel of the runtime.
    fn runtime_send(&self, s: &SendMsg) -> bool {
        self.channel_of(s)
            .is_some_and(|c| self.is_runtime_channel(c))
    }

    fn sends_by_loc(&self, tid: ThreadId) -> HashMap<Loc, Event> {
        let mut m = HashMap::new();
        for lab in self.labels(tid) {
            if let LabelEnum::SendMsg(s) = lab {
                if let Some(l) = s.send_loc().loc_opt() {
                    m.entry(l.clone()).or_insert(lab.pos());
                }
            }
        }
        m
    }

    /// The requests served by an object's thread, in the order it served them.
    fn served(&self, h: ThreadId) -> Vec<(Event, Event)> {
        self.labels(h)
            .iter()
            .filter_map(|lab| match lab {
                LabelEnum::RecvMsg(r) => r.rf().map(|req| (lab.pos(), req)),
                _ => None,
            })
            .collect()
    }

    fn scan_objects(&mut self) {
        let objs: Vec<(ThreadId, ThreadKind)> = self
            .views
            .iter()
            .filter_map(|v| match &v.kind {
                Some(k @ (ThreadKind::Atomic(_) | ThreadKind::Lock(_))) => Some((v.tid, k.clone())),
                _ => None,
            })
            .collect();
        for (h, kind) in objs {
            match kind {
                ThreadKind::Atomic(info) => self.scan_atomic(h, &info),
                ThreadKind::Lock(info) => self.scan_lock(h, &info),
                _ => {}
            }
        }
    }

    /// Numbers the accesses to an atomic in the order its thread served them, and works out
    /// which store each read comes from.
    fn scan_atomic(&mut self, h: ThreadId, info: &AtomicInfo) {
        let name = self.name(h);
        let replies = self.sends_by_loc(h);
        let response = |reply: &Loc| -> Option<AtomicMsg> {
            let s = self.send(*replies.get(reply)?)?;
            (info.decode)(s.val())
        };
        let mut current = fmt_val(&info.initial);
        let mut stored_at: Option<(usize, Event)> = None;
        let mut k = 0;
        let mut found: Vec<(Event, Access)> = Vec::new();
        for (serve, req) in self.served(h) {
            let Some(msg) = self.send(req).and_then(|s| (info.decode)(s.val())) else {
                continue;
            };
            let origin = |stored_at: Option<(usize, Event)>| match stored_at {
                Some((j, e)) => vec![
                    Part::Str(format!("stored at {}.{} ", name, j)),
                    Part::Ref(e),
                ],
                None => vec![Part::Str("the initial value".to_string())],
            };
            k += 1;
            let mut facts = AccessFacts {
                object: h,
                k: Some(k),
                read: None,
                written: None,
            };
            let (op, text) = match msg {
                AtomicMsg::Store { value, .. } => {
                    let v = fmt_val(&value);
                    let text = vec![Part::Str(format!("value {}", v))];
                    facts.written = Some(v.clone());
                    current = v;
                    stored_at = Some((k, req));
                    ("STORE", text)
                }
                AtomicMsg::Load { reply } => {
                    let v = match response(&reply) {
                        Some(AtomicMsg::Loaded(x)) => fmt_val(&x),
                        _ => current.clone(),
                    };
                    let mut text = vec![Part::Str(format!("value {}, ", v))];
                    text.extend(origin(stored_at));
                    facts.read = Some(v);
                    ("LOAD", text)
                }
                AtomicMsg::Cas {
                    expected,
                    new,
                    reply,
                } => {
                    let (e, n) = (fmt_val(&expected), fmt_val(&new));
                    let head = format!("expected {}, new {}", e, n);
                    let text = match response(&reply) {
                        Some(AtomicMsg::CasResult(ok)) => {
                            let mut text = vec![Part::Str(format!(
                                "{}: {}, found {}, ",
                                head,
                                if ok { "succeeded" } else { "failed" },
                                current
                            ))];
                            text.extend(origin(stored_at));
                            facts.read = Some(current.clone());
                            if ok {
                                facts.written = Some(n.clone());
                                current = n;
                                stored_at = Some((k, req));
                            }
                            text
                        }
                        _ => vec![Part::Str(head)],
                    };
                    ("CAS", text)
                }
                _ => continue,
            };
            found.push((
                req,
                Access {
                    serve,
                    op,
                    object: format!("{}.{}", name, k),
                    text,
                    facts,
                },
            ));
        }
        self.accesses.extend(found);
    }

    /// Numbers the requests to a lock in the order its thread served them, and tracks who
    /// holds it.
    fn scan_lock(&mut self, h: ThreadId, info: &LockInfo) {
        let name = self.name(h);
        let replies = self.sends_by_loc(h);
        let response = |reply: &Option<Loc>| -> Option<LockMsg> {
            let s = self.send(*replies.get(reply.as_ref()?)?)?;
            (info.decode)(s.val())
        };
        let mut holder: Option<(usize, Event)> = None;
        let mut readers: Vec<(usize, Event)> = Vec::new();
        let mut k = 0;
        let mut found: Vec<(Event, Access)> = Vec::new();
        for (serve, req) in self.served(h) {
            let Some(LockMsg::Request { op, reply }) =
                self.send(req).and_then(|s| (info.decode)(s.val()))
            else {
                continue;
            };
            k += 1;
            let mut text = Vec::new();
            match op {
                LockOp::Lock | LockOp::WriteLock => holder = Some((k, req)),
                LockOp::ReadLock => readers.push((k, req)),
                LockOp::Unlock | LockOp::WriteUnlock => holder = None,
                LockOp::ReadUnlock => readers.retain(|(_, e)| e.thread != req.thread),
                LockOp::TryLock => match response(&reply) {
                    Some(LockMsg::Granted) => {
                        text.push(Part::Str("acquired".to_string()));
                        holder = Some((k, req));
                    }
                    Some(LockMsg::Refused) => {
                        text.push(Part::Str("failed".to_string()));
                        if let Some((j, e)) = holder {
                            text.push(Part::Str(format!(", held since {}.{} ", name, j)));
                            text.push(Part::Ref(e));
                        }
                    }
                    _ => {}
                },
            }
            found.push((
                req,
                Access {
                    serve,
                    op: op.name(),
                    object: format!("{}.{}", name, k),
                    text,
                    facts: AccessFacts {
                        object: h,
                        k: Some(k),
                        read: None,
                        written: None,
                    },
                },
            ));
        }
        // Who holds the lock at the end, for the requests that were never served.
        let mut state = Vec::new();
        let write = if info.kind == LockKind::RwLock {
            "write-held"
        } else {
            "held"
        };
        if let Some((j, e)) = holder {
            state.push(Part::Str(format!("{} since {}.{} ", write, name, j)));
            state.push(Part::Ref(e));
        } else if !readers.is_empty() {
            state.push(Part::Str("read-held since ".to_string()));
            for (i, (j, e)) in readers.iter().enumerate() {
                let sep = if i == 0 { "" } else { ", " };
                state.push(Part::Str(format!("{}{}.{} ", sep, name, j)));
                state.push(Part::Ref(*e));
            }
        }
        self.lock_state.insert(h, state);
        self.accesses.extend(found);
    }

    /// For each awaited receive: the message its helper took, and the owner's event that
    /// receives `Ready` (where the RECV line goes).
    fn scan_helpers(&mut self) {
        let helpers: Vec<ThreadId> = self
            .views
            .iter()
            .filter(|v| matches!(v.kind, Some(ThreadKind::AsyncRecv)))
            .map(|v| v.tid)
            .collect();
        for h in helpers {
            let mut payload = None;
            let mut ready = None;
            for lab in self.labels(h) {
                match lab {
                    LabelEnum::RecvMsg(r) if payload.is_none() => {
                        if let Some(s) = r.rf() {
                            if self.send(s).is_some_and(|slab| !self.runtime_send(slab)) {
                                payload = Some(s);
                            }
                        }
                    }
                    LabelEnum::SendMsg(s) if matches!(poller(s.val()), Some(PollerMsg::Ready)) => {
                        ready = Some(lab.pos());
                    }
                    _ => {}
                }
            }
            let ready_recv = ready.and_then(|r| self.reader(r));
            if let (Some(p), Some(o)) = (payload, ready_recv) {
                self.folded.insert(o, p);
            }
            self.helpers.insert(
                h,
                Helper {
                    payload,
                    ready_recv,
                },
            );
        }
    }

    /// The peer of a send that carries a user's message.
    fn send_peer(&self, s: Event) -> Peer {
        match self.reader(s) {
            None => Peer::Text("-> not received".to_string()),
            Some(r) => match self.helpers.get(&r.thread) {
                Some(h) if h.payload == Some(s) => match h.ready_recv {
                    Some(o) => Peer::To(o),
                    None => Peer::Text("-> taken, then dropped".to_string()),
                },
                Some(_) => Peer::Text("-> taken, then dropped".to_string()),
                None => Peer::To(r),
            },
        }
    }

    fn line(&self, pos: Event, op: &'static str, peer: Peer, object: String, text: &str) -> Line {
        Line {
            pos,
            op,
            peer,
            object,
            text: if text.is_empty() {
                vec![]
            } else {
                vec![Part::Str(text.to_string())]
            },
            anchor: pos,
            message: None,
            access: None,
        }
    }

    fn lines(&mut self) -> Vec<Line> {
        let mut out = Vec::new();
        let tids: Vec<ThreadId> = self
            .views
            .iter()
            .filter(|v| !v.hidden)
            .map(|v| v.tid)
            .collect();
        for t in tids {
            for lab in self.labels(t) {
                if let Some(l) = self.classify(t, lab) {
                    out.push(l);
                }
            }
        }
        out
    }

    /// The line of an event of a user thread, or `None` if the event is hidden.
    fn classify(&mut self, t: ThreadId, lab: &LabelEnum) -> Option<Line> {
        let pos = lab.pos();
        match lab {
            LabelEnum::Begin(_) => None,
            LabelEnum::End(_) => Some(self.line(pos, "END", Peer::None, String::new(), "")),
            LabelEnum::Block(b) => {
                let text = match b.btype() {
                    BlockType::Assume => "by an assume that is false".to_string(),
                    BlockType::Assert => "by a failed assertion".to_string(),
                    BlockType::Join(c) => format!("on the join of {}", self.name(*c)),
                    BlockType::Value(rl, _) => {
                        let user = match rl.locs_opt() {
                            Some(locs) => locs.iter().any(|l| !self.is_runtime_loc(l)),
                            None => true,
                        };
                        if user {
                            "on a receive".to_string()
                        } else if self.blocked_on_lock.contains(&t) {
                            return None; // the lock line already says it is blocked
                        } else {
                            "on an await".to_string()
                        }
                    }
                };
                Some(self.line(pos, "BLOCKED", Peer::None, String::new(), &text))
            }
            LabelEnum::TCreate(tc) => {
                let c = tc.cid();
                match self.kind(c) {
                    Some(ThreadKind::Atomic(info)) => {
                        let text = format!(
                            "of {}, initially {}",
                            short_type(info.type_name),
                            fmt_val(&info.initial)
                        );
                        Some(self.line(pos, "NEW ATOMIC", Peer::None, self.name(c), &text))
                    }
                    Some(ThreadKind::Lock(info)) => {
                        let op = match info.kind {
                            LockKind::Mutex => "NEW MUTEX",
                            LockKind::RwLock => "NEW RWLOCK",
                        };
                        Some(self.line(pos, op, Peer::None, self.name(c), ""))
                    }
                    _ if self.hidden(c) => None,
                    _ => {
                        let daemon = if tc.daemon() { " (daemon)" } else { "" };
                        let text = format!("{}{}", self.name(c), daemon);
                        Some(self.line(pos, "SPAWN", Peer::None, String::new(), &text))
                    }
                }
            }
            LabelEnum::TJoin(j) => {
                let c = j.cid();
                if self.hidden(c) {
                    return None;
                }
                let task = if self.view(c).is_some_and(|v| v.task) {
                    " (task)"
                } else {
                    ""
                };
                let text = format!("{}{}", self.name(c), task);
                Some(self.line(pos, "JOIN", Peer::None, String::new(), &text))
            }
            LabelEnum::Unique(_) => match self.g.mark(pos) {
                Some(Mark::Channel(m)) if m.runtime => None,
                Some(Mark::Channel(m)) => {
                    let text = format!("of {}", short_type(m.type_name));
                    Some(self.line(pos, "NEW CHANNEL", Peer::None, channel_label(pos), &text))
                }
                _ => Some(self.line(pos, "NEW CHANNEL", Peer::None, channel_label(pos), "")),
            },
            LabelEnum::SendMsg(s) => self.classify_send(t, pos, s),
            LabelEnum::RecvMsg(r) => {
                let Some(s) = r.rf() else {
                    let locs: Vec<Loc> = r.recv_loc().locs_opt().cloned().unwrap_or_default();
                    if !locs.is_empty() && locs.iter().all(|l| self.is_runtime_loc(l)) {
                        return None;
                    }
                    let object = match locs.as_slice() {
                        [l] => self
                            .unique
                            .get(l)
                            .map(|&c| channel_label(c))
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    return Some(self.line(pos, "RECV", Peer::Text("timeout".into()), object, ""));
                };
                let slab = self.send(s)?;
                let sender = s.thread;
                if matches!(self.kind(sender), Some(ThreadKind::AsyncRecv)) {
                    // An awaited receive: print the message the helper took.
                    let p = *self.folded.get(&pos)?;
                    let pl = self.send(p)?;
                    let msg = fmt_val(pl.val());
                    let mut l = self.line(pos, "RECV", Peer::From(p), self.object_of(pl), &msg);
                    l.message = Some(msg);
                    return Some(l);
                }
                if self.runtime_send(slab) || self.kind(sender).is_some_and(|k| k.is_runtime()) {
                    let visible_task = !self.hidden(sender);
                    return match poller(slab.val()) {
                        Some(PollerMsg::Cancel) if visible_task => {
                            Some(self.line(pos, "ABORTED", Peer::From(s), String::new(), ""))
                        }
                        Some(PollerMsg::Done)
                            if visible_task
                                && self.view(sender).is_some_and(|v| v.task)
                                && !self.joined.contains(&(t, sender)) =>
                        {
                            let text = format!("{} (task)", self.name(sender));
                            Some(self.line(pos, "JOIN", Peer::None, String::new(), &text))
                        }
                        _ => None,
                    };
                }
                let msg = fmt_val(slab.recv_val(r));
                let mut l = self.line(pos, "RECV", Peer::From(s), self.object_of(slab), &msg);
                l.message = Some(msg);
                Some(l)
            }
            LabelEnum::Inbox(_)
            | LabelEnum::CToss(_)
            | LabelEnum::Choice(_)
            | LabelEnum::Sample(_) => {
                Some(self.line(pos, "", Peer::None, String::new(), &todays_text(lab)))
            }
            #[cfg(feature = "symbolic")]
            LabelEnum::SymbolicVar(_) | LabelEnum::ConstraintEval(_) => {
                Some(self.line(pos, "", Peer::None, String::new(), &todays_text(lab)))
            }
        }
    }

    fn classify_send(&mut self, t: ThreadId, pos: Event, s: &SendMsg) -> Option<Line> {
        // A request to an atomic or a lock.
        if let Some(h) = self.mailbox_of(s) {
            match self.kind(h).cloned() {
                Some(ThreadKind::Atomic(info)) => {
                    return self.access_line(pos, s, h, |v| {
                        (info.decode)(v).and_then(|m| match m {
                            AtomicMsg::Load { .. } => Some(("LOAD", false)),
                            AtomicMsg::Store { .. } => Some(("STORE", false)),
                            AtomicMsg::Cas { .. } => Some(("CAS", false)),
                            _ => None,
                        })
                    })
                }
                Some(ThreadKind::Lock(info)) => {
                    let line = self.access_line(pos, s, h, |v| match (info.decode)(v) {
                        Some(LockMsg::Request { op, .. }) => Some((
                            op.name(),
                            matches!(op, LockOp::Lock | LockOp::ReadLock | LockOp::WriteLock),
                        )),
                        _ => None,
                    });
                    if line
                        .as_ref()
                        .is_some_and(|l| l.access.as_ref().is_some_and(|a| a.k.is_none()))
                    {
                        self.blocked_on_lock.insert(t);
                    }
                    return line;
                }
                Some(k) if k.is_runtime() => return None,
                _ => {}
            }
        }
        if self.runtime_send(s) {
            // Plumbing, except the cancellation of a task.
            if let Some(PollerMsg::Cancel) = poller(s.val()) {
                if let Some(r) = self.reader(pos) {
                    if self.view(r.thread).is_some_and(|v| v.task && !v.hidden) {
                        return Some(self.line(pos, "ABORT", Peer::To(r), String::new(), ""));
                    }
                }
            }
            return None;
        }
        let peer = if s.is_dropped() {
            Peer::Text("-> dropped".to_string())
        } else {
            self.send_peer(pos)
        };
        let msg = fmt_val(s.val());
        let mut l = self.line(pos, "SEND", peer, self.object_of(s), &msg);
        l.message = Some(msg);
        Some(l)
    }

    /// The line of a request to an atomic or a lock. `describe` returns the operation and
    /// whether it acquires a lock (an unserved acquisition prints as blocked).
    fn access_line(
        &self,
        pos: Event,
        s: &SendMsg,
        h: ThreadId,
        describe: impl Fn(&Val) -> Option<(&'static str, bool)>,
    ) -> Option<Line> {
        if let Some(a) = self.accesses.get(&pos) {
            return Some(Line {
                pos,
                op: a.op,
                peer: Peer::None,
                object: a.object.clone(),
                text: a.text.clone(),
                anchor: a.serve,
                message: None,
                access: Some(a.facts.clone()),
            });
        }
        let (op, acquire) = describe(s.val())?;
        let mut text = Vec::new();
        if acquire {
            text.push(Part::Str("blocked".to_string()));
            if let Some(state) = self.lock_state.get(&h).filter(|s| !s.is_empty()) {
                text.push(Part::Str(", ".to_string()));
                text.extend(state.iter().cloned());
            }
        } else {
            text.push(Part::Str("not served".to_string()));
        }
        Some(Line {
            pos,
            op,
            peer: Peer::None,
            object: self.name(h),
            text,
            anchor: pos,
            message: None,
            access: Some(AccessFacts {
                object: h,
                k: None,
                read: None,
                written: None,
            }),
        })
    }

    fn objects(&self) -> Vec<ObjectView> {
        self.views
            .iter()
            .filter_map(|v| {
                let (kind, initial) = match &v.kind {
                    Some(ThreadKind::Atomic(info)) => {
                        (ObjectKind::Atomic, Some(fmt_val(&info.initial)))
                    }
                    Some(ThreadKind::Lock(info)) if info.kind == LockKind::Mutex => {
                        (ObjectKind::Mutex, None)
                    }
                    Some(ThreadKind::Lock(_)) => (ObjectKind::RwLock, None),
                    _ => return None,
                };
                Some(ObjectView {
                    tid: v.tid,
                    name: v.name.clone(),
                    kind,
                    creator: v.parent.unwrap_or(main_thread_id()),
                    initial,
                })
            })
            .collect()
    }
}
