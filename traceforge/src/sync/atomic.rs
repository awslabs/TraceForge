//! TraceForge's implementation of [`tokio::sync::atomic`].

use crate::channel::*;
use crate::thread::*;
use crate::*;
use std::fmt::Debug;
use std::sync::atomic::Ordering;

pub type AtomicBool = AtomicRegister<bool>;
pub type AtomicI8 = AtomicRegister<i8>;
pub type AtomicI16 = AtomicRegister<i16>;
pub type AtomicI32 = AtomicRegister<i32>;
pub type AtomicI64 = AtomicRegister<i64>;
pub type AtomicIsize = AtomicRegister<isize>;
pub type AtomicU8 = AtomicRegister<u8>;
pub type AtomicU16 = AtomicRegister<u16>;
pub type AtomicU32 = AtomicRegister<u32>;
pub type AtomicU64 = AtomicRegister<u64>;
pub type AtomicUsize = AtomicRegister<usize>;

#[derive(Clone, Debug, PartialEq)]
pub enum MsgRequest<T: Clone + Debug + PartialEq + std::marker::Send> {
    Read(Sender<MsgResponse<T>>),
    Write(T, Sender<MsgResponse<T>>),
    CAS(T, T, Sender<MsgResponse<T>>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum MsgResponse<T: Clone + Debug + PartialEq + std::marker::Send> {
    ReadResponse(T),
    WriteResponse,
    CASResponse(bool),
}

pub struct Synchronizer<T: Clone + Debug + PartialEq + std::marker::Send> {
    value: T,
}

impl<T: Clone + Debug + PartialEq + std::marker::Send + 'static> Synchronizer<T> {
    pub fn execute(&mut self) {
        loop {
            match recv_msg_block() {
                MsgRequest::Read(tx) => {
                    tx.send_msg(MsgResponse::ReadResponse(self.value.clone()));
                }
                MsgRequest::Write(new, tx) => {
                    self.value = new;
                    tx.send_msg(MsgResponse::WriteResponse);
                }
                MsgRequest::CAS(current, new, tx) => {
                    let success = self.value == current;
                    if success {
                        self.value = new;
                    }
                    tx.send_msg(MsgResponse::CASResponse(success));
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct AtomicRegister<T: Clone + Debug + PartialEq + std::marker::Send + 'static> {
    synchronizer: Arc<ThreadId>,
    _t: Arc<std::marker::PhantomData<T>>,
}

unsafe impl<T: Clone + Debug + PartialEq + std::marker::Send + 'static> Send for AtomicRegister<T> {}

unsafe impl<T: Clone + Debug + PartialEq + std::marker::Send + 'static> Sync for AtomicRegister<T> {}

impl<T: Clone + Debug + PartialEq + std::marker::Send + 'static> AtomicRegister<T> {
    pub fn new(value: T) -> Self {
        let mut tsync = Synchronizer::<T> { value };

        let tsync_handle = thread::Builder::new()
            .name("traceforge_runtime::atomic_register".to_string())
            .spawn_daemon(move || {
                tsync.execute();
            })
            .unwrap();

        AtomicRegister {
            synchronizer: Arc::new(tsync_handle.thread().id()),
            _t: Arc::new(std::marker::PhantomData),
        }
    }

    pub fn load(&self, order: Ordering) -> T {
        if order != Ordering::SeqCst {
            panic!("Load accesses to AtomicRegister are only implemented for SeqCst")
        }
        let chan = channel::Builder::<MsgResponse<T>>::new().build();
        send_msg(*self.synchronizer, MsgRequest::Read(chan.0));
        match chan.1.recv_msg_block() {
            MsgResponse::ReadResponse(x) => x,
            MsgResponse::WriteResponse => panic!("Error in the implementation of AtomicRegister"),
            MsgResponse::CASResponse(_) => panic!("Error in the implementation of AtomicRegister"),
        }
    }

    pub fn store(&self, new: T, order: Ordering) {
        if order != Ordering::SeqCst {
            panic!("Store accesses to AtomicRegister are only implemented for SeqCst")
        }
        let chan = channel::Builder::<MsgResponse<T>>::new().build();
        send_msg(*self.synchronizer, MsgRequest::Write(new, chan.0));
        match chan.1.recv_msg_block() {
            MsgResponse::ReadResponse(_) => panic!("Error in the implementation of AtomicRegister"),
            MsgResponse::WriteResponse => (),
            MsgResponse::CASResponse(_) => panic!("Error in the implementation of AtomicRegister"),
        }
    }

    // This is an incomplete implementation. It only handles Sequential Consistency
    pub fn compare_exchange(
        &self,
        current: T,
        new: T,
        _success: Ordering,
        _failure: Ordering,
    ) -> Result<T, T> {
        info!("This is an incomplete implementation. It only handles Sequential Consistency");
        let chan = channel::Builder::<MsgResponse<T>>::new().build();
        send_msg(
            *self.synchronizer,
            MsgRequest::CAS(current.clone(), new.clone(), chan.0),
        );
        match chan.1.recv_msg_block() {
            MsgResponse::ReadResponse(_) => panic!("Error in the implementation of AtomicRegister"),
            MsgResponse::WriteResponse => panic!("Error in the implementation of AtomicRegister"),
            MsgResponse::CASResponse(x) => {
                if x {
                    Ok(current)
                } else {
                    Err(current)
                }
            }
        }
    }
}
