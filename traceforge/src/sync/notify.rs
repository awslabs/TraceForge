use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{future::Future, task::Poll};

use crate::sync::atomic::AtomicUsize;
use crate::sync::oneshot::{self, Receiver};
use crate::sync::Mutex;

#[derive(Clone, Debug)]
pub struct Notify {
    state: AtomicUsize,
    waiters: Arc<Mutex<Vec<oneshot::Sender<bool>>>>,
}

#[derive(Debug)]
pub struct Notified<'a> {
    notify: &'a Notify,
    receiver: Option<Receiver<bool>>,
}

impl Notify {
    pub fn new() -> Notify {
        return Notify {
            state: AtomicUsize::new(0),
            waiters: Arc::new(Mutex::new(Vec::new())),
        };
    }

    pub const fn const_new() -> Notify {
        unimplemented!()
    }

    pub fn notified(&self) -> Notified<'_> {
        Notified {
            notify: self,
            receiver: None,
        }
    }

    pub fn notify_one(&self) {
        let mut waiters = self.waiters.blocking_lock();
        if waiters.len() > 0 {
            // there is a waiter, notify them by writing to their channel
            let ch = waiters.pop().unwrap();
            let _ = ch.send(true);
        } else {
            // mark that a notify has been sent for the next notified() call
            self.state.store(1, Ordering::SeqCst);
        }
    }

    pub fn notify_last(&self) {
        unimplemented!()
    }

    pub fn notify_waiters(&self) {
        unimplemented!()
    }
}

impl Default for Notify {
    fn default() -> Self {
        Notify::new()
    }
}

unsafe impl Send for Notify {}
unsafe impl Sync for Notify {}
impl Unpin for Notify {}

unsafe impl<'a> Send for Notified<'a> {}
unsafe impl<'a> Sync for Notified<'a> {}

impl<'a> Notified<'a> {
    fn poll_notified(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        // First check if there's already a notification available
        let cas = self
            .notify
            .state
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst);
        if cas.is_ok() {
            return Poll::Ready(());
        }

        // If we don't have a receiver yet, create one and register it
        if self.receiver.is_none() {
            let (tx, rx) = oneshot::channel::<bool>();
            let mut waiters = self.notify.waiters.blocking_lock();
            waiters.push(tx);
            self.receiver = Some(rx);
        }

        // Poll the receiver
        if let Some(ref mut receiver) = self.receiver {
            match Pin::new(receiver).poll(cx) {
                Poll::Ready(_) => Poll::Ready(()),
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

impl Future for Notified<'_> {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.poll_notified(cx)
    }
}
