//! An asynchronous `Mutex`-like type.

use std::cell::UnsafeCell;
use std::error::Error;
use std::fmt::{self, Debug, Display};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use crate::channel::{self, Sender};
use crate::thread::{self, ThreadId};
use crate::{recv_tagged_msg_block, send_tagged_msg};

#[derive(Clone, Debug, PartialEq)]
pub enum LockRequest {
    Lock(ThreadId, Sender<MsgResponse>),
    TryLock(ThreadId, Sender<MsgResponse>),
    Unlock(ThreadId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum MsgResponse {
    LockGranted,
    LockAlreadyHeld,
    Unlocked,
}

const LOCK_TAG: u32 = 1;
const UNLOCK_TAG: u32 = 2;
const TRYLOCK_TAG: u32 = 3;

pub struct Synchronizer {
    // The current holder of the lock, if any
    holder: Option<ThreadId>,
}

impl Synchronizer {
    pub fn new() -> Self {
        Self { holder: None }
    }

    pub fn execute(&mut self) {
        loop {
            // The mutex starts out unlocked. Only read LOCK_TAG/TRYLOCK_TAG messages.
            let req: LockRequest =
                recv_tagged_msg_block(|_, tag| tag == Some(LOCK_TAG) || tag == Some(TRYLOCK_TAG));
            match req {
                LockRequest::Lock(tid, chan) => match self.holder {
                    None => {
                        self.holder = Some(tid);
                        chan.send_msg(MsgResponse::LockGranted);
                    }
                    Some(_) => {
                        unreachable!()
                    }
                },
                LockRequest::TryLock(tid, chan) => match self.holder {
                    None => {
                        self.holder = Some(tid);
                        chan.send_msg(MsgResponse::LockGranted);
                    }
                    Some(_) => {
                        unreachable!()
                    }
                },
                LockRequest::Unlock(_) => {
                    panic!("Unlocking a lock that is not held");
                }
            }
            // now the mutex is locked. Only read TRYLOCK_TAG or UNLOCK_TAG messages.

            loop {
            let req: LockRequest =
                recv_tagged_msg_block(|_, tag| tag == Some(TRYLOCK_TAG) || tag == Some(UNLOCK_TAG));
            match req {
                LockRequest::Lock(_, _) => {
                    panic!("Locking a lock that is already held");
                }
                LockRequest::TryLock(_, chan) => match self.holder {
                    None => {
                        unreachable!()
                    }
                    Some(_) => {
                        chan.send_msg(MsgResponse::LockAlreadyHeld);
                    }
                },
                LockRequest::Unlock(tid) => {
                    if let Some(t) = self.holder {
                        if t == tid {
                            self.holder = None;
                            break; // break out of loop
                        } else {
                            panic!("Unlocking by different thread id");
                        }
                    } else {
                        panic!("Unlocking a lock that is not held");
                    }
                }
            }
            }
        }
    }
}

impl Default for Synchronizer {
    fn default() -> Self {
        Self::new()
    }
}

/// An asynchronous semaphore
pub struct Mutex<T: ?Sized> {
    synchronizer: ThreadId,
    inner: UnsafeCell<T>,
}

/// A handle to a held `Mutex`. The guard can be held across any `.await` point
/// as it is [`Send`].
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

impl<T: ?Sized + Display> Display for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&**self, f)
    }
}

/// An owned handle to a held `Mutex`.
pub struct OwnedMutexGuard<T: ?Sized> {
    mutex: Arc<Mutex<T>>,
}

/// Error returned from the [`Mutex::try_lock`], `RwLock::try_read` and
/// `RwLock::try_write` functions.
#[derive(Debug)]
pub struct TryLockError(pub(super) ());

impl Display for TryLockError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "operation would block")
    }
}

impl Error for TryLockError {}

// As long as T: Send, it's fine to send and share Mutex<T> between threads.
// If T was not Send, sending and sharing a Mutex<T> would be bad, since you can
// access T through Mutex<T>.
unsafe impl<T> Send for Mutex<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for Mutex<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for MutexGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for OwnedMutexGuard<T> where T: ?Sized + Send + Sync {}

impl<T: ?Sized> Mutex<T> {
    /// Creates a new lock in an unlocked state ready for use.
    pub fn new(t: T) -> Self
    where
        T: Sized,
    {
        let mut tsync = Synchronizer::new();

        let tsync_handle = thread::Builder::new()
            .name("Mutex synchronizer".to_string())
            .spawn_daemon(move || {
                tsync.execute();
            })
            .unwrap();

        Self {
            synchronizer: tsync_handle.thread().id(),
            inner: UnsafeCell::new(t),
        }
    }

    async fn acquire(&self) {
        let chan = channel::Builder::<MsgResponse>::new().build();
        send_tagged_msg(
            self.synchronizer,
            LOCK_TAG,
            LockRequest::Lock(thread::current().id(), chan.0),
        );
        match chan.1.recv_msg_block() {
            MsgResponse::LockGranted => (),
            _ => panic!("Error in the implementation of Mutex"),
        }
    }

    /// Locks this mutex, causing the current task to yield until the lock has
    /// been acquired.  When the lock has been acquired, function returns a
    /// [`MutexGuard`].    
    pub async fn lock(&self) -> MutexGuard<'_, T> {
        self.acquire().await;

        MutexGuard { mutex: self }
    }

    /// Blockingly locks this `Mutex`. When the lock has been acquired, function returns a
    /// [`MutexGuard`].
    ///
    /// This method is intended for use cases where you
    /// need to use this mutex in asynchronous code as well as in synchronous code.    
    pub fn blocking_lock(&self) -> MutexGuard<'_, T> {
        crate::future::block_on(self.lock())
    }

    pub fn blocking_lock_owned(self: Arc<Self>) -> OwnedMutexGuard<T> {
        crate::future::block_on(self.lock_owned())
    }

    /// Locks this mutex, causing the current task to yield until the lock has
    /// been acquired. When the lock has been acquired, this returns an
    /// [`OwnedMutexGuard`].
    pub async fn lock_owned(self: Arc<Self>) -> OwnedMutexGuard<T> {
        self.acquire().await;

        OwnedMutexGuard { mutex: self }
    }

    fn try_acquire(&self) -> Result<(), TryLockError> {
        let chan = channel::Builder::<MsgResponse>::new().build();
        send_tagged_msg(
            self.synchronizer,
            TRYLOCK_TAG,
            LockRequest::TryLock(thread::current().id(), chan.0),
        );
        match chan.1.recv_msg_block() {
            MsgResponse::LockGranted => Ok(()),
            MsgResponse::LockAlreadyHeld => Err(TryLockError(())),
            MsgResponse::Unlocked => panic!("Error in implementation of Mutex"),
        }
    }

    /// Attempts to acquire the lock, and returns [`TryLockError`] if the
    /// lock is currently held somewhere else.
    pub fn try_lock(&self) -> Result<MutexGuard<'_, T>, TryLockError> {
        match self.try_acquire() {
            Ok(_) => Ok(MutexGuard { mutex: self }),
            Err(_) => Err(TryLockError(())),
        }
    }

    /// Attempts to acquire the lock, and returns [`TryLockError`] if the lock
    /// is currently held somewhere else.
    pub fn try_lock_owned(self: Arc<Self>) -> Result<OwnedMutexGuard<T>, TryLockError> {
        match self.try_acquire() {
            Ok(_) => Ok(OwnedMutexGuard { mutex: self }),
            Err(_) => Err(TryLockError(())),
        }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place -- the mutable borrow statically guarantees no locks exist.
    ///
    ///
    pub fn get_mut(&mut self) -> &mut T {
        unsafe {
            // Safety: This is https://github.com/rust-lang/rust/pull/76936
            &mut *self.inner.get()
        }
    }

    /// Consumes the mutex, returning the underlying data.
    pub fn into_inner(self) -> T
    where
        T: Sized,
    {
        self.inner.into_inner()
    }
}

impl<T: ?Sized + Debug> Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: The TraceForge model checking engine runs single-threaded, so
        // only that thread is able to access `inner` at the time of this call.
        Debug::fmt(&unsafe { &*self.inner.get() }, f)
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.inner.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        send_tagged_msg(
            self.mutex.synchronizer,
            UNLOCK_TAG,
            LockRequest::Unlock(thread::current().id()),
        );
    }
}

impl<T: ?Sized + Debug> Debug for MutexGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.mutex, f)
    }
}

impl<T: ?Sized> Drop for OwnedMutexGuard<T> {
    fn drop(&mut self) {
        send_tagged_msg(
            self.mutex.synchronizer,
            UNLOCK_TAG,
            LockRequest::Unlock(thread::current().id()),
        );
    }
}

impl<T: ?Sized> Deref for OwnedMutexGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.inner.get() }
    }
}

impl<T: ?Sized> DerefMut for OwnedMutexGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.inner.get() }
    }
}

impl<T: ?Sized + Debug> Debug for OwnedMutexGuard<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.mutex, f)
    }
}

impl<T> From<T> for Mutex<T> {
    fn from(s: T) -> Self {
        Self::new(s)
    }
}

impl<T> Default for Mutex<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}
