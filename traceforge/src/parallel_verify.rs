use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use crate::exec_graph::ExecutionGraph;
use crate::must::Must;
use crate::revisit::RevisitEnum;
use crate::runtime::execution::Execution;
use crate::runtime::thread::continuation::{ContinuationPool, CONTINUATION_POOL};
use crate::{Config, Stats};

/// Frozen thread index mapping passed from parent to child workers.
/// Ensures that `named_nondet` computes the same thread indices as in
/// sequential exploration, so `predetermined_choices` are looked up correctly.
/// Uses `Vec<u32>` (origination_vec / spawn lineage paths) as the thread key,
/// which is stable across executions unlike ThreadId.
type FrozenMap = Option<HashMap<String, HashMap<Vec<u32>, usize>>>;

/// A work item for the RevisitQueueRayon strategy.
/// Contains a graph and its full revisit queue.
type QueueWorkItem = (ExecutionGraph, BTreeMap<usize, Vec<RevisitEnum>>);

/// Rayon-based parallel verification using the RevisitQueueRayon strategy.
///
/// Builds a Rayon thread pool, performs root exploration to seed initial work
/// items, then distributes them as Rayon tasks with work-stealing.
pub fn verify_partitioned_rayon<F>(conf: Config, f: F) -> Stats
where
    F: Fn() + Send + Sync + 'static,
{
    let f = Arc::new(f);
    let start_time = Instant::now();

    let actual_pool_size = conf.partitioned_num_threads.unwrap_or_else(num_cpus::get);
    println!(
        "\n=== RAYON PARALLEL EXPLORATION ===\n\
         Pool size: {}, Branching: {:?}, \
         RevisitEagerInterval: {}, BatchSize: {}",
        actual_pool_size,
        conf.partitioned_branching,
        conf.revisit_eager_interval,
        conf.revisit_queue_batch_size,
    );

    let results = Arc::new(Mutex::new(Vec::new()));

    // Create a must instance for running callbacks at the end
    let must = Rc::new(RefCell::new(Must::new(conf.clone(), false)));

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(actual_pool_size)
        .build()
        .expect("Failed to build Rayon thread pool");

    pool.scope(|s| {
        explore_workers_revisit_queue_rayon(
            s,
            conf.clone(),
            f.clone(),
            actual_pool_size,
            results.clone(),
        );
    });

    let worker_results = results.lock().unwrap().clone();

    // Aggregate stats from all workers
    let mut total_stats = Stats::default();
    for (_, stats) in &worker_results {
        total_stats.add(stats);
    }

    // Run callbacks at end
    must.borrow_mut().run_metrics_at_end();

    // Print statistics
    let elapsed = start_time.elapsed();
    let total_execs = total_stats.execs + total_stats.block;

    println!("\n=== Rayon Exploration Results ===");
    println!("Total time: {:?}", elapsed);
    println!("Number of workers: {}", worker_results.len());
    println!(
        "Total executions: {} ({} complete, {} blocked)",
        total_execs, total_stats.execs, total_stats.block
    );
    println!("=============================================\n");

    total_stats
}

// ---------------------------------------------------------------------------
// RevisitQueueRayon strategy
// ---------------------------------------------------------------------------

/// Shared metrics for RevisitQueueRayon (thread-safe counters only).
struct RayonQueueMetrics {
    total_spawned: AtomicUsize,
    frozen_map: Mutex<FrozenMap>,
}

/// Worker function for the RevisitQueueRayon strategy.
///
/// Uses rayon's built-in work-stealing instead of a manual shared queue.
/// Worker 0 does the initial root exploration, then spawns rayon tasks for
/// each surplus work item. Each task reuses a thread-local Must instance,
/// loads the work item, calls `try_revisit()`, explores for `interval`
/// executions, keeps the best item locally, and spawns tasks for the rest.
fn explore_workers_revisit_queue_rayon<'scope, F>(
    scope: &rayon::Scope<'scope>,
    conf: Config,
    f: Arc<F>,
    num_workers: usize,
    results: Arc<Mutex<Vec<(String, Stats)>>>,
) where
    F: Fn() + Send + Sync + 'static,
{
    let interval = conf.revisit_eager_interval;
    let batch_size = conf.revisit_queue_batch_size;
    let conf = Arc::new(conf);
    let metrics = Arc::new(RayonQueueMetrics {
        total_spawned: AtomicUsize::new(0),
        frozen_map: Mutex::new(None),
    });

    // Root exploration (worker 0): seed initial work items
    let must = Rc::new(RefCell::new(Must::new((*conf).clone(), false)));
    Must::set_current(Some(must.clone()));

    must.borrow_mut().config.max_iterations = Some(interval as u64);

    explore(&must, &f);

    // Store frozen map for all tasks to use
    *metrics.frozen_map.lock().unwrap() = must.borrow().frozen_thread_index_map.clone();

    // Drain saved states for spawning; keep current for local continuation
    let saved = must.borrow_mut().drain_saved_states();
    let items = filter_nonempty_work_items(saved);
    let total_revisits: usize = items
        .iter()
        .map(|(_, rq)| rq.values().map(|v| v.len()).sum::<usize>())
        .sum();
    println!(
        "\nRevisitQueueRayon: root seeded {} items ({} revisits), batch_size={}, {} workers",
        items.len(),
        total_revisits,
        batch_size,
        num_workers,
    );
    println!("RevisitQueueRayon per-worker stats:");

    // Spawn batched rayon tasks for surplus items
    spawn_batched_tasks(
        scope, items, &conf, &f, &metrics, &results, interval, batch_size,
    );

    // Root continues locally with current state
    let root_start = Instant::now();
    loop {
        if must.borrow().current_rqueue_empty() {
            break;
        }
        if !must.borrow_mut().try_revisit() {
            break;
        }

        let execs_so_far = {
            let s = must.borrow().stats();
            (s.execs + s.block) as u64
        };
        must.borrow_mut().config.max_iterations = Some(execs_so_far + interval as u64);

        explore(&must, &f);

        // Drain saved states, spawn as batched tasks
        let saved = must.borrow_mut().drain_saved_states();
        let surplus = filter_nonempty_work_items(saved);
        spawn_batched_tasks(
            scope, surplus, &conf, &f, &metrics, &results, interval, batch_size,
        );
    }

    // Record root stats
    let root_stats = must.borrow().stats();
    let root_execs = root_stats.execs + root_stats.block;
    if root_execs > 0 {
        println!(
            "  root: {} execs, {:.1}s",
            root_execs,
            root_start.elapsed().as_secs_f64(),
        );
    }
    results
        .lock()
        .unwrap()
        .push(("rayon-root".to_string(), root_stats));
}

// Thread-local cached Must instance for RevisitQueueRayon tasks.
// Avoids creating a new Must (with Telemetry, ReplayInformation, etc.)
// for every spawned rayon task. Each rayon pool thread creates at most one.
thread_local! {
    static RAYON_CACHED_MUST: RefCell<Option<Rc<RefCell<Must>>>> = const { RefCell::new(None) };
}

/// A single rayon task for RevisitQueueRayon.
/// Receives a batch of (graph, rqueue) pairs, loads them as a state stack,
/// explores, then keeps the current state locally and spawns batched tasks
/// for the saved states.
fn rayon_queue_task<'scope, F>(
    scope: &rayon::Scope<'scope>,
    batch: Vec<QueueWorkItem>,
    conf: &Arc<Config>,
    f: &Arc<F>,
    metrics: &Arc<RayonQueueMetrics>,
    results: &Arc<Mutex<Vec<(String, Stats)>>>,
    interval: usize,
    batch_size: usize,
    task_id: usize,
) where
    F: Fn() + Send + Sync + 'static,
{
    // Reuse a thread-local Must instance if available, otherwise create one.
    let must = RAYON_CACHED_MUST.with(|cached| {
        cached.borrow_mut().take().unwrap_or_else(|| {
            Rc::new(RefCell::new(Must::new((**conf).clone(), false)))
        })
    });
    must.borrow_mut().reset_for_reuse();
    Must::set_current(Some(must.clone()));

    // Restore frozen map so begin_execution picks it up
    if let Some(ref map) = *metrics.frozen_map.lock().unwrap() {
        must.borrow_mut().frozen_thread_index_map = Some(map.clone());
    }

    // Load the full batch as a state stack (last becomes current)
    must.borrow_mut().load_state_stack(batch);
    Must::set_current(Some(must.clone()));

    loop {
        if !must.borrow_mut().try_revisit() {
            break;
        }

        // Set bound: current total + interval
        let execs_so_far = {
            let s = must.borrow().stats();
            (s.execs + s.block) as u64
        };
        must.borrow_mut().config.max_iterations = Some(execs_so_far + interval as u64);

        explore(&must, f);

        // Drain saved states (push to new tasks), keep current in place.
        let saved = must.borrow_mut().drain_saved_states();
        let surplus_items = filter_nonempty_work_items(saved);

        // Spawn batched rayon tasks for surplus items
        spawn_batched_tasks(
            scope, surplus_items, conf, f, metrics, results, interval, batch_size,
        );

        // Check if current still has work
        if must.borrow().current_rqueue_empty() {
            break;
        }
        // Loop: try_revisit on current (still loaded)
    }

    // Record stats and return Must to thread-local cache for reuse
    let worker_stats = must.borrow().stats();
    let total_execs = worker_stats.execs + worker_stats.block;
    if total_execs > 0 {
        let label = format!("rt{}", task_id);
        results.lock().unwrap().push((label, worker_stats));
    }
    RAYON_CACHED_MUST.with(|cached| {
        *cached.borrow_mut() = Some(must);
    });
}

/// Split work items into batches of `batch_size` and spawn a rayon task per batch.
fn spawn_batched_tasks<'scope, F>(
    scope: &rayon::Scope<'scope>,
    items: Vec<QueueWorkItem>,
    conf: &Arc<Config>,
    f: &Arc<F>,
    metrics: &Arc<RayonQueueMetrics>,
    results: &Arc<Mutex<Vec<(String, Stats)>>>,
    interval: usize,
    batch_size: usize,
) where
    F: Fn() + Send + Sync + 'static,
{
    if items.is_empty() {
        return;
    }
    for chunk in items.chunks(batch_size) {
        let batch: Vec<QueueWorkItem> = chunk.to_vec();
        let conf = conf.clone();
        let f = f.clone();
        let results = results.clone();
        let metrics = metrics.clone();
        let child_id = metrics.total_spawned.fetch_add(1, Ordering::Relaxed);

        scope.spawn(move |s| {
            rayon_queue_task(
                s, batch, &conf, &f, &metrics, &results, interval, batch_size, child_id,
            );
        });
    }
}

/// Filter a state stack to only include entries with non-empty revisit queues.
fn filter_nonempty_work_items(
    state_stack: Vec<(ExecutionGraph, BTreeMap<usize, Vec<RevisitEnum>>)>,
) -> Vec<QueueWorkItem> {
    state_stack
        .into_iter()
        .filter(|(_, rq)| !rq.is_empty())
        .collect()
}

/// Helper function to explore with the given Must instance.
fn explore<F>(must: &Rc<RefCell<Must>>, f: &Arc<F>)
where
    F: Fn() + Send + Sync + 'static,
{
    must.borrow_mut().started_at = Instant::now();
    Must::set_current(Some(must.clone()));
    CONTINUATION_POOL.set(&ContinuationPool::new(), || loop {
        let f = Arc::clone(f);
        let execution = Execution::new(Rc::clone(must));
        Must::begin_execution(must);
        execution.run(move || f());
        if Must::complete_execution(must) {
            break;
        }
    });
}
