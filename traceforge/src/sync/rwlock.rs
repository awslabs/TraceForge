// TraceForge's implementation of RwLock:
// This is incomplete and does not support the entire API for tokio::sync::RwLock

use std::cell::UnsafeCell;
use std::collections::HashSet;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use crate::sync::TryLockError;

const MAX_READERS: usize = usize::MAX >> 3;

//#[derive(std::fmt::Debug, Clone, PartialEq)]
pub struct RwLock<T: ?Sized> {
    backing_tid: crate::thread::ThreadId,
    data: UnsafeCell<T>,
}

//#[derive(std::fmt::Debug)]
pub struct RwLockReadGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    data: *const T,
}

//#[derive(std::fmt::Debug)]
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    data: *mut T,
}

/// An owned handle to a held `RwLock`.
//#[derive(std::fmt::Debug)]
pub struct OwnedRwLockReadGuard<T: ?Sized, U: ?Sized = T> {
    lock: Arc<RwLock<T>>,
    data: *const U,
}

pub struct OwnedRwLockWriteGuard<T: ?Sized, U: ?Sized = T> {
    lock: Arc<RwLock<T>>,
    data: *mut U,
}

// As long as T: Send + Sync, it's fine to send and share RwLock<T> between threads.
// If T were not Send, sending and sharing a RwLock<T> would be bad, since you can access T through
// RwLock<T>.
unsafe impl<T> Send for RwLock<T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for RwLock<T> where T: ?Sized + Send + Sync {}
// NB: These impls need to be explicit since we're storing a raw pointer.
// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send`.
unsafe impl<T> Send for RwLockReadGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for RwLockReadGuard<'_, T> where T: ?Sized + Send + Sync {}
// T is required to be `Send` because an OwnedRwLockReadGuard can be used to drop the value held in
// the RwLock, unlike RwLockReadGuard.
unsafe impl<T, U> Send for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Sync,
{
}
unsafe impl<T, U> Sync for OwnedRwLockReadGuard<T, U>
where
    T: ?Sized + Send + Sync,
    U: ?Sized + Send + Sync,
{
}
unsafe impl<T> Sync for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Sync for OwnedRwLockWriteGuard<T> where T: ?Sized + Send + Sync {}

// Safety: Stores a raw pointer to `T`, so if `T` is `Sync`, the lock guard over
// `T` is `Send` - but since this is also provides mutable access, we need to
// make sure that `T` is `Send` since its value can be sent across thread
// boundaries.
unsafe impl<T> Send for RwLockWriteGuard<'_, T> where T: ?Sized + Send + Sync {}
unsafe impl<T> Send for OwnedRwLockWriteGuard<T> where T: ?Sized + Send + Sync {}

impl<T: ?Sized> RwLock<T>
where
    T: Sized,
{
    pub fn new(t: T) -> Self
    where
        T: Sized,
    {
        Self::with_max_readers(t, MAX_READERS)
    }

    pub fn with_max_readers(t: T, max_readers: usize) -> Self {
        assert!(
            max_readers > 0,
            "a RwLock may not be created with zero readers"
        );
        assert!(
            max_readers <= MAX_READERS,
            "a RwLock may not be created with more than {} readers",
            MAX_READERS
        );
        let mut synchronizer = MustRwLock::new(max_readers);

        let backing_tid = crate::thread::Builder::new()
            .name("rwlock".into())
            .spawn_daemon(move || synchronizer.run())
            .unwrap()
            .thread()
            .id();

        Self {
            backing_tid,
            data: UnsafeCell::new(t),
        }
    }

    pub async fn read(&self) -> RwLockReadGuard<'_, T> {
        crate::send_tagged_msg(
            self.backing_tid,
            ACQUIRE_READ_TAG,
            Request::AcquireRead(crate::thread::current().id()),
        );
        let backing_tid = self.backing_tid;
        let resp = crate::recv_tagged_msg_block(move |tid, _| tid == backing_tid);
        match resp {
            Response::AcquireRead => RwLockReadGuard {
                lock: self,
                data: self.data.get(),
            },
            _ => panic!("unexpected response"),
        }
    }
    pub fn blocking_read(&self) -> RwLockReadGuard<'_, T> {
        crate::future::block_on(self.read())
    }

    pub async fn try_read(&self) -> Result<RwLockReadGuard<'_, T>, TryLockError> {
        todo!()
        /*
        crate::send_tagged_msg(
            self.backing_tid,
            TRY_ACQUIRE_READ_TAG,
            Request::TryAcquireRead(crate::thread::current().id()),
        );
        let backing_tid = self.backing_tid;
        let resp = crate::recv_tagged_msg_block(move |tid, _| tid == backing_tid);
        match resp {
            Response::TryAcquireRead => Some(RwLockReadGuard {
                lock: self,
                data: self.data.get(),
            }),
            Response::AcquireRead => Some(RwLockReadGuard {
                lock: self,
                data: self.data.get(),
            }),
            _ => None,
        }
        */
    }

    pub async fn write(&self) -> RwLockWriteGuard<'_, T> {
        crate::send_tagged_msg(
            self.backing_tid,
            ACQUIRE_WRITE_TAG,
            Request::AcquireWrite(crate::thread::current().id()),
        );
        let backing_tid = self.backing_tid;
        let resp = crate::recv_tagged_msg_block(move |tid, _| tid == backing_tid);
        match resp {
            Response::AcquireWrite => RwLockWriteGuard {
                lock: self,
                data: self.data.get(),
            },
            _ => panic!("unexpected response"),
        }
    }

    pub fn blocking_write(&self) -> RwLockWriteGuard<'_, T> {
        crate::future::block_on(self.write())
    }

    pub async fn write_owned(self: Arc<Self>) -> OwnedRwLockWriteGuard<T> {
        // TODO: Do we need ManuallyDrop for the lock?
        crate::send_tagged_msg(
            self.backing_tid,
            ACQUIRE_WRITE_TAG,
            Request::AcquireWrite(crate::thread::current().id()),
        );
        let backing_tid = self.backing_tid;
        let resp = crate::recv_tagged_msg_block(move |tid, _| tid == backing_tid);
        match resp {
            Response::AcquireWrite => OwnedRwLockWriteGuard {
                data: self.data.get(),
                lock: self,
            },
            _ => panic!("unexpected response"),
        }
    }
}

impl<T> From<T> for RwLock<T>
where
    T: Send + Sync,
{
    fn from(s: T) -> Self {
        Self::new(s)
    }
}

impl<T> Default for RwLock<T>
where
    T: Default + std::fmt::Debug + Clone + PartialEq + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockReadGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLockWriteGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() { return; }
        let tid = self.lock.backing_tid;
        crate::send_tagged_msg(
            tid,
            RELEASE_READ_TAG,
            Request::ReleaseRead(crate::thread::current().id()),
        );
        let _: Response = crate::recv_tagged_msg_block(move |t, _| t == tid);
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() { return; }
        let tid = self.lock.backing_tid;
        crate::send_tagged_msg(
            tid,
            RELEASE_WRITE_TAG,
            Request::ReleaseWrite(crate::thread::current().id()),
        );
        let _: Response = crate::recv_tagged_msg_block(move |t, _| t == tid);
    }
}

impl<T> Deref for RwLockReadGuard<'_, T>
where
    T: std::fmt::Debug + Clone + PartialEq + Send + 'static,
{
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}

impl<T> Deref for RwLockWriteGuard<'_, T>
where
    T: std::fmt::Debug + Clone + PartialEq + Send + 'static,
{
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}

// This is unique to the RwLockWriteGuard; everything else is the same.
impl<T> DerefMut for RwLockWriteGuard<'_, T>
where
    T: std::fmt::Debug + Clone + PartialEq + Send + 'static,
{
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
}

impl<T> fmt::Display for RwLockReadGuard<'_, T>
where
    T: std::fmt::Debug + Clone + PartialEq + Send + std::fmt::Display + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T> fmt::Display for RwLockWriteGuard<'_, T>
where
    T: std::fmt::Debug + Clone + PartialEq + Send + std::fmt::Display + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: ?Sized, U: ?Sized> Deref for OwnedRwLockReadGuard<T, U> {
    type Target = U;

    fn deref(&self) -> &U {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized, U: ?Sized> fmt::Debug for OwnedRwLockReadGuard<T, U>
where
    U: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, U: ?Sized> Drop for OwnedRwLockReadGuard<T, U> {
    fn drop(&mut self) {
        if std::thread::panicking() { return; }
        let tid = self.lock.backing_tid;
        crate::send_tagged_msg(
            tid,
            RELEASE_READ_TAG,
            Request::ReleaseWrite(crate::thread::current().id()),
        );
        let _: Response = crate::recv_tagged_msg_block(move |t, _| t == tid);
        //TODO: Do we need ManuallyDrop?
        // unsafe { ManuallyDrop::drop(&mut self.lock) };
    }
}

impl<T: ?Sized> Deref for OwnedRwLockWriteGuard<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.data }
    }
}

impl<T: ?Sized> DerefMut for OwnedRwLockWriteGuard<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
}

impl<T: ?Sized> fmt::Debug for OwnedRwLockWriteGuard<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized, U: ?Sized> Drop for OwnedRwLockWriteGuard<T, U> {
    fn drop(&mut self) {
        if std::thread::panicking() { return; }
        let tid = self.lock.backing_tid;
        crate::send_tagged_msg(
            tid,
            RELEASE_WRITE_TAG,
            Request::ReleaseWrite(crate::thread::current().id()),
        );
        let _: Response = crate::recv_tagged_msg_block(move |t, _| t == tid);
        //TODO: Do we need ManuallyDrop?
        // unsafe { ManuallyDrop::drop(&mut self.lock) };
    }
}
// The extra types (messages, message loop, etc.) required to emulate the Mutex
// with Message passing for Must follows:

const ACQUIRE_READ_TAG: u32 = 1;
const ACQUIRE_WRITE_TAG: u32 = 2;
const RELEASE_READ_TAG: u32 = 3;
const RELEASE_WRITE_TAG: u32 = 4;

#[derive(Debug, Clone, PartialEq)]
enum Request {
    AcquireRead(crate::thread::ThreadId),
    AcquireWrite(crate::thread::ThreadId),
    ReleaseRead(crate::thread::ThreadId),
    ReleaseWrite(crate::thread::ThreadId),
}

#[derive(Debug, Clone, PartialEq)]
enum Response {
    AcquireRead,
    AcquireWrite,
    ReleaseRead,
    ReleaseWrite,
}

struct MustRwLock {
    max_readers: usize,
    current_readers: usize,
}

impl MustRwLock {
    pub fn new(max_readers: usize) -> Self {
        Self {
            max_readers,
            current_readers: 0,
        }
    }

    pub fn run(&mut self) {
        loop {
            // The mutex starts out unlocked. Only read LOCK_TAG messages.
            let req: Request = crate::recv_tagged_msg_block(|_, tag| {
                tag == Some(ACQUIRE_READ_TAG) || tag == Some(ACQUIRE_WRITE_TAG)
            });

            match req {
                Request::AcquireWrite(locking_tid) => {
                    crate::send_msg(locking_tid, Response::AcquireWrite);

                    // This case is easy--the only kind of request we can accept now is to unlock the request
                    // from the correct thread.
                    let req: Request = crate::recv_tagged_msg_block(move |tid, tag| {
                        tid == locking_tid && tag == Some(RELEASE_WRITE_TAG)
                    });
                    match req {
                        Request::ReleaseWrite(tid) => {
                            assert!(locking_tid == tid);
                            crate::send_msg(tid, Response::ReleaseWrite);
                        }
                        _ => unreachable!(),
                    }
                }
                Request::AcquireRead(tid) => {
                    self.reader_lock_loop(tid);
                }
                m => panic!("Logic error; unexpected message {:?}", m),
            }
        }
    }

    fn reader_lock_loop(&mut self, tid: crate::thread::ThreadId) {
        crate::send_msg(tid, Response::AcquireRead);

        let mut reader_tids = HashSet::new();
        reader_tids.insert(tid);
        self.current_readers += 1;

        loop {
            // Now in this case, we can accept either more read lock requests, or any unlock requests.
            let reader_tids_clone = reader_tids.clone();
            let req: Request = if self.current_readers == self.max_readers {
                // only accept release tags if we already have the max number of readers
                crate::recv_tagged_msg_block(move |tid, tag| {
                    reader_tids_clone.contains(&tid) && tag == Some(RELEASE_READ_TAG)
                })
            } else {
                crate::recv_tagged_msg_block(move |tid, tag| {
                    if reader_tids_clone.contains(&tid) {
                        tag == Some(RELEASE_READ_TAG) // Only unlock requests are allowed from an existing reader.
                    } else {
                        tag == Some(ACQUIRE_READ_TAG) // An only READ lock tags are allowed from anybody else.
                    }
                })
            };
            match req {
                Request::AcquireRead(thread_id) => {
                    reader_tids.insert(thread_id);
                    self.current_readers += 1;
                    crate::send_msg(thread_id, Response::AcquireRead);
                }
                Request::ReleaseRead(thread_id) => {
                    reader_tids.remove(&thread_id);
                    self.current_readers -= 1;
                    crate::send_msg(thread_id, Response::ReleaseRead);
                }
                m => panic!("Logic error; unexpected message {:?}", m),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{cover, Config};

    use crate::sync::RwLock;

    #[test]
    fn test_mutex() {
        crate::verify(Config::default(), || {
            let mutex = Arc::new(RwLock::new(1));

            let mutex2 = mutex.clone();
            let f1 = crate::future::spawn(async move {
                let mut val = mutex2.write().await;
                *val += 1;
            });

            crate::future::block_on(f1).unwrap();
            let ending_val = crate::future::block_on(async move {
                let val = mutex.read().await;
                *val
            });

            assert_eq!(ending_val, 2);
        });
    }

    #[test]
    fn test_mutex_race() {
        let stats = crate::verify(Config::default(), || {
            let mutex = Arc::new(RwLock::new(1));

            let mutex2 = mutex.clone();
            let f1 = crate::future::spawn(async move {
                let mut val = mutex2.write().await;
                *val += 1;
            });

            let mutex2 = mutex.clone();
            let f2 = crate::future::spawn(async move {
                let mut val = mutex2.write().await;
                *val *= 10;
            });

            crate::future::block_on(f1).unwrap();
            crate::future::block_on(f2).unwrap();

            let ending_val = crate::future::block_on(async move {
                let val = mutex.read().await;
                *val
            });

            cover!("20", ending_val == 20); // (1 + 1) * 10
            cover!("11", ending_val == 11); // 1 + (1 * 10)
        });
        assert_eq!((2, 0), (stats.execs, stats.block));
        assert!(stats.coverage.is_covered("20".into()));
        assert!(stats.coverage.is_covered("11".into()));
    }

    // This test shows why we should wait to get back a response after releasing a lock
    // if we do not wait, the backing thread may receive the release requests in two different orders
    // even though the drop of mutex2 should finish before the join
    #[test]
    fn test_multiple_readers() {
        let stats = crate::verify(Config::builder().with_verbose(2).build(), || {
            crate::future::block_on(async {
                let mutex = Arc::new(RwLock::new(1));

                let guard = mutex.read().await;

                // This future will acquire a read lock that overlaps with the main thread's guard
                let mutex2 = mutex.clone();
                let f1 = crate::future::spawn(async move {
                    mutex2.read().await;
                });

                f1.await.unwrap();

                drop(guard);
                cover!("FINISHED", true);
            });
        });

        assert_eq!((1, 0), (stats.execs, stats.block));
        assert_eq!(
            stats.coverage.covered("FINISHED".into()),
            stats.execs as u64
        );
    }
}
