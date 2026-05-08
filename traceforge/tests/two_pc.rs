use traceforge::monitor_types::*;
use traceforge::thread;
use traceforge::thread::JoinHandle;
use traceforge::thread::ThreadId;
use traceforge::SchedulePolicy;
use traceforge::{Config, ConsType};
use traceforge_macros::monitor;
use std::env;
use std::time::Instant;

// Messages to coordinator
#[derive(Clone, PartialEq, Debug)]
pub enum CoordinatorMsg {
    Init(Vec<ThreadId>),
    Yes,
    No,
}

// Messages to participants
#[derive(Clone, PartialEq, Debug)]
pub enum ParticipantMsg {
    Prepare(ThreadId),
    Commit,
    Abort,
}

fn coordinator() {
    let ps = if let CoordinatorMsg::Init(ids) = traceforge::recv_msg_block() {
        ids
    } else {
        panic!()
    };

    // send "prepare" messages
    ps.iter()
        .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Prepare(thread::current().id())));

    // collect responses
    let mut num_yes = 0;
    for _ in 0..ps.len() {
        let r_i: CoordinatorMsg = traceforge::recv_msg_block();
        match r_i {
            CoordinatorMsg::Yes => num_yes += 1,
            CoordinatorMsg::No => (),
            _ => panic!(),
        };
    }
    if num_yes == ps.len() {
        ps.iter()
            .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Commit));
    } else {
        ps.iter()
            .for_each(|id| traceforge::send_msg(*id, ParticipantMsg::Abort));
    }
}

fn participant() {
    let cid = if let ParticipantMsg::Prepare(id) = traceforge::recv_msg_block() {
        id
    } else {
        panic!();
    };

    // P1
    let response = traceforge::nondet();
    if response {
        traceforge::send_msg(cid, CoordinatorMsg::Yes);
    } else {
        traceforge::send_msg(cid, CoordinatorMsg::No);
    }

    // P2
    let action = traceforge::recv_msg_block();
    match action {
        ParticipantMsg::Commit => assert!(response),
        ParticipantMsg::Abort => (),
        _ => panic!(),
    }
}

#[test]
fn two_pc_serial() {
    let num_ps: u32 = 5;
    for cons in [ConsType::FIFO] {
        let stats = traceforge::verify(Config::builder().with_cons_type(cons).build(), move || {
            let c = thread::spawn(coordinator);

            let mut ps = Vec::new();
            for _i in 0..num_ps {
                ps.push(thread::spawn(participant));
            }
            traceforge::send_msg(
                c.thread().id(),
                CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
            );
        });
        assert_eq!(
            stats.execs as u32,
            2_u32.pow(num_ps) * (1..=num_ps).product::<u32>()
        ); // 2^N * N!
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn two_pc_parallel() {
    let default_num_ps = 5;

    let num_ps: u32 = env::var("MUST_2PC_PARTS")
        .unwrap_or(default_num_ps.to_string())
        .parse()
        .unwrap_or(default_num_ps);

    let stats = traceforge::verify(Config::builder().with_parallel(true).build(), move || {
        let c = thread::spawn(coordinator);

        let mut ps = Vec::new();
        for _i in 0..num_ps {
            ps.push(thread::spawn(participant));
        }
        traceforge::send_msg(
            c.thread().id(),
            CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
        );
    });

    assert_eq!(
        stats.execs as u32,
        2_u32.pow(num_ps) * (1..=num_ps).product::<u32>()
    ); // 2^N * N!

    assert_eq!(stats.block, 0);
}

#[ignore = "symmetry reduction"]
#[test]
fn two_pc_sym() {
    let num_ps: usize = 5;
    for cons in [ConsType::FIFO] {
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(cons)
                .with_symmetry(true)
                .build(),
            move || {
                let c = thread::spawn(coordinator);

                let mut ps: Vec<traceforge::thread::JoinHandle<()>> = Vec::new();
                for i in 0..num_ps {
                    ps.push(if i == 0 {
                        thread::spawn(participant)
                    } else {
                        traceforge::spawn_symmetric(participant, ps[i - 1].thread().id())
                    });
                }
                traceforge::send_msg(
                    c.thread().id(),
                    CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
                );
            },
        );
        assert_eq!(stats.execs, 2_usize.pow(num_ps as u32)); // 2^N
        assert_eq!(stats.block, 0);
    }
}

fn scenario(num_ps: u32) {
    let c = thread::spawn(coordinator);

    let mut ps = Vec::new();
    for _i in 0..num_ps {
        ps.push(thread::spawn(participant));
    }
    traceforge::send_msg(
        c.thread().id(),
        CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
    );
}

#[test]
fn two_pc_estimate() {
    let num_ps: u32 = 5;
    let est = traceforge::estimate_execs(move || scenario(num_ps));
    assert_eq!(
        est as u32,
        2_u32.pow(num_ps) * (1..=num_ps).product::<u32>()
    ); // 2^N * N!
}

#[test]
fn two_pc_parallel_testmode() {
    let now = Instant::now();
    let num_ps: u32 = 3;

    let stats_vec = traceforge::parallel_test(
        Config::builder()
            .with_dot_out("/tmp/twopc.dot")
            .with_error_trace("/tmp/twopc.bug")
            .with_max_iterations(2)
            // .with_verbose(1)
            .build(),
        move || scenario(num_ps),
    );
    for stats in stats_vec {
        assert_eq!(
            stats.execs as u32,
            2 // each test runs two iterations
        );
        assert_eq!(stats.block, 0);
    }
    let elapsed = now.elapsed();
    println!("Elapsed: {:.2?}", elapsed);
}

#[test]
fn two_pc_testmode() {
    let num_ps: u32 = 3;

    let _stats_vec = traceforge::test(
        Config::builder()
            .with_dot_out("/tmp/twopc.dot")
            .with_error_trace("/tmp/twopc.bug")
            .with_max_iterations(2)
            // .with_verbose(1)
            .build(),
        move || scenario(num_ps),
        5,
    );
}

#[monitor(CoordinatorMsg, ParticipantMsg)]
#[derive(Clone, Debug, Default)]
pub struct TpcMonitor {
    p_y_count: usize,
    p_n_count: usize,
    c_response: Option<ParticipantMsg>,
}

impl Observer<CoordinatorMsg> for TpcMonitor {
    fn notify(&mut self, _who: ThreadId, _whom: ThreadId, _what: &CoordinatorMsg) -> MonitorResult {
        match _what {
            CoordinatorMsg::Yes => {
                self.p_y_count += 1;
            }
            CoordinatorMsg::No => {
                self.p_n_count += 1;
            }
            CoordinatorMsg::Init(_) => (),
        }
        Ok(())
    }
}

impl Observer<ParticipantMsg> for TpcMonitor {
    fn notify(&mut self, _who: ThreadId, _whom: ThreadId, _what: &ParticipantMsg) -> MonitorResult {
        match _what {
            ParticipantMsg::Commit => {
                self.c_response = Some(ParticipantMsg::Commit);
            }
            ParticipantMsg::Abort => {
                self.c_response = Some(ParticipantMsg::Abort);
            }
            ParticipantMsg::Prepare(_) => (),
        }
        Ok(())
    }
}

impl Acceptor<CoordinatorMsg> for TpcMonitor {
    fn accept(&mut self, _who: ThreadId, _whom: ThreadId, _what: &CoordinatorMsg) -> bool {
        true
    }
}

impl Acceptor<ParticipantMsg> for TpcMonitor {
    fn accept(&mut self, _who: ThreadId, _whom: ThreadId, _what: &ParticipantMsg) -> bool {
        true
    }
}

impl Monitor for TpcMonitor {
    fn on_stop(&mut self, _execution_end: &ExecutionEnd) -> MonitorResult {
        if self.p_n_count > 0 && self.c_response == Some(ParticipantMsg::Commit) {
            Err("'No' Response Count > 0, but Committed".to_string())
        } else if self.p_n_count == 0 && self.c_response == Some(ParticipantMsg::Abort) {
            Err("'No' Response Count == 0, but Aborted".to_string())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
enum Mon {
    UseMon,
    DontUse,
}

fn two_pc_monitored(num_ps: u32, cons_type: ConsType, policy: SchedulePolicy, monitored: Mon) {
    let mut execs = vec![];
    for i in 0..10 {
        // Let iteration 0 use LTR to get a canonical number of execs; then validate that all
        // remaining execs match it.
        let monitored = monitored.clone();
        let policy = if i == 0 { SchedulePolicy::LTR } else { policy };
        let stats = traceforge::verify(
            Config::builder()
                .with_cons_type(cons_type)
                .with_verbose(20)
                .with_policy(if i == 0 { SchedulePolicy::LTR } else { policy })
                .build(),
            move || {
                let mtid1 = if let Mon::UseMon = monitored {
                    Some(start_monitor_tpc_monitor(TpcMonitor::default()))
                } else {
                    None
                };

                let c = thread::spawn(coordinator);

                let mut ps: Vec<JoinHandle<()>> =
                    (0..num_ps).map(|_| thread::spawn(participant)).collect();

                traceforge::send_msg(
                    c.thread().id(),
                    CoordinatorMsg::Init(ps.iter().map(|tid| tid.thread().id()).collect()),
                );

                c.join().unwrap();
                ps.drain(..).for_each(|p| p.join().unwrap());

                if let Some(mtid1) = mtid1 {
                    terminate_monitor_tpc_monitor(mtid1.thread().id());
                    assert!(mtid1.join().unwrap().is_ok());
                }
            },
        );
        assert_eq!(stats.block, 0);
        execs.push(stats.execs);
    }
    let expected = execs.iter().map(|_| execs[0]).collect::<Vec<_>>();
    assert_eq!(execs, expected);
}

macro_rules! two_pc_tests {
    ($($name:ident: $value:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let (num_ps, cons_type, policy, monitored) = $value;
                two_pc_monitored(num_ps, cons_type, policy, monitored);
            }
        )*
    }
}

// TraceForge is sensitive to difference combinations of parameters.
// At present it seems that MO consistency type is not working right; every test run
// with the same parameters should get the same number of executions, but MO does not
// do this.
two_pc_tests! {
    two_pc_1_mo_ltr: (1, ConsType::MO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_1_mo_arb: (1, ConsType::MO, SchedulePolicy::Arbitrary, Mon::DontUse),
    two_pc_1_fifo_ltr: (1, ConsType::FIFO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_1_fifo_arb: (1, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::DontUse),
    two_pc_2_mo_ltr: (2, ConsType::MO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_2_mo_arb: (2, ConsType::MO, SchedulePolicy::Arbitrary, Mon::DontUse),
    two_pc_2_fifo_ltr: (2, ConsType::FIFO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_2_fifo_arb: (2, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::DontUse),
    two_pc_3_mo_ltr: (3, ConsType::MO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_3_mo_arb: (3, ConsType::MO, SchedulePolicy::Arbitrary, Mon::DontUse),
    two_pc_3_fifo_ltr: (3, ConsType::FIFO, SchedulePolicy::LTR, Mon::DontUse),
    two_pc_3_fifo_arb: (3, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::DontUse),

    two_pc_monitored_1_mo_ltr: (1, ConsType::MO, SchedulePolicy::LTR, Mon::UseMon),
    // FAILS: two_pc_monitored_1_mo_arb: (1, ConsType::MO, SchedulePolicy::Arbitrary, Mon::UseMon),
    two_pc_monitored_1_fifo_ltr: (1, ConsType::FIFO, SchedulePolicy::LTR, Mon::UseMon),
    two_pc_monitored_1_fifo_arb: (1, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::UseMon),
    two_pc_monitored_2_mo_ltr: (2, ConsType::MO, SchedulePolicy::LTR, Mon::UseMon),
    // FAILS: two_pc_monitored_2_mo_arb: (2, ConsType::MO, SchedulePolicy::Arbitrary, Mon::UseMon),
    two_pc_monitored_2_fifo_ltr: (2, ConsType::FIFO, SchedulePolicy::LTR, Mon::UseMon),
    two_pc_monitored_2_fifo_arb: (2, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::UseMon),
    two_pc_monitored_3_mo_ltr: (3, ConsType::MO, SchedulePolicy::LTR, Mon::UseMon),
    // FAILS: two_pc_monitored_3_mo_arb: (3, ConsType::MO, SchedulePolicy::Arbitrary, Mon::UseMon),
    two_pc_monitored_3_fifo_ltr: (3, ConsType::FIFO, SchedulePolicy::LTR, Mon::UseMon),
    two_pc_monitored_3_fifo_arb: (3, ConsType::FIFO, SchedulePolicy::Arbitrary, Mon::UseMon),
}
