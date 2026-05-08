use std::{cell::RefCell, rc::Rc, sync::Arc};

use crate::{must::Must, Config, SchedulePolicy, Stats};
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};

use rand::prelude::*;
use rand_pcg::Pcg64Mcg;

/// `verify` tries to systematically explore the state space until completion. For preliminary analysis,
/// `parallel_test` provides an incomplete exploration strategy. `parallel_test` spins up a configurable number of threads,
/// and runs TraceForge on each thread using different random scheduling orders on each thread but otherwise running
/// the ODPOR algorithm. `parallel_test` is not meant to be comprehensive and usually runs with an upper bound on the number
/// of executions explored in each thread or an upper bound on the time
/// (both can be configured---see `with_samples` and `with_time_bound`)
///
/// TODO: In the future, we can make `test` scale on multiple servers
pub fn parallel_test<F>(mut conf: Config, f: F) -> Vec<Stats>
where
    F: Fn() + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let worker_count: usize = if let Some(rpw) = conf.parallel_workers {
        rpw
    } else if let Ok(rpw) = std::env::var("MUST_PARALLEL_WORKERS") {
        rpw.parse().unwrap()
    } else {
        num_cpus::get()
    };
    println!("[TraceForge Test] Spinning up {worker_count} threads");

    conf.schedule_policy = SchedulePolicy::Arbitrary;

    let mut jh = Vec::new();
    for worker in 0..worker_count {
        let mut conf_clone = conf.clone();
        let f_clone = f.clone();
        let tid = std::thread::spawn(move || {
            // set output file names indexed by the worker id to prevent scribbling on each other
            conf_clone.rename_files(format!("{worker}"));
            let must = Rc::new(RefCell::new(Must::new(conf_clone, false)));
            crate::explore(&must, &f_clone);
            let stats = must.borrow().stats();
            stats
        });
        jh.push(tid);
    }
    // collect and return the stats
    jh.into_iter()
        .map(|j| j.join().expect("Could not join test workers"))
        .collect::<Vec<_>>()
}

/// `verify` tries to systematically explore the state space until completion. For preliminary analysis,
/// `test` provides an incomplete exploration strategy. `test` is not meant to be comprehensive and usually runs with an upper bound on the number
/// of executions explored
pub fn test<F>(mut config: Config, f: F, samples: u64) -> f64
where
    F: Fn() + Send + Sync + 'static,
{
    config.schedule_policy = SchedulePolicy::Arbitrary;

    let f = Arc::new(f);

    let mut seed = config.seed;
    let mut rng = Pcg64Mcg::seed_from_u64(config.seed);

    let must = Rc::new(RefCell::new(Must::new(config, false)));

    // Create a single ContinuationPool for the entire test run so that
    // green-thread stacks (mmap'd memory) are reused across samples
    // instead of being leaked on every pool drop.
    let pool = ContinuationPool::new();
    CONTINUATION_POOL.set(&pool, || {
        for i in 0..samples {
            must.borrow_mut().reset_for_sample(seed);
            crate::explore_with_pool(&must, &f);

            let progress_desc = format!("Executions attempted so far: {}", i);
            if Must::should_report(i) {
                println!("{}", progress_desc);
            }
            seed = rng.next_u64();
        }
    });

    // Clear the thread-local reference so the Must instance can be dropped
    Must::set_current(None);
    0.0
}
