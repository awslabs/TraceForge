use std::collections::HashMap;

use traceforge::{
    recv_msg_block, recv_tagged_msg_block,
    thread::{self, ThreadId},
    Config,
};

#[derive(Clone, Debug, PartialEq)]
enum Request {
    Put(u32), // client request to put record

    Log(u32, u32),
    Ok(u32),

    Fail, // model of failure detector
          //TakeOver,

          //Terminate, // stop the servers
}

#[derive(Clone, Debug, PartialEq)]
struct Init(ThreadId, ThreadId, ThreadId);

const INIT_TAG: u32 = 424242;

enum Role {
    Primary,
    Standby,
    Backup,
}

#[derive(Clone, Debug, PartialEq)]
struct Replica {
    primary: ThreadId,
    secondary: ThreadId,
    version: u32,
}

#[allow(unused)]
#[derive(Clone, Debug, PartialEq)]
enum ReplicaMsg {
    Read(ThreadId),
    Write(ThreadId, Replica),
    CAS(ThreadId, Replica, Replica),

    RdOk(Replica),
    WrOk,
    CASResponse(bool),

    Terminate,
}

fn _replica(pid: ThreadId, sid: ThreadId) {
    let mut state = Replica {
        primary: pid,
        secondary: sid,
        version: 0,
    };
    loop {
        let m = recv_msg_block();
        match m {
            ReplicaMsg::Read(tid) => {
                traceforge::send_msg(tid, ReplicaMsg::RdOk(state.clone()));
            }
            ReplicaMsg::Write(tid, r) => {
                state = r;
                traceforge::send_msg(tid, ReplicaMsg::WrOk);
            }
            ReplicaMsg::CAS(tid, oldr, newr) => {
                if state == oldr {
                    state = newr;
                    traceforge::send_msg(tid, ReplicaMsg::CASResponse(true));
                } else {
                    traceforge::send_msg(tid, ReplicaMsg::CASResponse(false));
                }
            }
            _ => panic!("Strange message {:?}", m),
        }
    }
}

fn initialize(who: Role) -> (ThreadId, ThreadId, ThreadId) {
    let m = recv_tagged_msg_block(|_, t| t.is_some() && t.unwrap() == INIT_TAG);
    let Init(pid, sid, sbyid) = m;
    let id = match who {
        Role::Primary => pid,
        Role::Standby => sbyid,
        Role::Backup => sid,
    };
    assert_eq!(id, thread::current().id());
    (pid, sid, sbyid)
}

fn primary_node() -> () {
    let mut commit_log: Vec<u32> = Vec::new();
    let mut buffer: HashMap<u32, u32> = HashMap::new();
    let mut acked: HashMap<u32, u32> = HashMap::new();
    let mut txid: u32 = 0;

    let (_pid, sid, _sbyid) = initialize(Role::Primary);

    loop {
        let m = recv_msg_block();
        match m {
            Request::Put(v) => {
                // serve a put: send it to the secondary
                // we will commit and reply when we get a reply
                let _ = buffer.insert(txid, v);
                txid += 1;

                traceforge::send_msg(sid, Request::Log(txid, v));
            }
            Request::Ok(tx) => {
                // backup confirms txid. Push it to commit_log and reply to client
                // if this is the earliest
                if txid == tx {
                    let v = buffer.remove(&tx).unwrap();
                    commit_log.push(v);
                    txid += 1;
                    loop {
                        let v = acked.remove(&txid);
                        if v.is_none() {
                            break;
                        } else {
                            commit_log.push(v.unwrap());
                            txid += 1;
                        }
                    }
                } else {
                    let v = buffer.remove(&tx).unwrap();
                    let _ = acked.insert(tx, v);
                }
            }
            Request::Log(..) => panic!("primary should not receive Log"),
            Request::Fail => {
                // recover by boosting standby
                // Fail says that Primary's fault detector thinks secondary has failed
                // The recovery protocol is as follows:
                // the primary sends the log to Standby
                // the primary writes to replica
                // if it wins, then it informs Standby
                // if it loses, it means secondary won the race and it should become failed
                // in a failed state, it should not accept requests
                println!("fail");
            } //Request::TakeOver => panic!("Primary should not receive Takeover"),

              //Request::Terminate => break,
        }
    }
}

fn secondary_node() -> () {
    let mut commit_log: Vec<u32> = Vec::new();
    let mut buffer: HashMap<u32, u32> = HashMap::new();
    let mut txid = 0;

    let (pid, _sid, _sbyid) = initialize(Role::Backup);
    loop {
        let m = recv_msg_block();
        match m {
            Request::Put(_) => {
                println!("Dropping put");
            }
            Request::Ok(_) => {
                panic!("backup should not receive Ok");
            }
            Request::Log(tx, v) => {
                if tx == txid {
                    commit_log.push(v);
                    traceforge::send_msg(pid, Request::Ok(tx));
                    txid += 1;
                    loop {
                        let v = buffer.remove(&txid);
                        if v.is_some() {
                            commit_log.push(v.unwrap());
                            traceforge::send_msg(pid, Request::Ok(txid));
                            txid += 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    // buffer this
                    let old = buffer.insert(tx, v);
                    assert!(old.is_none());
                }
            }
            Request::Fail => {
                // recover by boosting standby
            } //Request::TakeOver => panic!("Backup should not receive Takeover"),

              //Request::Terminate => break,
        }
    }
}

fn standby_node() -> () {
    let (_pid, _sid, _sbyid) = initialize(Role::Standby);
}

fn env(fault_budget: usize, primary: ThreadId, secondary: ThreadId, _standby: ThreadId) {
    for _i in 0..fault_budget {
        if traceforge::nondet() {
            // trigger the fault detector in the primary
            traceforge::send_msg(primary, Request::Fail);
        } else {
            // trigger the fault detector in the secondary
            traceforge::send_msg(secondary, Request::Fail);
        }
    }
}

fn crash_scenario() {
    let fault_budget = 1; // how many faults shall we simulate?
    let primary = thread::spawn(primary_node);
    let secondary = thread::spawn(secondary_node);
    let standby = thread::spawn(standby_node);

    let pid = primary.thread().id();
    let sid = secondary.thread().id();
    let sbyid = standby.thread().id();

    traceforge::send_tagged_msg(pid, INIT_TAG, Init(pid, sid, sbyid));
    traceforge::send_tagged_msg(sid, INIT_TAG, Init(pid, sid, sbyid));
    traceforge::send_tagged_msg(sbyid, INIT_TAG, Init(pid, sid, sbyid));

    let environment = thread::spawn(move || env(fault_budget, pid, sid, sbyid));

    traceforge::send_msg(pid, Request::Put(1));
    traceforge::send_msg(pid, Request::Put(2));
    let _ = environment.join();
}

#[test]
fn test_crash() {
    traceforge::verify(
        Config::builder()
            //.with_verbose(2)
            //.with_trace_out("/tmp/mpsc.traces")
            .build(),
        move || crash_scenario(),
    );
}
