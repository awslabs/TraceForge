//! This package defines a Telemetry module for TraceForge to gather various statistics in one place.
//! The design of the module is influenced strongly by the `metrics` crate.
//! Since we need some custom logic in the telemetry, e.g., in state space estimation, we overload the histogram
//! interface of `metrics`.
//!
//! Caveat: If we require a histogram for some other reason in the future, we have to define Handler appropriately
//!
//! The `Telemetry` module allows us to put all telemetry in one place
//! (rather than several scattered variables in the code interspersed with the logic of the model checker).
//!
//! Caveat 2: This telemetry design comes at a performance overhead: instead of simply incrementing counters, we
//! are keeping everything in a mutex-wrapped hashtable. We should check if the overhead is worth it or
//! discuss redesigning this module

// we allow dead code because we created Gauges that we don't use yet, but might use in the future
// TODO: revisit decision
#![allow(dead_code)]

use log::{debug, info, warn};

use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::coverage::ExecutionId;

pub type Key = String;
pub type KeyName = String;
pub type SharedString = String;

/// An object which can be converted into a `f64` representation.
///
/// This trait provides a mechanism for existing types, which have a natural representation
/// as a 64-bit floating-point number, to be transparently passed in when recording a histogram.
pub trait IntoF64 {
    /// Converts this object to its `f64` representation.
    fn into_f64(self) -> f64;
}

impl IntoF64 for f64 {
    fn into_f64(self) -> f64 {
        self
    }
}

/// A counter handler.
pub trait CounterFn {
    /// Increments the counter by the given amount.
    ///
    /// Returns the previous value.
    fn increment(&self, value: u64);

    /// Sets the counter to at least the given amount.
    ///
    /// This is intended to support use cases where multiple callers are attempting to synchronize
    /// this counter with an external counter that they have no control over.  As multiple callers
    /// may read that external counter, and attempt to set it here, there could be reordering issues
    /// where a caller attempts to set an older (smaller) value after the counter has been updated to
    /// the latest (larger) value.
    ///
    /// This method must cope with those cases.  An example of doing so atomically can be found in
    /// `AtomicCounter`.
    ///
    /// Returns the previous value.
    fn absolute(&self, value: u64);
}

/// A gauge handler.
pub trait GaugeFn {
    /// Increments the gauge by the given amount.
    ///
    /// Returns the previous value.
    fn increment(&self, value: f64);

    /// Decrements the gauge by the given amount.
    ///
    /// Returns the previous value.
    fn decrement(&self, value: f64);

    /// Sets the gauge to the given amount.
    ///
    /// Returns the previous value.
    fn set(&self, value: f64);
}

/// A histogram handler.
pub trait HistogramFn {
    /// Records a value into the histogram.
    fn record(&self, value: f64);
}

/// A counter.
#[derive(Clone)]
pub struct Counter {
    inner: Option<Arc<dyn CounterFn + Send + Sync>>,
}

/// A gauge.
#[derive(Clone)]
pub struct Gauge {
    inner: Option<Arc<dyn GaugeFn + Send + Sync>>,
}

/// A histogram.
#[derive(Clone)]
pub struct Histogram {
    inner: Option<Arc<dyn HistogramFn + Send + Sync>>,
}

impl Counter {
    /// Creates a no-op `Counter` which does nothing.
    ///
    /// Suitable when a handle must be provided that does nothing i.e. a no-op recorder or a layer
    /// that disables specific metrics, and so on.
    pub fn noop() -> Self {
        Self { inner: None }
    }

    /// Creates a `Counter` based on a shared handler.
    pub fn from_arc<F: CounterFn + Send + Sync + 'static>(a: Arc<F>) -> Self {
        Self { inner: Some(a) }
    }

    /// Increments the counter.
    pub fn increment(&self, value: u64) {
        if let Some(c) = &self.inner {
            c.increment(value)
        }
    }

    /// Sets the counter to an absolute value.
    pub fn absolute(&self, value: u64) {
        if let Some(c) = &self.inner {
            c.absolute(value)
        }
    }
}

impl Gauge {
    /// Creates a no-op `Gauge` which does nothing.
    ///
    /// Suitable when a handle must be provided that does nothing i.e. a no-op recorder or a layer
    /// that disables specific metrics, and so on.
    pub fn noop() -> Self {
        Self { inner: None }
    }

    /// Creates a `Gauge` based on a shared handler.
    pub fn from_arc<F: GaugeFn + Send + Sync + 'static>(a: Arc<F>) -> Self {
        Self { inner: Some(a) }
    }

    /// Increments the gauge.
    pub fn increment<T: IntoF64>(&self, value: T) {
        if let Some(g) = &self.inner {
            g.increment(value.into_f64())
        }
    }

    /// Decrements the gauge.
    pub fn decrement<T: IntoF64>(&self, value: T) {
        if let Some(g) = &self.inner {
            g.decrement(value.into_f64())
        }
    }

    /// Sets the gauge.
    pub fn set<T: IntoF64>(&self, value: T) {
        if let Some(g) = &self.inner {
            g.set(value.into_f64())
        }
    }
}

impl Histogram {
    /// Creates a no-op `Histogram` which does nothing.
    ///
    /// Suitable when a handle must be provided that does nothing i.e. a no-op recorder or a layer
    /// that disables specific metrics, and so on.
    pub fn noop() -> Self {
        Self { inner: None }
    }

    /// Creates a `Histogram` based on a shared handler.
    pub fn from_arc<F: HistogramFn + Send + Sync + 'static>(a: Arc<F>) -> Self {
        Self { inner: Some(a) }
    }

    /// Records a value in the histogram.
    pub fn record<T: IntoF64>(&self, value: T) {
        if let Some(ref inner) = self.inner {
            inner.record(value.into_f64())
        }
    }
}

/// A trait for registering and recording metrics.
///
/// This is the core trait that allows interoperability between exporter implementations and the
/// macros provided by `metrics`.
pub trait Recorder {
    /// Describes a counter.
    ///
    fn describe_counter(&self, key: KeyName, description: SharedString);

    /// Describes a gauge.
    ///
    fn describe_gauge(&self, key: KeyName, description: SharedString);

    /// Describes a histogram.
    ///
    fn describe_histogram(&self, key: KeyName, description: SharedString);

    /// Registers a counter.
    fn register_counter(&self, key: &Key) -> Counter;

    /// Registers a gauge.
    fn register_gauge(&self, key: &Key) -> Gauge;

    /// Registers a histogram.
    fn register_histogram(&self, key: &Key) -> Histogram;
}

/// A no-op recorder.
///
/// Used as the default recorder when one has not been installed yet.  Useful for acting as the root
/// recorder when testing layers.
pub struct NoopRecorder;

impl Recorder for NoopRecorder {
    fn describe_counter(&self, _key: KeyName, _description: SharedString) {}
    fn describe_gauge(&self, _key: KeyName, _description: SharedString) {}
    fn describe_histogram(&self, _key: KeyName, _description: SharedString) {}
    fn register_counter(&self, _key: &Key) -> Counter {
        Counter::noop()
    }
    fn register_gauge(&self, _key: &Key) -> Gauge {
        Gauge::noop()
    }
    fn register_histogram(&self, _key: &Key) -> Histogram {
        Histogram::noop()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StateEstimator {
    pub val: f64,
    pub exact: bool,
    pub threshold: f64,
}

impl StateEstimator {
    pub fn new<T: IntoF64>(init: T, threshold: T) -> Self {
        Self {
            val: init.into_f64(),
            exact: true,
            threshold: threshold.into_f64(),
        }
    }

    pub fn default() -> Self {
        Self::new(1.0_f64, 1_000_000.0_f64)
    }

    fn compute_appx(&mut self, v: f64) {
        let logfv = v.ln();
        let newval = self.val + logfv;
        if self.val.is_finite() && !newval.is_finite() {
            warn!("StateEstimator: Overflow in sample! This record may be ignored");
        }
        self.val = newval;
    }

    pub fn as_f64(&self) -> f64 {
        if self.exact {
            self.val
        } else {
            self.val.exp()
        }
    }

    pub fn sample<T: IntoF64>(&mut self, v: T) {
        let fv = v.into_f64();
        assert!(fv > 0.0);
        if !self.val.is_finite() {
            return;
        }
        if self.exact {
            let newv = self.val * fv;
            if newv.is_finite() && newv < self.threshold {
                self.val = newv;
            } else {
                debug!("Transferring estimator from exact to approximate");
                self.exact = false;
                self.val = self.val.ln();
                self.compute_appx(fv);
            }
        } else {
            self.compute_appx(fv);
        }
    }
}

impl std::fmt::Display for StateEstimator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.exact || !self.val.is_finite() {
            write!(f, "{}", self.val)
        } else {
            let vexp = self.val.exp();
            if vexp.is_finite() {
                write!(f, "appx {}", vexp)
            } else {
                write!(f, "appx exp({})", self.val)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Value {
    U64(u64),
    F64(f64),
    StateEstimator(StateEstimator),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::U64(uval) => write!(f, "{uval}"),
            Value::F64(fval) => write!(f, "{fval}"),
            Value::StateEstimator(lc) => write!(f, "{}", lc),
        }
    }
}

#[derive(Clone, Debug)]
struct Handle {
    key: Key,
    inner: Arc<Mutex<HashMap<Key, Value>>>,
}

impl Handle {
    pub fn new(key: Key, inner: Arc<Mutex<HashMap<Key, Value>>>) -> Self {
        Self { key, inner }
    }
}

impl CounterFn for Handle {
    fn increment(&self, value: u64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        let entry = m.get_mut(&self.key);
        match entry {
            None => {
                m.insert(self.key.clone(), Value::U64(value));
            }
            Some(v) => match v {
                Value::U64(u64v) => *v = Value::U64(*u64v + value),
                _ => panic!("Strange value"),
            },
        }
    }

    fn absolute(&self, value: u64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        *m.entry(self.key.clone()).or_insert(Value::U64(0)) = Value::U64(value);
    }
}

impl GaugeFn for Handle {
    fn increment(&self, value: f64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        let entry = m.get_mut(&self.key);
        match entry {
            None => {
                m.insert(self.key.clone(), Value::F64(value));
            }
            Some(v) => match v {
                Value::F64(f64v) => *v = Value::F64(*f64v + value),
                _ => panic!("Strange value"),
            },
        }
    }

    fn decrement(&self, value: f64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        let entry = m.get_mut(&self.key);
        match entry {
            None => {
                m.insert(self.key.clone(), Value::F64(-value));
            }
            Some(v) => match v {
                Value::F64(f64v) => *v = Value::F64(*f64v - value),
                _ => panic!("Strange value"),
            },
        }
    }

    fn set(&self, value: f64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        *m.entry(self.key.clone()).or_insert(Value::F64(0.0)) = Value::F64(value);
    }
}

impl HistogramFn for Handle {
    fn record(&self, value: f64) {
        let mut m = self.inner.lock().expect("Could not get lock");
        let entry = m.get_mut(&self.key);
        match entry {
            None => {
                let mut se = StateEstimator::default();
                se.sample(value);
                m.insert(self.key.clone(), Value::StateEstimator(se));
            }
            Some(v) => match v {
                Value::StateEstimator(se) => {
                    se.sample(value);
                }
                _ => panic!("Strange value"),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Telemetry {
    inner: Arc<Mutex<HashMap<Key, Value>>>,

    pub coverage: CoverageTelemetry,
}

impl Telemetry {
    pub fn new(keep_per_execution_coverage: bool) -> Self {
        let hm: HashMap<Key, Value> = HashMap::new();
        Self {
            inner: Arc::new(Mutex::new(hm)),
            coverage: CoverageTelemetry::new(keep_per_execution_coverage),
        }
    }

    pub fn take(self) -> HashMap<Key, Value> {
        mem::take(
            &mut Arc::into_inner(self.inner)
                .unwrap()
                //.expect("Could not unwrap telemetry hashmap")
                .into_inner()
                .unwrap(),
        )
        //.expect("Could not extract telemetry from lock")
    }

    pub fn read_counter(&self, k: Key) -> Option<u64> {
        let hm = self.inner.lock().expect("Cannot get lock");
        let v = hm.get(&k).cloned();
        match v {
            Some(Value::U64(u)) => Some(u),
            _ => None,
        }
    }

    pub fn read_gauge(&self, k: Key) -> Option<f64> {
        let hm = self.inner.lock().expect("Cannot get lock");
        let v = hm.get(&k).cloned();
        match v {
            Some(Value::F64(u)) => Some(u),
            _ => None,
        }
    }

    pub fn read_histogram(&self, k: Key) -> Option<f64> {
        let hm = self.inner.lock().expect("Cannot get lock");
        let v = hm.get(&k).cloned();
        match v {
            Some(Value::StateEstimator(se)) => {
                debug!("State estimator {k} has value {se}");
                Some(se.as_f64())
            }
            _ => None,
        }
    }

    pub fn read_estimator(&self, k: Key) -> Option<StateEstimator> {
        let hm = self.inner.lock().expect("Cannot get lock");
        let v = hm.get(&k).cloned();
        info!("read estimator called with {}, value {:?}", k, v);
        match v {
            Some(Value::StateEstimator(se)) => Some(se),
            _ => None,
        }
    }

    pub fn stat(&self, k: Key) -> Option<Value> {
        let hm = self.inner.lock().expect("Cannot get lock");
        hm.get(&k).cloned()
    }

    pub(crate) fn print(&self) {
        let hm = self.inner.lock().expect("Could not get lock");
        println!("========================= Stats ==============");
        for (k, v) in &*hm {
            println!("{}: {}", k, v);
        }
        println!("==============================================");
    }

    pub fn increment_counter(&self, k: Key, v: u64) {
        let c = self.register_counter(&k);
        c.increment(v);
    }

    pub fn counter(&self, k: Key) {
        self.increment_counter(k, 1u64);
    }

    pub fn absolute_counter(&self, k: Key, v: u64) {
        let c = self.register_counter(&k);
        c.absolute(v);
    }

    pub fn increment_gauge(&self, k: Key, v: f64) {
        let g = self.register_gauge(&k);
        g.increment(v);
    }
    pub fn decrement_gauge(&self, k: Key, v: f64) {
        let g = self.register_gauge(&k);
        g.decrement(v);
    }

    pub fn gauge(&self, k: Key, v: f64) {
        let g = self.register_gauge(&k);
        g.set(v);
    }

    pub fn histogram(&self, k: Key, v: f64) {
        let g = self.register_histogram(&k);
        g.record(v);
    }
}

impl Recorder for Telemetry {
    fn describe_counter(&self, key_name: KeyName, description: SharedString) {
        log::info!(
            "(counter) registered key {} with description {:?}",
            key_name.as_str(),
            description
        );
    }

    fn describe_gauge(&self, key_name: KeyName, description: SharedString) {
        log::info!(
            "(gauge) registered key {} with description {:?}",
            key_name.as_str(),
            description
        );
    }

    fn describe_histogram(&self, key_name: KeyName, description: SharedString) {
        log::info!(
            "(histogram) registered key {} with description {:?}",
            key_name.as_str(),
            description
        );
    }

    fn register_counter(&self, key: &Key) -> Counter {
        let handler = Handle::new(key.clone(), self.inner.clone());
        Counter::from_arc(Arc::new(handler))
    }

    fn register_gauge(&self, key: &Key) -> Gauge {
        let handler = Handle::new(key.clone(), self.inner.clone());
        Gauge::from_arc(Arc::new(handler))
    }

    fn register_histogram(&self, key: &Key) -> Histogram {
        let handler = Handle::new(key.clone(), self.inner.clone());
        Histogram::from_arc(Arc::new(handler))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExecutionIdGenerator {
    current_val: Rc<AtomicU32>,
}

impl ExecutionIdGenerator {
    pub fn new() -> Self {
        Self {
            current_val: Rc::new(AtomicU32::new(0)),
        }
    }

    pub fn get(&mut self) -> ExecutionId {
        self.current_val.fetch_add(1, Ordering::SeqCst)
    }

    pub fn current(&self) -> ExecutionId {
        self.current_val.load(Ordering::SeqCst)
    }
}

pub(crate) type Coverage = HashMap<String, u64>;
pub(crate) type ModuleFileLine = (&'static str, &'static str, u32);

/* The coverage telemetry data structure is a map from each execution graph to
    associated coverage information as well as the aggregate coverage and information about the execution id

*/
#[derive(Clone, Debug)]
pub(crate) struct CoverageTelemetry {
    // per execution statistics about hitting a coverage goal
    perexec: Arc<Mutex<HashMap<ExecutionId, Coverage>>>,
    // global aggregate statistics
    aggregate: Arc<Mutex<Coverage>>,
    // ensuring that the same coverage goal name is not used in multiple places, warning if multiple instances are hit
    // stores the module name, file name, and line number along with a coverage goal
    unique_table: Arc<Mutex<HashMap<String, ModuleFileLine>>>,

    // execution id generator for the must engine: each execution gets a unique id
    idgen: ExecutionIdGenerator,
     
    // whether to keep per-execution coverage data across all executions
    keep_per_execution_coverage: bool,
}

impl CoverageTelemetry {
    pub fn new(keep_per_execution_coverage: bool) -> Self {
        Self {
            perexec: Arc::new(Mutex::new(HashMap::new())),
            aggregate: Arc::new(Mutex::new(Coverage::new())),
            unique_table: Arc::new(Mutex::new(HashMap::new())),
            idgen: ExecutionIdGenerator::new(),
            keep_per_execution_coverage,
        }
    }

    pub fn new_eid(&mut self) -> ExecutionId {
        self.idgen.get()
    }

    pub fn current_eid(&self) -> ExecutionId {
        self.idgen.current()
    }

    fn debug_aggr(&self, atbl: Coverage) {
        println!("Debug_aggr");
        for (k, v) in atbl.iter() {
            println!("{:?} -> {}", k, v);
        }
    }

    pub fn cover(
        &mut self,
        eid: ExecutionId,
        covgoal: String,
        module: &'static str,
        file: &'static str,
        line: u32,
    ) {
        debug!("In cover: {covgoal} {module} {file} {line}");
        /* The logic of `cover` is as follows:
           1. a coverage goal is a user-given name to the coverage point,
           together with the module path and location in the code (`file` and `line` number).

           2. In the coverage telemetry, we save both coverage information per execution and in aggregate.
           The aggregate information is the sum of per-execution coverage information.

           3. If the same coverage goal shows up in different places, we emit a warning but glob all the goals together.
           TODO: Check if this is the right semantics or if we should try to internally disambiguate the different goals
        */

        // check uniqueness
        let mut uniquetable = self
            .unique_table
            .lock()
            .expect("Could not lock unique table");
        let location = uniquetable.get_mut(&covgoal);
        match location {
            None => {
                // this coverage goal was not seen before
                debug!("Coverage goal {covgoal} not seen before");
                uniquetable.insert(covgoal.clone(), (module, file, line));
            }
            Some((mname, fname, lno)) => {
                debug!("Coverage goal {covgoal} seen before at {mname} {fname} {lno} and now at {module} {file} {line}");
                if module != *mname || file != *fname || line != *lno {
                    eprintln!("WARN: Coverage goal {covgoal} has multiple occurrences: seen at module {module} (file {file}::{line}, previously at module {mname} (file {fname}::{lno}.");
                    eprintln!("WARN: TraceForge will glob these different occurrences. You should disambiguate the goal names if this is not intended behavior.");
                    warn!("Coverage goal {covgoal} has multiple occurrences: seen at module {module} (file {file}::{line}, previously at module {mname} (file {fname}::{lno}");
                }
            }
        }

        let mut hm = self.perexec.lock().expect("Could not lock coverage table");
        let thistable = hm.get_mut(&eid);
        match thistable {
            None => {
                debug!("inserting {} in per-exectbl for the first time", covgoal);
                let mut covtbl = Coverage::new();
                covtbl.insert(covgoal.clone(), 1);
                hm.insert(eid, covtbl);
            }
            Some(covtbl) => {
                debug!("inserting {} in per-exectbl", covgoal);
                let current = covtbl.get_mut(&covgoal);
                match current {
                    None => {
                        let _ = covtbl.insert(covgoal.clone(), 1);
                    }
                    Some(v) => {
                        *v += 1;
                    }
                }
            }
        }

        let mut atbl = self
            .aggregate
            .lock()
            .expect("Could not lock aggregate table");
        let goal = atbl.get_mut(&covgoal);
        match goal {
            None => {
                debug!("inserting {} in aggr-tbl for the first time", covgoal);
                atbl.insert(covgoal, 1);
            }
            Some(v) => {
                debug!("inserting {} in aggr-tbl", covgoal);
                *v += 1;
            }
        }
    }

    // Queries

    // returns if the coverage goal was covered in the execution `eid`
    pub fn is_covered_in_exec(&self, eid: ExecutionId, covgoal: String) -> bool {
        let tbl = self.perexec.lock().expect("Could not lock aggregate table");
        let ctbl = tbl.get(&eid);
        match ctbl {
            None => false,
            Some(ctbl) => {
                let n = ctbl.get(&covgoal).unwrap_or(&0);
                *n > 0
            }
        }
    }

    // returns if the coverage goal is covered in some execution
    pub fn is_covered(&self, covgoal: String) -> bool {
        let n = *self
            .aggregate
            .lock()
            .expect("Could not lock aggregate table")
            .get(&covgoal)
            .unwrap_or(&0);
        n > 0
    }

    // returns the number of times coverage goal is covered in the execution with id `eid`
    pub fn covered_in_exec(&self, eid: ExecutionId, covgoal: String) -> u64 {
        let getbl = self.perexec.lock().expect("Could not lock per exec table");
        let etbl = getbl.get(&eid);
        match etbl {
            None => 0,
            Some(tbl) => {
                let n = tbl.get(&covgoal).unwrap_or(&0);
                *n
            }
        }
    }

    // returns the total number of times the coverage goal is covered
    pub fn covered(&self, covgoal: String) -> u64 {
        *self
            .aggregate
            .lock()
            .expect("Could not lock aggregate table")
            .get(&covgoal)
            .unwrap_or(&0)
    }

    pub fn debug(&self) {
        println!("=====================");
        println!("Printing aggregate statistics");
        let aggregatetbl = self
            .aggregate
            .lock()
            .expect("Could not lock aggregate table");
        self.debug_aggr(aggregatetbl.to_owned());
        println!("=====================");
        println!();
        println!("Printing per execution statistics");
        let perexectbl = self
            .perexec
            .lock()
            .expect("Could not acquire per-exec lock");
        for (k, v) in &*perexectbl {
            println!("Execution id: {:}", k);
            println!("=====================");
            for (key, val) in v.iter() {
                println!("Key: {:} => {:}", key, val);
            }
            println!("=====================");
        }
    }

    pub fn export_aggregate(&self) -> Coverage {
        let atbl = self
            .aggregate
            .lock()
            .expect("Could not get aggregate table");
        atbl.clone()
    }

    pub fn export_current(&self) -> Coverage {
        let eid = self.current_eid();
        let mut hm = self.perexec.lock().expect("Could not lock coverage table");
        let thistable_option = hm.get_mut(&eid);
        match thistable_option {
            None => Coverage::default(), // it is possible that no coverage goal was hit in this execution and so no table was created for this execution
            Some(thistable) => thistable.clone(),
        }
    }

    /// Cleans up the per-execution coverage data for the current execution if not configured to keep it.
    /// This should be called after ExecutionObserver callbacks have been invoked.
    pub fn cleanup_current_execution(&self) {
        if !self.keep_per_execution_coverage {
            let eid = self.current_eid();
            let mut hm = self.perexec.lock().expect("Could not lock coverage table");
            hm.remove(&eid);
            debug!(
                "Cleaned up per-execution coverage data for execution {}",
                eid
            );
        }
    }
}

#[test]
fn cov_telemetry_basic() {
    let mut cv = CoverageTelemetry::new(true);
    let eid = cv.new_eid();
    cv.cover(eid, "B".to_owned(), std::module_path!(), "foo", 1);
    cv.cover(eid, "C".to_owned(), std::module_path!(), "foo", 2);
    cv.cover(eid, "C".to_owned(), std::module_path!(), "foo", 3);

    assert_eq!(1, cv.covered("B".to_owned()));
    assert!(cv.is_covered("B".to_owned()));
    assert!(!cv.is_covered("X".to_owned()));
    assert_eq!(2, cv.covered("C".to_owned()));

    let new_eid = cv.new_eid();
    assert!(cv.is_covered_in_exec(eid, "B".to_owned()));
    assert!(!cv.is_covered_in_exec(new_eid, "B".to_owned()));
}
