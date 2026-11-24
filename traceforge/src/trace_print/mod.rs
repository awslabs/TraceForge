//! The readable trace printer, used when the configuration sets
//! `with_pretty_graph_printing(true)`.
//!
//! The analysis builds the list of visible events from the graph and the marks the libraries
//! left in it (see [`marks`]). The renderer prints that list twice: per thread, and, when the
//! graph is printed because of a failure, as one interleaving.

mod analysis;
pub(crate) mod marks;
pub(crate) mod names;
mod order;
mod render;
#[cfg(test)]
pub(crate) mod tests;

use crate::event::Event;
use crate::exec_graph::ExecutionGraph;
use crate::monitor_types::EndCondition;

pub(crate) use marks::*;

/// What failed, when the graph is printed because of a failure.
#[derive(Clone, Debug)]
pub(crate) enum Failure {
    /// A monitor's `on_stop` returned an error.
    Monitor {
        message: String,
        condition: EndCondition,
    },
    /// `traceforge::assert` failed at `pos`.
    Assertion { pos: Event },
    /// A thread panicked after its event `pos`.
    Panic { pos: Option<Event>, message: String },
}

#[derive(Clone, Debug)]
pub(crate) struct FailureReport {
    pub(crate) failure: Failure,
    pub(crate) seed: u64,
}

/// The text of the graph, with a `VIOLATION` block and the interleaving if `failure` is given.
pub(crate) fn print(g: &ExecutionGraph, failure: Option<&FailureReport>) -> String {
    let views = analysis::thread_views(g);
    let _names = names::install(analysis::name_ctx(g, &views));
    let a = analysis::analyse(g, views);
    let order = failure.map(|_| order::interleaving(g, &a));
    let text = render::render(&a, failure, order.as_deref());
    #[cfg(test)]
    tests::record(&text, tests::structural_problems(&a, order.as_deref()));
    text
}
