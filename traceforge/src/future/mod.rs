//! Shuttle's implementation of an async executor, roughly equivalent to [`futures::executor`].
//!
//! The [spawn] method spawns a new asynchronous task that the executor will run to completion. The
//! [block_on] method blocks the current thread on the completion of a future.
//!
//! Copied over to the Must runtime to allow handling of async calls
//!
//! [`futures::executor`]: https://docs.rs/futures/0.3.30/futures/executor/index.html

use crate::channel::{from_receiver, Builder, Receiver, Sender};
use crate::loc::WakeMsg;
use crate::msg::Message;
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread::{self, switch};
use crate::thread::Thread;
use crate::CommunicationModel::LocalOrder;
use crate::TJoin;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::result::Result;
use std::task::{Context, Poll, Waker};

// Not really unsafe, we're not doing any concurrency.
// This is needed for `Waker::from`
// unsafe impl Sync for Sender<()> {}

// The value is irrelevant, we're using a Channel<()> as a waker.
impl std::task::Wake for Sender<WakeMsg> {
    fn wake(self: std::sync::Arc<Self>) {
        self.send_msg(WakeMsg);
    }
}

fn get_bidir_handles() -> (TwoWayCom, TwoWayCom) {
    let (sender1, receiver1) = Builder::new().with_comm(LocalOrder).build();
    let (sender2, receiver2) = Builder::new().with_comm(LocalOrder).build();
    // *flip* them
    (
        TwoWayCom {
            sender: sender1,
            receiver: receiver2,
        },
        TwoWayCom {
            sender: sender2,
            receiver: receiver1,
        },
    )
}

/// Spawn a new async task that the executor will run to completion.
pub fn spawn<T, F>(fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Message + 'static,
{
    spawn_with_attributes::<T, F>(false, None, fut)
}

/// Spawn a new async task that the executor will run to completion.
pub fn spawn_with_attributes<T, F>(is_daemon: bool, name: Option<String>, fut: F) -> JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Message + 'static,
{
    thread::switch();

    let stack_size = ExecutionState::with(|s| s.must.borrow().config.stack_size);
    let (fut_handles, join_handles) = get_bidir_handles();

    let task_id = ExecutionState::spawn_thread(
        move || {
            let (sender, fut_recv) = Builder::<WakeMsg>::new().build();
            let fut_waker = Waker::from(std::sync::Arc::new(sender.clone()));

            // Poll once in advance:
            // tokio's spawn semantics: the future will start running immediately.
            let mut fut = Box::pin(fut);
            let mut res = fut.as_mut().poll(&mut Context::from_waker(&fut_waker));
            let mut join_waker: Option<Waker> = None;
            let res = loop {
                match res {
                    // We're done, call the waker
                    Poll::Ready(res) => {
                        if let Some(waker) = join_waker {
                            waker.wake();
                        }
                        break Some(res);
                    }
                    Poll::Pending => { /* keep going */ }
                }

                // Wait for either the joiner or the future, to poll or inform us, respectively
                let (msg, ind) = crate::select_val_block(&fut_handles.receiver, &fut_recv);

                // Joiner polled us, inform them it's pending
                if ind == 0 {
                    match msg.as_any().downcast::<PollerMsg>() {
                        Ok(waker) => match *waker {
                            PollerMsg::Waker(waker) => {
                                assert!(ind == 0);
                                join_waker = Some(waker.clone());
                                fut_handles.sender.send_msg(PollerMsg::Pending);
                            }
                            PollerMsg::Cancel => break None,
                            _ => unreachable!(),
                        },
                        _ => unreachable!(),
                    }
                } else {
                    // Futured informed us to poll again
                    assert!(ind == 1);
                    assert!(msg.as_any().downcast::<WakeMsg>().is_ok());
                    res = fut.as_mut().poll(&mut Context::from_waker(&fut_waker));
                }
            };

            let val = match res {
                // We consumed the future to completion
                Some(result) => {
                    // Wait for a final request
                    match fut_handles.receiver.recv_msg_block() {
                        PollerMsg::Waker(_) => {
                            // Inform them it's ready, they can try to Join
                            fut_handles.sender.send_msg(PollerMsg::Ready);
                            crate::Val::new(result)
                        }
                        PollerMsg::Cancel => {
                            // Explicitly drop the future here to cancel it
                            drop(fut);
                            crate::Val::new(())
                        }
                        _ => unreachable!(),
                    }
                }
                // We were cancelled
                None => {
                    // Explicitly drop the future here as well
                    // (it may have cancellation code to run?)
                    drop(fut);
                    crate::Val::new(())
                }
            };

            // Final Message, useful for impl of Drop on JoinHandle
            fut_handles.sender.send_msg(PollerMsg::Done);

            // Properly End the thread
            ExecutionState::with(|state| {
                let pos = state.next_pos();
                state
                    .must
                    .borrow_mut()
                    .handle_tend(crate::End::new(pos, val));
                crate::must::Must::unstuck_joiners(state, pos.thread);
            });
        },
        stack_size,
        None,
    );

    let (thread_id, name) = ExecutionState::with(|state| {
        let pos = state.next_pos();
        let tid = state.must.borrow().next_thread_id(&pos);
        let name = match name {
            None => format!("<future-{}>", tid.to_number()),
            Some(x) => x,
        };
        //let name = format!("<future-{}>", tid.to_number());
        state.must.borrow_mut().handle_tcreate(
            tid,
            task_id,
            None, /* asyncs do not have symmetric versions for symm reduction */
            pos,
            Some(name.clone()),
            is_daemon,
        );
        (tid, Some(name))
    });

    let thread = Thread {
        id: thread_id,
        name,
    };

    thread::switch();

    JoinHandle {
        task_id,
        thread,
        com: join_handles,
        _p: std::marker::PhantomData,
    }
}

pub(crate) fn spawn_receive<T>(recv: &Receiver<T>) -> JoinHandle<T>
where
    T: Message + Clone + 'static,
{
    thread::switch();

    let stack_size = ExecutionState::with(|s| s.must.borrow().config.stack_size);
    let (fut_handles, join_handles) = get_bidir_handles();

    let recv = recv.clone();
    let task_id = ExecutionState::spawn_thread(
        move || {
            let mut join_waker: Option<Waker> = None;
            let res = loop {
                // Wait for either the joiner to poll us, or the receive to succeed.
                let (msg, ind) = crate::select_val_block(&fut_handles.receiver, &recv);

                // TODO: Use `cast!` to avoid all the `unreachable!` mess.
                // Joiner polled us
                if ind == 0 {
                    match msg.as_any().downcast::<PollerMsg>() {
                        Ok(msg) => {
                            match *msg {
                                PollerMsg::Waker(waker) => {
                                    // Save the waker and inform them it's Pending
                                    join_waker = Some(waker.clone());
                                    fut_handles.sender.send_msg(PollerMsg::Pending);
                                }
                                // We're cancelled, without having consumed anything
                                PollerMsg::Cancel => break None,
                                _ => unreachable!(),
                            }
                        }
                        _ => unreachable!(),
                    }
                } else {
                    // We did the receive, call the waker.
                    assert!(ind == 1);
                    match msg.as_any().downcast::<T>() {
                        Ok(result) => {
                            if let Some(waker) = join_waker {
                                waker.wake();
                            }
                            // We consumed the message
                            break Some(*result);
                        }
                        _ => unreachable!(),
                    }
                }
            };

            // Select is done, either wait for the request or cancel the receive
            let val = match res {
                // We consumed the message
                Some(result) => {
                    // Wait once more for the poller
                    match fut_handles.receiver.recv_msg_block() {
                        // Inform them it's ready, they can try to Join
                        PollerMsg::Waker(_) => {
                            fut_handles.sender.send_msg(PollerMsg::Ready);
                            crate::Val::new(result)
                        }
                        // Cancelled, let's put the message back
                        PollerMsg::Cancel => {
                            from_receiver(recv).send_msg(result);
                            crate::Val::new(())
                        }
                        _ => unreachable!(),
                    }
                }
                // We got cancelled without consuming the message: nothing to do
                None => crate::Val::new(()),
            };

            // Final Message, useful for impl of Drop on JoinHandle
            fut_handles.sender.send_msg(PollerMsg::Done);

            // Properly End the thread
            ExecutionState::with(|state| {
                let pos = state.next_pos();
                state
                    .must
                    .borrow_mut()
                    .handle_tend(crate::End::new(pos, val));
                crate::must::Must::unstuck_joiners(state, pos.thread);
            });
        },
        stack_size,
        None,
    );

    let (thread_id, name) = ExecutionState::with(|state| {
        let pos = state.next_pos();
        let tid = state.must.borrow().next_thread_id(&pos);
        let name = format!("<async_recv-{}>", tid.to_number());
        state.must.borrow_mut().handle_tcreate(
            tid,
            task_id,
            None, /* asyncs do not have symmetric versions for symm reduction */
            pos,
            Some(name.clone()),
            false, /* asyncs are not daemon threads */
        );
        (tid, Some(name))
    });

    let thread = Thread {
        id: thread_id,
        name,
    };

    thread::switch();

    JoinHandle {
        task_id,
        thread,
        com: join_handles,
        _p: std::marker::PhantomData,
    }
}

/// An owned permission to join on an async task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    task_id: TaskId,
    thread: Thread,
    com: TwoWayCom,
    _p: std::marker::PhantomData<T>,
}

#[derive(Clone, Debug)]
pub enum PollerMsg {
    Waker(Waker),
    Pending,
    Cancel,
    Done,
    Ready,
}

// PollerMsg must satisfy Message in order to be sent around with channels.
// Message includes PartialEq, which the foreign type Waker does not implement.
// This implementation acts as if all Wakers are equal.
// The PartialEq on Message is (only) used for validating during replay,
// which is needed for the nondeterminism detector.
impl PartialEq for PollerMsg {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // Wakers are equal
            (PollerMsg::Waker(_), PollerMsg::Waker(_)) => true,
            (PollerMsg::Pending, PollerMsg::Pending) => true,
            (PollerMsg::Cancel, PollerMsg::Cancel) => true,
            (PollerMsg::Ready, PollerMsg::Ready) => true,
            (PollerMsg::Done, PollerMsg::Done) => true,
            _ => false,
        }
    }
}

// Helper for two-way communication between JoinHandle and Poller
#[derive(Clone, Debug)]
pub struct TwoWayCom {
    pub sender: Sender<PollerMsg>,
    pub receiver: Receiver<PollerMsg>,
}

impl<T> JoinHandle<T> {
    /// Returns `true` if this task is finished, otherwise returns `false`.
    ///
    /// ## Panics
    /// Panics if called outside of shuttle context, i.e. if there is no execution context.
    pub fn is_finished(&self) -> bool {
        ExecutionState::with(|state| {
            let task = state.get(self.task_id);
            task.finished()
        })
    }

    /// Extracts a handle to the underlying thread.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

// TODO: need to work out all the error cases here
/// Task failed to execute to completion.
#[derive(Debug)]
pub enum JoinError {
    /// Task was aborted
    Cancelled,
}

impl Display for JoinError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Cancelled => write!(f, "task was cancelled"),
        }
    }
}

impl Error for JoinError {}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        // If a Join Handle for a spawned task is never awaited, one could abort the task by calling `self.abort()`
        // But this may mean that certain side effects (message sends or receives) of the task
        // do not get to run.
        // Must currently does not perform a backtrack on aborted tasks. So it is conservative to
        // not abort tasks that are not awaited.

        // FIXME: Is the above comment relevant?

        // Tricky: it could be that the handle was dropped because there was nothing else to do,
        // i.e. there is no ScheduledTask.
        // In that case, we cannot send a message (it would panic).
        // We only lose the case where:
        // there is someone that was waiting to read from a receive that was
        // consumed through the underlying future but was not used,
        // and had we actually cancelled the future this would now be able to run.
        // If there are no concurrent receives, this shouldn't happen.
        // TODO: Detect and handle this scenario?
        if ExecutionState::with(|state| state.is_running()) {
            self.com.sender.send_msg(PollerMsg::Cancel);
            // We wait for the Future to actually finish,
            // whether it was actually cancelled or not.
            // This is necessary so that we "synchronize" in a porf-sense
            // and the Future's receives are no longer concurrent.
            let ack = self.com.receiver.recv_msg_block();
            assert!(matches!(ack, PollerMsg::Done));
        }
    }
}

impl<T: Message + 'static> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Ask poller
        self.com
            .sender
            .send_msg(PollerMsg::Waker(cx.waker().clone()));
        match self.com.receiver.recv_msg_block() {
            PollerMsg::Ready => {
                loop {
                    switch();
                    let val = ExecutionState::with(|s| {
                        let target_task_id = s.get(self.task_id).id();
                        let target_id = s.must.borrow().to_thread_id(target_task_id);
                        let pos = s.next_pos();
                        s.must.borrow_mut().handle_tjoin(TJoin::new(pos, target_id))
                    });

                    // Wait for the thread to *actually* finish
                    if let Some(val) = val {
                        if val.is_pending() {
                            ExecutionState::with(|s| s.current_mut().stuck());
                        } else {
                            return Poll::Ready(Ok(*val.as_any().downcast().unwrap()));
                        }
                    }

                    ExecutionState::with(|s| s.prev_pos());
                }
            }
            PollerMsg::Pending => Poll::Pending,
            _ => unreachable!(),
        }
    }
}

/// Run a future to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let (sender, receiver) = Builder::<WakeMsg>::new().build();
    let waker = Waker::from(std::sync::Arc::new(sender.clone()));
    let cx = &mut Context::from_waker(&waker);

    thread::switch();

    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                break result;
            }
            Poll::Pending => {
                receiver.recv_msg_block();
            }
        }

        thread::switch();
    }
}

#[cfg(test)]
mod test {
    use crate::{recv_msg_block, send_msg, thread, verify, Config};

    use super::block_on;

    #[test]
    fn test_thread() {
        verify(Config::builder().build(), || {
            let parent_id = thread::current().id();

            let fut = crate::future::spawn(async move {
                let i: i32 = recv_msg_block();
                send_msg(parent_id, i); // Echo back the same value.
                3 // return 3.
            });

            let fut_tid = fut.thread().id();
            println!("Future's thread id is {}", fut.thread().id());

            send_msg(fut_tid, 4);
            let echoed: i32 = recv_msg_block();
            assert_eq!(echoed, 4);

            let res = block_on(fut);
            println!("Retrieved {:?} from future", &res);
            assert_eq!(res.unwrap(), 3);
        });
    }
}
