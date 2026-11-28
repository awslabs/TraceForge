//! TraceForge's implementation of [`std::thread`].

use std::cmp::Ordering;
use std::convert::TryFrom;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};

use crate::event_label::{End, TJoin};
use crate::msg::Message;
use crate::must::Must;
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread::{self, switch};
use crate::trace_print::ThreadKind;
use crate::Val;

/// A unique identifier for a running thread
// Do not derive PartialOrd or Ord --
// Reason 1: Rust's ThreadId does not derive them and we should be as interface-compatible as possible
//
// Reason 2: symmetry reduction depends on ThreadId's not being comparable.
// Symmetry reduction uses the fact that two threads that receive exactly the same history of messages act
// exactly alike if their Id's are swapped. This is true if the ThreadId is an opaque value and a thread
// can only compare it's thread id `current().id()` with others for equality/disequality.
// However, if the thread id's are comparable, a thread can decide to act differently based on whether
// it's thread id is lower than someone else.
//
// Technically, swapping thread id's will not be a group automorphism if Ord is allowed, and symmetry
// reduction heuristics need a group structure.
//
// You might want to add PartialOrd because you want to put ThreadId's as a key in a BTreeMap.
// Instead, use a HashMap. Even though HashMap iterations are randomly ordered, this should be fine
// for most applications. If you need a deterministic version, you should use a deterministic hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct ThreadId {
    opaque_id: u32,
}

impl std::fmt::Debug for ThreadId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // Inside the pretty printer: the thread's name.
        if let Some(name) = crate::trace_print::names::thread_name(*self) {
            return f.write_str(&name);
        }
        // Elsewhere: the same text as `derive(Debug)`.
        f.debug_struct("ThreadId")
            .field("opaque_id", &self.opaque_id)
            .finish()
    }
}

impl Serialize for ThreadId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("t{}", self.opaque_id))
    }
}

impl Display for ThreadId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("t{}", self.opaque_id))
    }
}

pub struct ThreadIdFromStrError {
    msg: String,
}

impl Display for ThreadIdFromStrError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl TryFrom<String> for ThreadId {
    type Error = ThreadIdFromStrError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.starts_with('t') {
            let mut num = s.clone();
            num.remove(0);
            match num.parse::<u32>() {
                Ok(tid) => Ok(ThreadId { opaque_id: tid }),
                Err(_) => Err(ThreadIdFromStrError {
                    msg: format!("Can't parse {} as a number", &s),
                }),
            }
        } else {
            Err(ThreadIdFromStrError {
                msg: format!("`{}` should begin with `t`", &s),
            })
        }
    }
}

/// Construct a new ThreadId from a given integer.
///
/// This function should be avoided if possible or used with caution. Although TraceForge assigns
/// thread ids in order, this is not guaranteed in all cases, so code in TraceForge models should not assume
/// that it can guess the next thread ID.
///
/// (During backtrack searches, a thread which is launched from the same location in the code
/// can be assigned a new unique number if the sequence of scheduling decisions that leads to
/// its creation is not the same as in a previous execution.)
///
/// However, this function is public because there are some cases such as constructing traces where
/// the creation of a thread id is needed.
///
/// Instead of associating an integer with each ThreadId, a better design is to give each thread
/// a name using traceForge::thread::builder.name()
pub fn construct_thread_id(numeric_id: u32) -> ThreadId {
    ThreadId {
        opaque_id: numeric_id,
    }
}

/// Convert a TraceForge ThreadId into an integer. This should be avoided if possible because thread ids
/// are not always assigned sequentially, and thread ids can change during backtrack searches.
///
/// See `construct_thread_id` for more information.
///
/// Instead of associating an integer with each ThreadId, a better design is to give each thread
/// a name using traceForge::thread::builder.name()
impl From<ThreadId> for u32 {
    fn from(tid: ThreadId) -> Self {
        tid.opaque_id
    }
}

/// Future versions of TraceForge will remove this conversion.
///
/// It will be removed because this leaks internal details about how TraceForge assigns ids to threads,
/// and using this inside a model can cause confusion or soundness issues if a thread id is used
/// for something other than equality comparison.
///
/// We are not removing it in this version of TraceForge because some models depend on it, and the
/// model-based testing generation depends on it as well. It is deprecated and we'll remove it
/// after the things that depend on it have been adjusted.
impl From<ThreadId> for usize {
    fn from(tid: ThreadId) -> Self {
        tid.opaque_id as usize
    }
}

impl ThreadId {
    /// See comments on construct_thread_id and the conversion to u32
    pub(crate) fn to_number(self) -> u32 {
        self.opaque_id
    }
}
/// Returns the main thread ID.
pub fn main_thread_id() -> ThreadId {
    ThreadId { opaque_id: 0 }
}

// Needed in order to have the trait Ord
impl PartialOrd for ThreadId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Needed in order to use a BTreeMap whose keys are thread ids
impl Ord for ThreadId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.opaque_id.cmp(&other.opaque_id)
    }
}

/// A handle to a thread.
#[derive(Debug, Clone)]
pub struct Thread {
    pub(crate) name: Option<String>,
    pub(crate) id: ThreadId,
}

impl Thread {
    /// Gets the thread's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Gets the thread's unique identifier
    pub fn id(&self) -> ThreadId {
        self.id
    }
}

/// Spawn a new thread, returning a JoinHandle for it.
///
/// The join handle can be used (via the `join` method) to block until the child thread has
/// finished.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Message + 'static,
{
    switch();
    let jh = spawn_without_switch(f, None, false, None, None, None);
    switch();
    jh
}

pub fn spawn_daemon<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Message + 'static,
{
    switch();
    let jh = spawn_without_switch(f, None, true, None, None, None);
    switch();
    jh
}

/// `spawn_without_switch` is an internal TraceForge function that actually does the work of spawning, but
/// does not call `switch`.
///
/// `switch` introduces a scheduling point, which is needed for some consistency models if the
/// consistency model specifies that there is an ordering of messages sent to a particular thread
/// (i.e., mailbox or stronger).
///
/// This allows the caller (which is some other TraceForge version of the `spawn` function) to spawn
/// a thread, retrieve the id, and do something with the id before the `switch` occurs.
///
/// The reason this matters is that during replay, if the spawned thread/monitor hits a block,
/// the whole execution ends, and the main thread that calls `spawn` will never regain control
/// (and it would fail to register the monitor).
pub(crate) fn spawn_without_switch<F, T>(
    f: F,
    name: Option<String>,
    is_daemon: bool,
    stack_size: Option<usize>,
    sym_cid: Option<ThreadId>,
    kind: Option<ThreadKind>,
) -> JoinHandle<T>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Message + 'static,
{
    let stack_size =
        stack_size.unwrap_or_else(|| ExecutionState::with(|s| s.must.borrow().config().stack_size));
    let (task_id, tid) = {
        let f = move || {
            let ret = f();

            // // Run thread-local destructors before publishing the result, because
            // // [`JoinHandle::join`] says join "waits for the associated thread to finish", but
            // // destructors TraceForge be run on the thread, so it can't be considered "finished" if the
            // // destructors haven't run yet.
            // // See `pop_local` for details on why this loop looks this slightly funky way.
            // while let Some(local) = ExecutionState::with(|state| state.current_mut().pop_local()) {
            //     drop(local);
            // }

            // Publish the result and unstuck the waiter. We need to do this now, because once this
            // closure completes, the Execution will consider this task Finished and invoke the
            // scheduler.
            ExecutionState::with(|state| {
                let pos = state.next_pos();
                state
                    .must
                    .borrow_mut()
                    .handle_tend(End::new(pos, Val::new(ret)));
                Must::unstuck_joiners(state, pos.thread);
            });
        };
        let cid = ExecutionState::spawn_thread(f, stack_size, name.clone());
        let tid = ExecutionState::with(|state| {
            let pos = state.next_pos();
            let tid = state.must.borrow().next_thread_id(&pos);
            state.must.borrow_mut().handle_tcreate(
                tid,
                cid,
                sym_cid,
                pos,
                name.clone(),
                is_daemon,
                kind,
            );
            tid
        });
        (cid, tid)
    };

    let thread = Thread { id: tid, name };

    JoinHandle {
        task_id,
        thread,
        _p: PhantomData::<T>,
    }
}

/// An owned permission to join on a thread (block on its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    thread: Thread,
    _p: PhantomData<T>,
}

impl<T: 'static> JoinHandle<T> {
    /// Waits for the associated thread to finish.
    pub fn join(self) -> std::thread::Result<T> {
        let ret = loop {
            thread::switch();
            let val = ExecutionState::with(|s| {
                let target_task_id = s.get(self.task_id).id();
                let target_id = s.must.borrow().to_thread_id(target_task_id);
                let pos = s.next_pos();
                s.must.borrow_mut().handle_tjoin(TJoin::new(pos, target_id))
            });

            if let Some(message) = val {
                if message.is_pending() {
                    // Block the task to wait for the joined task to finish.
                    ExecutionState::with(|s| s.current_mut().stuck());
                } else {
                    break message;
                }
            }

            ExecutionState::with(|s| s.prev_pos());
        };
        let actual_type = &ret.type_name;
        Ok(*(ret.as_any().downcast().unwrap_or_else(|_| {
            panic!(
                "Expected a thread result of {}, but got {}",
                std::any::type_name::<T>(),
                actual_type
            );
        })))
    }

    /// Extracts a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

/// Puts the current thread to sleep for at least the specified amount of time.
// Note that Shuttle does not model time, so this behaves just like a context switch.
pub fn sleep(_dur: Duration) {
    thread::switch();
}

/// Get a handle to the thread that invokes it
pub fn current() -> Thread {
    let (tid, name) = ExecutionState::with(|s| {
        let me = s.current();
        let tid = s.must.borrow_mut().to_thread_id(me.id());
        (tid, me.name())
    });

    Thread { id: tid, name }
}

/// Get the ID of the thread that invokes it. This is a shortcut for current().id()
pub fn current_id() -> ThreadId {
    current().id()
}

/// Thread factory, which can be used in order to configure the properties of a new thread.
#[derive(Debug, Default)]
pub struct Builder {
    name: Option<String>,
    stack_size: Option<usize>,
    kind: Option<ThreadKind>,
}

impl Builder {
    /// Generates the base configuration for spawning a thread, from which configuration methods can be chained.
    pub fn new() -> Self {
        Self {
            name: None,
            stack_size: None,
            kind: None,
        }
    }

    /// Tells the pretty printer what this thread is, e.g. the thread behind an atomic.
    pub(crate) fn kind(mut self, kind: ThreadKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Names the thread-to-be. Currently the name is used for identification only in panic messages.
    pub fn name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    /// Sets the size of the stack (in bytes) for the new thread.
    pub fn stack_size(mut self, stack_size: usize) -> Self {
        self.stack_size = Some(stack_size);
        self
    }

    /// Spawns a new thread by taking ownership of the Builder, and returns an `io::Result` to its `JoinHandle`.
    pub fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Message + 'static,
    {
        switch();
        let jh = Ok(spawn_without_switch(
            f,
            self.name,
            false,
            self.stack_size,
            None,
            self.kind,
        ));
        switch();
        jh
    }

    /// Spawns a new daemon thread by taking ownership of the Builder, and returns an `io::Result` to its `JoinHandle`.
    /// See the TraceForge book for daemon threads: these are threads that need not terminate
    pub fn spawn_daemon<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
    where
        F: FnOnce() -> T,
        F: Send + 'static,
        T: Message + 'static,
    {
        switch();
        let jh = Ok(spawn_without_switch(
            f,
            self.name,
            true,
            self.stack_size,
            None,
            self.kind,
        ));
        switch();
        jh
    }
}

/// A thread local storage key which owns its contents
// Sadly, the fields of this thing need to be public because function pointers in const fns are
// unstable, so an explicit instantiation is the only way to construct this struct. User code should
// not rely on these fields.
pub struct LocalKey<T: 'static> {
    #[doc(hidden)]
    pub init: fn() -> T,
    #[doc(hidden)]
    pub _p: PhantomData<T>,
}

// Safety: `LocalKey` implements thread-local storage; each thread sees its own value of the type T.
unsafe impl<T> Send for LocalKey<T> {}
unsafe impl<T> Sync for LocalKey<T> {}

impl<T: 'static> std::fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalKey").finish_non_exhaustive()
    }
}

impl<T: 'static> LocalKey<T> {
    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.try_with(f).expect(
            "cannot access a Thread Local Storage value \
            during or after destruction",
        )
    }

    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet. If the
    /// key has been destroyed (which may happen if this is called in a destructor), this function
    /// will return an AccessError.
    pub fn try_with<F, R>(&'static self, f: F) -> Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        let value = self.get().unwrap_or_else(|| {
            // let value = (self.init)();

            // ExecutionState::with(move |state| {
            //     state.current_mut().init_local(self, value);
            // });

            self.get().unwrap()
        })?;

        Ok(f(value))
    }

    fn get(&'static self) -> Option<Result<&'static T, AccessError>> {
        // Safety: see the usage below
        // unsafe fn extend_lt<'a, 'b, T>(t: &'a T) -> &'b T {
        //     std::mem::transmute(t)
        // }

        ExecutionState::with(|_state| {
            // if let Ok(value) = state.current().local(self)? {
            // Safety: unfortunately the lifetime of a value in our thread-local storage is
            // bound to the lifetime of `ExecutionState`, which has no visible relation to the
            // lifetime of the thread we're running on. However, *we* know that the
            // `ExecutionState` outlives any thread, including the caller, and so it's safe to
            // give the caller the lifetime it's asking for here.
            // Some(Ok(unsafe { extend_lt(value) }))
            // } else {
            // Slot has already been destructed
            Some(Err(AccessError))
            // }
        })
    }
}

/// An error returned by [`LocalKey::try_with`]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub struct AccessError;

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt("already destroyed", f)
    }
}

impl std::error::Error for AccessError {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn threadid_is_serializable() {
        let tid = ThreadId { opaque_id: 123 };
        let str = serde_json::to_string_pretty(&tid).unwrap();
        assert_eq!("\"t123\"", str);
        let deserialized: ThreadId = serde_json::from_str(&str).unwrap();
        assert_eq!(deserialized, tid);
    }
}
