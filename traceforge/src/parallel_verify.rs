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
/// Contains an execution graph snapshot and its pending revisit queue.
/// These are produced by `drain_saved_states()` and distributed to rayon tasks.
type QueueWorkItem = (ExecutionGraph, BTreeMap<usize, Vec<RevisitEnum>>);

// =============================================================================
// Public entry point
// =============================================================================

/// Rayon-based parallel verification using the RevisitQueueRayon strategy.
///
/// # Architecture
///
/// 1. A rayon thread pool is created with `num_threads` workers.
/// 2. The root worker (worker 0) runs on the calling thread and performs an
///    initial exploration of `iterations_until_split` executions.
/// 3. After the initial exploration, backward revisits produce saved states
///    (graph snapshots + revisit queues). These are drained and distributed
///    as rayon tasks — each task gets a batch of work items.
/// 4. Each rayon task loads its batch, explores for `interval` executions,
///    then drains its own saved states and spawns new rayon tasks for them.
///    This creates a recursive work-stealing pattern where rayon handles
///    load balancing automatically.
/// 5. The root worker also continues exploring its own local state in a loop,
///    spawning new tasks for any saved states it produces.
/// 6. When all tasks exhaust their revisit queues, the scope exits and
///    statistics are aggregated.
///
/// # Memory management
///
/// Each `explore()` call creates a fresh `ContinuationPool` that allocates
/// mmap'd generator stacks for green threads. These stacks are explicitly
/// freed via `drain_and_free()` after each explore call. Without this,
/// `ManuallyDrop<Generator>` would prevent the stacks from being munmap'd,
/// leaking mmap regions until the kernel's `vm.max_map_count` limit is hit.
/// See `ContinuationPool::drain_and_free()` for details.
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
        conf.iterations_until_split,
        conf.state_batch_size,
    );

    let results = Arc::new(Mutex::new(Vec::new()));

    // Temporary Must for running end-of-exploration callbacks (not used for exploration itself)
    let must = Rc::new(RefCell::new(Must::new(conf.clone(), false)));

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(actual_pool_size)
        .build()
        .expect("Failed to build Rayon thread pool");

    // All exploration happens inside this scope. Rayon guarantees that all
    // spawned tasks complete before the scope exits.
    pool.scope(|s| {
        explore_workers_revisit_queue_rayon(
            s,
            conf.clone(),
            f.clone(),
            actual_pool_size,
            results.clone(),
        );
    });

    // --- Post-exploration: aggregate results ---

    let worker_results = results.lock().unwrap().clone();

    let mut total_stats = Stats::default();
    for (_, stats) in &worker_results {
        total_stats.add(stats);
    }

    must.borrow_mut().run_metrics_at_end();

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

// =============================================================================
// RevisitQueueRayon internals
// =============================================================================

/// Shared state across all rayon tasks (thread-safe).
struct RayonQueueMetrics {
    /// Monotonic counter for assigning unique task IDs (for logging).
    total_spawned: AtomicUsize,
    /// Frozen thread index mapping computed by the root worker during its
    /// first exploration. Cloned into each rayon task so that `named_nondet`
    /// produces consistent thread indices across all workers.
    frozen_map: Mutex<FrozenMap>,
}

/// Orchestrates the root worker and initial task spawning.
///
/// Runs on the calling thread inside `pool.scope()`. The root worker:
/// 1. Performs the first `interval` executions to seed saved states.
/// 2. Freezes the thread index mapping for all future tasks.
/// 3. Distributes saved states as rayon tasks.
/// 4. Continues exploring its own local state, spawning new tasks each interval.
fn explore_workers_revisit_queue_rayon<'scope, F>(
    scope: &rayon::Scope<'scope>,
    conf: Config,
    f: Arc<F>,
    num_workers: usize,
    results: Arc<Mutex<Vec<(String, Stats)>>>,
) where
    F: Fn() + Send + Sync + 'static,
{
    let interval = conf.iterations_until_split;
    let batch_size = conf.state_batch_size;
    let conf = Arc::new(conf);
    let metrics = Arc::new(RayonQueueMetrics {
        total_spawned: AtomicUsize::new(0),
        frozen_map: Mutex::new(None),
    });

    // --- Phase 1: Root exploration to seed work items ---

    let must = Rc::new(RefCell::new(Must::new((*conf).clone(), false)));
    Must::set_current(Some(must.clone()));

    // Run the first `interval` executions. This populates the execution graph
    // and generates backward revisits (saved states) that become work items.
    must.borrow_mut().config.max_iterations = Some(interval as u64);
    explore(&must, &f);

    // Freeze the thread index mapping so all tasks use consistent indices.
    // This must happen after the first exploration which builds the mapping.
    *metrics.frozen_map.lock().unwrap() = must.borrow().frozen_thread_index_map.clone();

    // --- Phase 2: Distribute initial work items ---

    // drain_saved_states() returns (graph, rqueue) pairs from backward revisits.
    // These are the branching points that need to be explored in parallel.
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

    spawn_batched_tasks(
        scope, items, &conf, &f, &metrics, &results, interval, batch_size,
    );

    // --- Phase 3: Root continues exploring locally ---
    // The root keeps its current state (not drained) and loops: revisit →
    // explore → drain → spawn tasks, until its revisit queue is exhausted.

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

        // After each interval, drain saved states and distribute as new tasks
        let saved = must.borrow_mut().drain_saved_states();
        let surplus = filter_nonempty_work_items(saved);
        spawn_batched_tasks(
            scope, surplus, &conf, &f, &metrics, &results, interval, batch_size,
        );
    }

    // Record root worker stats
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

// Thread-local cached Must instance for rayon tasks.
// Each rayon pool thread creates at most one Must (expensive: includes
// Telemetry, ReplayInformation, etc.) and reuses it across tasks via
// reset_for_reuse(). The Must is taken out at the start of a task and
// returned at the end, so it's never shared across concurrent tasks.
thread_local! {
    static RAYON_CACHED_MUST: RefCell<Option<Rc<RefCell<Must>>>> = const { RefCell::new(None) };
}

/// A single rayon task: the recursive building block of parallel exploration.
///
/// Each task:
/// 1. Acquires a Must instance (from thread-local cache or freshly allocated).
/// 2. Restores the frozen thread index mapping for consistent named_nondet.
/// 3. Loads its batch of (graph, rqueue) pairs as a state stack.
/// 4. Loops: pick a revisit → explore for `interval` executions → drain
///    saved states → spawn new rayon tasks for them.
/// 5. When the revisit queue is exhausted, records stats and returns the
///    Must instance to the thread-local cache.
///
/// This creates a recursive work-stealing tree: each task can spawn children,
/// and rayon handles scheduling and load balancing across pool threads.
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
    // --- Setup: acquire and configure Must ---

    let must = RAYON_CACHED_MUST.with(|cached| {
        cached.borrow_mut().take().unwrap_or_else(|| {
            Rc::new(RefCell::new(Must::new((**conf).clone(), false)))
        })
    });
    // Clear accumulated state from previous task on this thread
    must.borrow_mut().reset_for_reuse();
    Must::set_current(Some(must.clone()));

    // Restore frozen thread index mapping so begin_execution uses
    // consistent indices for named_nondet across all workers.
    if let Some(ref map) = *metrics.frozen_map.lock().unwrap() {
        must.borrow_mut().frozen_thread_index_map = Some(map.clone());
    }

    // Load the batch: last item becomes current state, rest become saved states.
    must.borrow_mut().load_state_stack(batch);
    Must::set_current(Some(must.clone()));

    // --- Explore loop ---

    loop {
        // try_revisit() picks the next revisit from the current state's queue.
        // If the current queue is empty, it pops from the saved state stack.
        // Returns false when all revisits are exhausted.
        if !must.borrow_mut().try_revisit() {
            break;
        }

        // Explore for `interval` more executions from this revisit point.
        let execs_so_far = {
            let s = must.borrow().stats();
            (s.execs + s.block) as u64
        };
        must.borrow_mut().config.max_iterations = Some(execs_so_far + interval as u64);

        explore(&must, f);

        // Drain saved states produced by backward revisits during this
        // interval. These are new branching points to explore. Distribute
        // them as child rayon tasks for parallel exploration.
        let saved = must.borrow_mut().drain_saved_states();
        let surplus_items = filter_nonempty_work_items(saved);

        spawn_batched_tasks(
            scope, surplus_items, conf, f, metrics, results, interval, batch_size,
        );

        // If current state has no more revisits, the next try_revisit()
        // will pop from saved states (if any) or return false.
        if must.borrow().current_rqueue_empty() {
            break;
        }
    }

    // --- Teardown: record stats and return Must to cache ---

    let worker_stats = must.borrow().stats();
    let total_execs = worker_stats.execs + worker_stats.block;
    if total_execs > 0 {
        let label = format!("rt{}", task_id);
        results.lock().unwrap().push((label, worker_stats));
    }
    // Return Must to thread-local cache for reuse by the next task on this thread.
    RAYON_CACHED_MUST.with(|cached| {
        *cached.borrow_mut() = Some(must);
    });
}

/// Split work items into batches and spawn a rayon task per batch.
///
/// Consumes `items` by value (moves, not clones) to avoid duplicating
/// ExecutionGraph data. Each batch becomes a separate rayon task that
/// will be scheduled via work-stealing across the thread pool.
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
    let mut iter = items.into_iter().peekable();
    while iter.peek().is_some() {
        let batch: Vec<QueueWorkItem> = iter.by_ref().take(batch_size).collect();
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

/// Filter work items to only those with non-empty revisit queues.
/// Items with empty queues have no branching points left to explore.
fn filter_nonempty_work_items(
    state_stack: Vec<(ExecutionGraph, BTreeMap<usize, Vec<RevisitEnum>>)>,
) -> Vec<QueueWorkItem> {
    state_stack
        .into_iter()
        .filter(|(_, rq)| !rq.is_empty())
        .collect()
}

/// Run executions on the given Must instance until `max_iterations` is reached
/// or all revisits are exhausted.
///
/// Creates a fresh `ContinuationPool` for green thread stacks. After the
/// exploration loop, `drain_and_free()` explicitly munmaps all generator
/// stacks. This is critical in the parallel path where `explore()` is called
/// repeatedly — without it, `ManuallyDrop<Generator>` in `Continuation`
/// prevents automatic cleanup, leaking mmap regions until the kernel's
/// `vm.max_map_count` (default 65530) is exhausted and the process crashes.
/// The sequential path doesn't need this because its pool persists for the
/// entire verification run and stacks are recycled.
fn explore<F>(must: &Rc<RefCell<Must>>, f: &Arc<F>)
where
    F: Fn() + Send + Sync + 'static,
{
    must.borrow_mut().started_at = Instant::now();
    Must::set_current(Some(must.clone()));
    let pool = ContinuationPool::new();
    CONTINUATION_POOL.set(&pool, || loop {
        let f = Arc::clone(f);
        let execution = Execution::new(Rc::clone(must));
        Must::begin_execution(must);
        execution.run(move || f());
        if Must::complete_execution(must) {
            break;
        }
    });
    // Explicitly free mmap'd generator stacks before the pool drops.
    // Safe here because we are in normal execution, not TLS destruction.
    // See ContinuationPool::drain_and_free() for the full rationale.
    pool.drain_and_free();
}
