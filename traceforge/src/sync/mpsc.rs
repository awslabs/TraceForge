//! TraceForge's implementation of [`tokio::sync::mpsc`].
use std::task::{Context, Poll};

use crate::{sync::mpsc::error::TrySendError, *};

// The current version ignores the buffer size
pub fn channel<T>(_buffer: usize) -> (Sender<T>, Receiver<T>)
where
    T: Clone + std::fmt::Debug + PartialEq + Message + 'static,
{
    info!("This is an incomplete implementation. We ingore the buffer size");
    let (tx, rx) = crate::channel::Builder::<T>::new().build();
    (Sender::new(tx), Receiver::new(rx))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sender<T> {
    sender: crate::channel::Sender<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}

unsafe impl<T: Sync> Sync for Sender<T> {}

impl<T: Message + 'static> Sender<T> {
    fn new(sender: crate::channel::Sender<T>) -> Self {
        Sender { sender }
    }
    pub async fn send(&self, v: T) -> Result<(), error::SendError<T>> {
        self.sender.send_msg(v);
        Ok(())
    }
    pub fn try_send(&self, v: T) -> Result<(), TrySendError<T>> {
        if nondet() {
            self.sender.send_msg(v);
            Ok(())
        } else {
            Err(TrySendError::Full(v))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Receiver<T> {
    receiver: crate::channel::Receiver<T>,
}

unsafe impl<T: Send> Send for Receiver<T> {}

unsafe impl<T: Sync> Sync for Receiver<T> {}

impl<T: Message + Clone + 'static> Receiver<T> {
    fn new(receiver: crate::channel::Receiver<T>) -> Self {
        Receiver { receiver }
    }

    // This is incomplete as it does not model receive errors.
    // A complete implementation would non-deterministically return None.
    pub fn recv(&self) -> Option<T> {
        info!("This is an incomplete implementation. It never returns None");
        Some(self.receiver.recv_msg_block())
    }

    // This is incomplete as it does not model receive errors.
    // A complete implementation would non-deterministically return an error.
    pub fn try_recv(&self) -> Result<T, error::TryRecvError> {
        info!("This is an incomplete implementation. It never returns errors");
        Ok(self.receiver.recv_msg_block())
    }

    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        return Poll::Ready(self.recv());
    }
}

pub mod error {
    //! Channel error types.

    use std::error::Error;
    use std::fmt;

    /// Error returned by the `Sender`.
    #[derive(PartialEq, Eq, Clone, Copy)]
    pub struct SendError<T>(pub T);

    impl<T> fmt::Debug for SendError<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("SendError").finish_non_exhaustive()
        }
    }

    impl<T> fmt::Display for SendError<T> {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(fmt, "channel closed")
        }
    }

    impl<T> Error for SendError<T> {}

    // ===== TrySendError =====

    /// This enumeration is the list of the possible error outcomes for the
    /// [`try_send`](super::Sender::try_send) method.
    #[derive(PartialEq, Eq, Clone, Copy)]
    pub enum TrySendError<T> {
        /// The data could not be sent on the channel because the channel is
        /// currently full and sending would require blocking.
        Full(T),

        /// The receive half of the channel was explicitly closed or has been
        /// dropped.
        Closed(T),
    }

    impl<T> TrySendError<T> {
        /// Consume the `TrySendError`, returning the unsent value.
        pub fn into_inner(self) -> T {
            match self {
                TrySendError::Full(val) => val,
                TrySendError::Closed(val) => val,
            }
        }
    }

    impl<T> fmt::Debug for TrySendError<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                TrySendError::Full(..) => "Full(..)".fmt(f),
                TrySendError::Closed(..) => "Closed(..)".fmt(f),
            }
        }
    }

    impl<T> fmt::Display for TrySendError<T> {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                fmt,
                "{}",
                match self {
                    TrySendError::Full(..) => "no available capacity",
                    TrySendError::Closed(..) => "channel closed",
                }
            )
        }
    }

    impl<T> Error for TrySendError<T> {}

    impl<T> From<SendError<T>> for TrySendError<T> {
        fn from(src: SendError<T>) -> TrySendError<T> {
            TrySendError::Closed(src.0)
        }
    }

    // ===== TryRecvError =====

    /// Error returned by `try_recv`.
    #[derive(PartialEq, Eq, Clone, Copy, Debug)]
    pub enum TryRecvError {
        /// This **channel** is currently empty, but the **Sender**(s) have not yet
        /// disconnected, so data may yet become available.
        Empty,
        /// The **channel**'s sending half has become disconnected, and there will
        /// never be any more data received on it.
        Disconnected,
    }

    impl fmt::Display for TryRecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            match *self {
                TryRecvError::Empty => "receiving on an empty channel".fmt(fmt),
                TryRecvError::Disconnected => "receiving on a closed channel".fmt(fmt),
            }
        }
    }

    impl Error for TryRecvError {}

    // ===== RecvError =====

    /// Error returned by `Receiver`.
    #[derive(Debug, Clone)]
    #[doc(hidden)]
    #[deprecated(note = "This type is unused because recv returns an Option.")]
    pub struct RecvError(());

    #[allow(deprecated)]
    impl fmt::Display for RecvError {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(fmt, "channel closed")
        }
    }

    #[allow(deprecated)]
    impl Error for RecvError {}
}
