use std::collections::HashMap;

use traceforge::msg::Message;
use traceforge::thread::{self, ThreadId};
use traceforge::*;
use log::{info, trace};

mod utils;

#[derive(Clone, Debug, PartialEq)]
enum ActorMsg<S: Send + 'static, T: Message + Send + 'static> {
    Init(S),
    Terminate,
    Msg(T),
}
pub trait Actor<S, T: Message + Send + 'static> {
    fn start(&mut self, d: S);
    fn handle(&mut self, m: T);
    fn stop(&mut self);
}

pub fn actor<S, T>(a: &mut dyn Actor<S, T>)
where
    S: Clone + std::fmt::Debug + PartialEq + Send + 'static,
    T: Clone + std::fmt::Debug + PartialEq + Send + 'static,
{
    let mut started = false;
    // first start the actor
    if let ActorMsg::Init(d) = recv_msg_block::<ActorMsg<S, T>>() {
        a.start(d);
        started = true;
    } else {
        traceforge::assume!(false);
    }
    loop {
        let msg: ActorMsg<S, T> = recv_msg_block();
        trace!("msg = {:?}", msg);
        match msg {
            ActorMsg::Init(_d) => {
                panic!("Restarting actor?");
            }
            ActorMsg::Msg(m) => {
                if started {
                    a.handle(m);
                } else {
                    panic!("Message to actor that is not started");
                }
            }
            ActorMsg::Terminate => {
                a.stop();
                break;
            }
        }
    }
}

pub fn send_msg_to_actor<
    S: Clone + std::fmt::Debug + PartialEq + Send + 'static,
    M: Clone + std::fmt::Debug + PartialEq + Send + 'static,
>(
    tid: ThreadId,
    m: M,
) {
    trace!("Sending message {:?}", &m);
    traceforge::send_msg(tid, ActorMsg::<S, M>::Msg(m));
}

// Messages to coordinator
#[derive(Clone, PartialEq, Debug)]
enum CoordinatorMsg {
    Request(u32),
    Yes(u32),
    No(u32),
}

// Messages to participants
#[derive(Clone, PartialEq, Debug)]
enum ParticipantMsg {
    Prepare(ThreadId, u32),
    Commit(u32),
    Abort(u32),
}

struct Coordinator {
    participants: Vec<ThreadId>,
    reply_map: HashMap<u32, (u32, u32)>,
}

impl Coordinator {
    pub fn default() -> Self {
        Self {
            participants: Vec::new(),
            reply_map: HashMap::new(),
        }
    }

    fn commit(&self, id: u32) {
        self.participants
            .iter()
            .for_each(|tid| send_msg_to_participant(*tid, ParticipantMsg::Commit(id)));
    }

    fn abort(&self, id: u32) {
        self.participants
            .iter()
            .for_each(|tid| send_msg_to_participant(*tid, ParticipantMsg::Abort(id)));
    }
}

impl Actor<Vec<ThreadId>, CoordinatorMsg> for Coordinator {
    fn start(&mut self, d: Vec<ThreadId>) {
        self.participants = d;
    }

    fn stop(&mut self) {}

    fn handle(&mut self, msg: CoordinatorMsg) {
        trace!("C received {:?}", &msg);
        match msg {
            CoordinatorMsg::Request(reqid) => {
                assert!(!self.reply_map.contains_key(&reqid));
                self.reply_map.insert(reqid, (0, 0));

                // send this request to all participants
                // send "prepare" messages
                self.participants.iter().for_each(|id| {
                    send_msg_to_participant(
                        *id,
                        ParticipantMsg::Prepare(thread::current().id(), reqid),
                    )
                });
            }
            CoordinatorMsg::Yes(id) => {
                assert!(self.reply_map.contains_key(&id));
                let (total, yes) = *self.reply_map.get(&id).unwrap();
                let _ = self.reply_map.insert(id, (total + 1, yes + 1));

                if yes + 1 == self.participants.len() as u32 {
                    self.commit(id);
                    self.reply_map.remove(&id);
                }
            }
            CoordinatorMsg::No(id) => {
                assert!(self.reply_map.contains_key(&id));
                let (total, yes) = *self.reply_map.get(&id).unwrap();
                let _ = self.reply_map.insert(id, (total + 1, yes));

                // this request id will be aborted
                if total + 1 == self.participants.len() as u32 {
                    self.abort(id);
                    self.reply_map.remove(&id);
                }
            }
        }
    }
}

type CoordinatorActorMsg = ActorMsg<Vec<ThreadId>, CoordinatorMsg>;
fn send_msg_to_coordinator(t: ThreadId, m: CoordinatorMsg) {
    send_msg_to_actor::<Vec<ThreadId>, CoordinatorMsg>(t, m);
}

fn coordinator() -> thread::JoinHandle<()> {
    let mut c = Coordinator::default();
    thread::spawn(move || actor(&mut c))
}

struct Participant {
    requests: HashMap<u32, bool>,
}

impl Participant {
    fn default() -> Self {
        Self {
            requests: HashMap::new(),
        }
    }
}

impl Actor<(), ParticipantMsg> for Participant {
    fn start(&mut self, _d: ()) {}

    fn handle(&mut self, msg: ParticipantMsg) {
        trace!("P received {:?}", &msg);
        match msg {
            ParticipantMsg::Prepare(tid, reqid) => {
                let response = traceforge::nondet();
                if response {
                    send_msg_to_coordinator(tid, CoordinatorMsg::Yes(reqid));
                } else {
                    send_msg_to_coordinator(tid, CoordinatorMsg::No(reqid));
                }
                self.requests.insert(reqid, response);
            }
            ParticipantMsg::Abort(reqid) => {
                assert!(self.requests.contains_key(&reqid));
            }
            ParticipantMsg::Commit(reqid) => {
                assert!(self.requests.contains_key(&reqid));
                assert!(self.requests.get(&reqid).unwrap()); // I muse have said Yes
            }
        }
    }

    fn stop(&mut self) {}
}

type ParticipantActorMsg = ActorMsg<(), ParticipantMsg>;
fn send_msg_to_participant(tid: ThreadId, m: ParticipantMsg) {
    send_msg_to_actor::<(), ParticipantMsg>(tid, m);
}

fn participant() -> thread::JoinHandle<()> {
    let mut c = Participant::default();
    thread::spawn(move || actor(&mut c))
}

fn two_pc(num_ps: u32) {
    let mut ps = Vec::new();

    let c = coordinator();
    for _i in 0..num_ps {
        ps.push(participant());
    }
    traceforge::send_msg(
        c.thread().id(),
        CoordinatorActorMsg::Init(ps.iter().map(|h| h.thread().id()).collect()),
    );
    let _: Vec<()> = ps
        .iter()
        .map(|h| traceforge::send_msg(h.thread().id(), ParticipantActorMsg::Init(())))
        .collect();

    send_msg_to_actor::<Vec<ThreadId>, _>(c.thread().id(), CoordinatorMsg::Request(1));
    send_msg_to_actor::<Vec<ThreadId>, _>(c.thread().id(), CoordinatorMsg::Request(2));

    traceforge::send_msg(c.thread().id(), CoordinatorActorMsg::Terminate);
    let _ = c.join();

    let _: Vec<()> = ps
        .iter()
        .map(|h| traceforge::send_msg(h.thread().id(), ParticipantActorMsg::Terminate))
        .collect();
    for h in ps {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {

    use traceforge::*;

    use super::*;

    // use utils::init_log;

    #[test]
    fn two_pc_verify() {
        let num_ps: u32 = 2;
        // init_log();
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(ConsType::FIFO)
                .with_progress_report(100)
                .with_print_all_graphs_dot("my_graphs")
                .with_verbose(1)
                .with_trace_out("/tmp/twopc.traces")
                .build(),
            move || two_pc(num_ps),
        );
        info!("Stats: {} {}", stats.execs, stats.block);
    }
}
