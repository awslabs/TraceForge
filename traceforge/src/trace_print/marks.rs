//! Marks: what the libraries tell the printer about the threads and channels they create.
//!
//! A mark is stored in a side table of the execution graph, keyed by the event that created
//! the thread (`TCREATE`) or the channel (`UNIQUE`). Only the printer reads it: it plays no
//! part in consistency checks, symmetry reduction, replay validation or serialised traces.

use crate::loc::Loc;
use crate::msg::Val;

#[derive(Clone, Debug)]
pub(crate) enum Mark {
    Thread(ThreadKind),
    Channel(ChannelMark),
}

/// Set on every channel created by `channel::Builder::build`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChannelMark {
    /// `std::any::type_name` of the messages.
    pub(crate) type_name: &'static str,
    /// The runtime created this channel for its own protocols.
    pub(crate) runtime: bool,
}

/// What a thread is, when it is not a plain user thread.
#[derive(Clone, Debug)]
pub(crate) enum ThreadKind {
    /// A task created by `future::spawn`. `named` is true if the user gave it a name.
    Task { named: bool },
    /// The helper that performs an awaited receive for the thread that created it.
    AsyncRecv,
    /// The thread that holds the value of an atomic.
    Atomic(AtomicInfo),
    /// The thread that implements a mutex or a rwlock.
    Lock(LockInfo),
}

impl ThreadKind {
    /// A thread of the runtime, not printed.
    pub(crate) fn is_runtime(&self) -> bool {
        !matches!(self, ThreadKind::Task { .. })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AtomicInfo {
    pub(crate) type_name: &'static str,
    pub(crate) initial: Val,
    /// Reads a request to, or a response from, this atomic's thread.
    pub(crate) decode: fn(&Val) -> Option<AtomicMsg>,
}

/// A message of the atomic protocol, independent of the type of the value.
pub(crate) enum AtomicMsg {
    Load { reply: Loc },
    Store { value: Val },
    Cas { expected: Val, new: Val, reply: Loc },
    Loaded(Val),
    Stored,
    CasResult(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockKind {
    Mutex,
    RwLock,
}

#[derive(Clone, Debug)]
pub(crate) struct LockInfo {
    pub(crate) kind: LockKind,
    /// Reads a request to, or a response from, this lock's thread.
    pub(crate) decode: fn(&Val) -> Option<LockMsg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockOp {
    Lock,
    TryLock,
    Unlock,
    ReadLock,
    WriteLock,
    ReadUnlock,
    WriteUnlock,
}

impl LockOp {
    pub(crate) fn name(self) -> &'static str {
        match self {
            LockOp::Lock => "LOCK",
            LockOp::TryLock => "TRY_LOCK",
            LockOp::Unlock => "UNLOCK",
            LockOp::ReadLock => "READ_LOCK",
            LockOp::WriteLock => "WRITE_LOCK",
            LockOp::ReadUnlock => "READ_UNLOCK",
            LockOp::WriteUnlock => "WRITE_UNLOCK",
        }
    }
}

/// A message of the mutex or rwlock protocol.
pub(crate) enum LockMsg {
    Request {
        op: LockOp,
        reply: Option<Loc>,
    },
    /// The lock was acquired, or the release acknowledged.
    Granted,
    /// A `try_lock` found the lock held.
    Refused,
}
