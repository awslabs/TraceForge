//! TraceForge's implementation of [`tokio::sync::oneshot`].

use crate::*;

use futures::task::Context;
use futures::task::Poll;
use std::pin::Pin;
//use futures::task::Poll::Ready;

pub type Receiver<T> = crate::channel::Receiver<T>;

#[derive(Clone, PartialEq)]
pub struct Sender<T> {
    sender: crate::channel::Sender<T>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Inside the pretty printer: only the channel end, e.g. `Receiver(c2.2)`.
        if crate::trace_print::names::active() {
            return std::fmt::Debug::fmt(&self.sender, f);
        }
        f.debug_struct("Sender")
            .field("sender", &self.sender)
            .finish()
    }
}

unsafe impl<T: Send> Send for Sender<T> {}

unsafe impl<T: Sync> Sync for Sender<T> {}

impl<T: Message + 'static> Sender<T> {
    fn new(sender: crate::channel::Sender<T>) -> Self {
        Sender { sender }
    }

    #[allow(unused_mut)]
    pub fn send(mut self, v: T) -> Result<(), T> {
        self.sender.send_msg(v);
        Ok(())
    }
}

// The current version ignores the buffer size
pub fn channel<T>() -> (Sender<T>, Receiver<T>)
where
    T: Clone + std::fmt::Debug + PartialEq + Message + 'static,
{
    let (tx, rx) = crate::channel::Builder::<T>::new().build();
    (Sender::new(tx), rx)
}

impl<T: Message + Clone + 'static> Future for Receiver<T> {
    type Output = Result<T, error::RecvError>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Ok(self.recv_msg_block()))
    }
}

pub mod error {
    //! `Oneshot` error types.

    use std::fmt;

    /// Error returned by the `Future` implementation for `Receiver`.
    ///
    /// This error is returned by the receiver when the sender is dropped without sending.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub struct RecvError(pub(super) ());

    /// Error returned by the `try_recv` function on `Receiver`.
    #[derive(Debug, Eq, PartialEq, Clone)]
    pub enum TryRecvError {
        /// The send half of the channel has not yet sent a value.
        Empty,

        /// The send half of the channel was dropped without sending a value.
        Closed,
    }

    // ===== impl RecvError =====

    impl fmt::Display for RecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(fmt, "channel closed")
        }
    }

    impl std::error::Error for RecvError {}

    // ===== impl TryRecvError =====

    impl fmt::Display for TryRecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                TryRecvError::Empty => write!(fmt, "channel empty"),
                TryRecvError::Closed => write!(fmt, "channel closed"),
            }
        }
    }

    impl std::error::Error for TryRecvError {}
}
