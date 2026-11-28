//! This module contains the logic for printing and persisting enough failure information when a
//! test panics to allow the failure to be replayed.
//!
//! The core idea is that we install a custom panic hook (`init_panic_hook`) that runs when a thread
//! panics. That hook tries to print information about the failing schedule by calling
//! `persist_failure`.
//!
//! The complexity in this logic comes from a few requirements beyond the simple panic hook:
//! 1. If a panic occurs within `ExecutionState`, the panic hook might not be able to access the
//!    execution state to retrieve the failing schedule, so we need to be careful about accessing it
//!    and try to recover from this problem when the panic is later caught in
//!    `ExecutionState::step`. We try wherever possible to avoid panicing in this state, but if it
//!    does happen we want to get useful output and not crash.
//! 2. In addition to simply printing the failing schedule, we want to include it in the panic
//!    payload wherever possible, so that we can parse it back out in tests (essentially we are
//!    reinventing try/catch, which is usually an anti-pattern in Rust, but here we don't have
//!    control over the user code that panics). That means sometimes we end up calling
//!    `persist_schedule` twice -- once in the panic hook (where we can't modify the panic payload)
//!    and again when we catch the panic (where we can modify the payload). We don't want to print
//!    the schedule twice, so we keep track of whether the info has already been printed.

use std::panic;
use std::sync::{Mutex, Once};

use log::error;

use crate::event::Event;
use crate::must::Must;
use crate::runtime::execution::ExecutionState;

pub(crate) fn persist_task_failure(message: String, pos: Option<Event>) -> String {
    persist_failure(message, pos, None)
}

/// Like `persist_task_failure`. With `panic` (a thread's panic message), also prints the
/// pretty graph when pretty printing is on.
pub(crate) fn persist_failure(message: String, pos: Option<Event>, panic: Option<String>) -> String {
    // Disarm the panic hook so that we don't print the failure twice
    if let PanicHookState::Persisted(persisted_message) =
        PANIC_HOOK.with(|lock| lock.lock().unwrap().clone())
    {
        return persisted_message;
    }
    if let Some(must) = Must::current() {
        if let Ok(mut must) = must.try_borrow_mut() {
            must.store_replay_information(pos);
            if let Some(text) = panic.and_then(|p| must.print_panic_graph(pos, p)) {
                println!("{}", text);
            }
        } else {
            error!("Couldn't generate a counterexample because Must::current is borrowed");
        }
    } else {
        error!("Couldn't generate a counterexample because Must::current returned None");
    }

    let persisted_message = message;
    PANIC_HOOK
        .with(|lock| *lock.lock().unwrap() = PanicHookState::Persisted(persisted_message.clone()));
    println!("{}", persisted_message);
    persisted_message
}

#[derive(Clone)]
enum PanicHookState {
    Disarmed,
    Armed,
    Persisted(String),
}

thread_local! {
    static PANIC_HOOK: Mutex<PanicHookState> = const { Mutex::new(PanicHookState::Disarmed) };
}

/// A guard that disarms the panic hook when dropped
#[derive(Debug)]
#[non_exhaustive]
pub struct PanicHookGuard;

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if std::thread::panicking() { return; }
        PANIC_HOOK.with(|lock| *lock.lock().unwrap() = PanicHookState::Disarmed);
    }
}

/// Set up a panic hook that will try to print the current schedule to stderr so that the failure
/// can be replayed. Returns a guard that will disarm the panic hook when dropped.
///
/// See the module documentation for more details on how this method fits into the failure reporting
/// story.
#[must_use = "the panic hook will be disarmed when the returned guard is dropped"]
pub(crate) fn init_panic_hook() -> PanicHookGuard {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let state = PANIC_HOOK.with(|lock| {
                std::mem::replace(&mut *lock.lock().unwrap(), PanicHookState::Disarmed)
            });
            // The hook is armed if this is the first time it's fired
            if let PanicHookState::Armed = state {
                if let Some((name, pos)) = ExecutionState::failure_info() {
                    let payload = panic_info.payload();
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "Box<dyn Any>".to_string());
                    let panic = match panic_info.location() {
                        Some(l) => format!("panicked at {}:{}:{}:\n{}", l.file(), l.line(), l.column(), msg),
                        None => format!("panicked:\n{}", msg),
                    };
                    persist_failure(name, Some(pos), Some(panic));
                } else {
                    persist_task_failure("A panic was detected".to_string(), None);
                }
            }
            original_hook(panic_info);
        }));
    });

    PANIC_HOOK.with(|lock| *lock.lock().unwrap() = PanicHookState::Armed);

    PanicHookGuard
}
