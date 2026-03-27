use std::{future::Future, iter};

use serde::{Deserialize, Serialize};

use crate::{
    async_recv_msg, get_execution_state_info,
    identifier::Identifier,
    loc::{CommunicationModel, Loc},
    msg::Message,
    runtime::execution::ExecutionState,
    thread::ThreadId,
    ConsType, Unique,
};

use crate::predicate::{normalize_vec_tag, PredicateType};
use std::fmt::Error;
use std::sync::Arc;

type Result<T> = std::result::Result<T, Error>;

#[allow(deprecated)]
pub(crate) fn cons_to_model(c: ConsType) -> CommunicationModel {
    match c {
        ConsType::Bag => CommunicationModel::NoOrder,
        ConsType::WB | ConsType::FIFO => CommunicationModel::LocalOrder,
        ConsType::CD | ConsType::Causal => CommunicationModel::CausalOrder,
        ConsType::MO | ConsType::Mailbox => CommunicationModel::TotalOrder,
    }
}

pub struct Builder<T> {
    named_id: Option<Loc>,
    comm: Option<CommunicationModel>,
    _t: std::marker::PhantomData<T>,
}

impl<T: Message + Clone + 'static> Builder<T> {
    pub fn new() -> Self {
        Builder {
            named_id: None,
            comm: None,
            _t: std::marker::PhantomData,
        }
    }

    pub fn with_name<I: Identifier>(mut self, id: I) -> Self {
        self.named_id = Some(Loc::new(id));
        self
    }

    pub fn with_comm(mut self, comm: CommunicationModel) -> Self {
        self.comm = Some(comm);
        self
    }

    pub fn build(self) -> Channel<T> {
        let (inner, comm) = if let Some(name) = self.named_id {
            let comm = self.comm.unwrap_or_else(|| get_execution_state_info().1);
            (Loc::new(name), comm)
        } else {
            ExecutionState::with(|s| {
                let pos = s.next_pos();
                let comm = self
                    .comm
                    .unwrap_or_else(|| cons_to_model(s.must.borrow().config.cons_type));
                let channel = Unique::new(pos, comm);
                (s.must.borrow_mut().handle_unique(channel), comm)
            })
        };
        (Sender::new(inner.clone(), comm), Receiver::new(inner, comm))
    }
}

impl<T: Message + Clone + 'static> Default for Builder<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sender<T> {
    inner: Loc,
    comm: CommunicationModel,
    _t: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for Sender<T> {}

unsafe impl<T: Sync> Sync for Sender<T> {}

impl<T: Message + 'static> Sender<T> {
    fn new(inner: Loc, comm: CommunicationModel) -> Self {
        Sender {
            inner,
            comm,
            _t: std::marker::PhantomData,
        }
    }

    pub fn send_tagged_msg(&self, tag: u32, v: T) {
        crate::send_msg_with_tag(v, Some(tag), &self.inner, self.comm, false)
    }

    pub fn send_tagged_lossy_msg(&self, tag: u32, v: T) {
        crate::send_msg_with_tag(v, Some(tag), &self.inner, self.comm, true)
    }

    pub fn send_vec_tagged_msg(&self, tag: Vec<u32>, v: T) {
        let tag = if tag.is_empty() { None } else { Some(tag) };
        crate::send_msg_with_vec_tag(v, tag, &self.inner, self.comm, false)
    }

    pub fn send_vec_tagged_lossy_msg(&self, tag: Vec<u32>, v: T) {
        let tag = if tag.is_empty() { None } else { Some(tag) };
        crate::send_msg_with_vec_tag(v, tag, &self.inner, self.comm, true)
    }

    pub fn send_msg(&self, v: T) {
        crate::send_msg_with_tag(v, None, &self.inner, self.comm, false);
    }

    pub fn send_lossy_msg(&self, v: T) {
        crate::send_msg_with_tag(v, None, &self.inner, self.comm, true);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Receiver<T> {
    pub(crate) inner: Loc,
    pub(crate) comm: CommunicationModel,
    _t: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for Receiver<T> {}

unsafe impl<T: Sync> Sync for Receiver<T> {}

// FIXME: Message should already imply Clone,
// but for some reason this is necessary.
impl<T: Message + Clone + 'static> Receiver<T> {
    fn new(inner: Loc, comm: CommunicationModel) -> Self {
        Receiver {
            inner,
            comm,
            _t: std::marker::PhantomData,
        }
    }

    pub fn recv_msg(&self) -> Option<T> {
        crate::recv_msg_with_tag(iter::once(&self.inner), self.comm, None).map(|x| x.0)
    }

    pub fn async_recv_msg(&self) -> impl Future<Output = T> {
        async_recv_msg(self)
    }

    pub fn recv_tagged_msg<F>(&self, f: F) -> Option<T>
    where
        F: Fn(Option<u32>) -> bool + 'static + Send + Sync,
    {
        self.recv_vec_tagged_msg(move |tag_vec| {
            let tag = tag_vec.as_ref().and_then(|tags| tags.first().copied());
            f(tag)
        })
    }

    pub fn recv_vec_tagged_msg<F>(&self, f: F) -> Option<T>
    where
        F: Fn(Option<Vec<u32>>) -> bool + 'static + Send + Sync,
    {
        let f = move |_tid, opt| f(normalize_vec_tag(opt));
        crate::recv_msg_with_tag(
            iter::once(&self.inner),
            self.comm,
            Some(PredicateType(Arc::new(f))),
        )
        .map(|x| x.0)
    }

    pub fn recv_msg_block(&self) -> T {
        crate::recv_msg_block_with_tag(iter::once(&self.inner), self.comm, None).0
    }

    pub fn try_recv(&self) -> Result<T> {
        Ok(self.recv_msg_block())
    }

    pub fn recv_tagged_msg_block<F>(&self, f: F) -> T
    where
        F: Fn(Option<u32>) -> bool + 'static + Send + Sync,
    {
        self.recv_vec_tagged_msg_block(move |tag_vec| {
            let tag = tag_vec.as_ref().and_then(|tags| tags.first().copied());
            f(tag)
        })
    }

    pub fn recv_vec_tagged_msg_block<F>(&self, f: F) -> T
    where
        F: Fn(Option<Vec<u32>>) -> bool + 'static + Send + Sync,
    {
        let f = move |_tid, opt| f(normalize_vec_tag(opt));
        crate::recv_msg_block_with_tag(
            iter::once(&self.inner),
            self.comm,
            Some(PredicateType(Arc::new(f))),
        )
        .0
    }
}

pub type Channel<T> = (Sender<T>, Receiver<T>);

// Hacky way for async_recv's cancel
pub(crate) fn from_receiver<T: Message + 'static>(recv: Receiver<T>) -> Sender<T> {
    Sender::new(recv.inner, recv.comm)
}

// A synonym of ThreadId that is hidden from the user,
// used to implement (legacy) thread channels.
#[derive(PartialEq, Eq, Clone, Debug, Hash, Serialize, Deserialize)]
pub(crate) struct Thread(pub(crate) ThreadId);

pub(crate) fn thread_loc_comm(t: ThreadId) -> (Loc, CommunicationModel) {
    let (_self_tid, model) = get_execution_state_info();
    (Loc::new(Thread(t)), model)
}

pub(crate) fn self_loc_comm() -> (Loc, CommunicationModel) {
    let (self_tid, model) = get_execution_state_info();
    (Loc::new(Thread(self_tid)), model)
}
