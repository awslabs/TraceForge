//! The naming context: while the printer formats message values, a `ThreadId` prints as the
//! thread's name and a channel endpoint as `Sender(c2.2)` or `Receiver(c2.2)`.
//!
//! The context is a thread-local installed by the printer for the duration of one print. When
//! it is not installed, the `Debug` implementations print exactly what `derive(Debug)` would.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::loc::Loc;
use crate::thread::ThreadId;

#[derive(Default)]
pub(crate) struct NameCtx {
    pub(crate) threads: HashMap<ThreadId, String>,
    pub(crate) channels: HashMap<Loc, String>,
}

thread_local! {
    static CTX: RefCell<Option<Rc<NameCtx>>> = const { RefCell::new(None) };
}

/// Restores the previous context when dropped.
pub(crate) struct Guard {
    prev: Option<Rc<NameCtx>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        let _ = CTX.try_with(|c| *c.borrow_mut() = prev);
    }
}

pub(crate) fn install(ctx: NameCtx) -> Guard {
    let prev = CTX.with(|c| c.borrow_mut().replace(Rc::new(ctx)));
    Guard { prev }
}

fn current() -> Option<Rc<NameCtx>> {
    CTX.try_with(|c| c.try_borrow().ok().and_then(|b| b.clone()))
        .ok()
        .flatten()
}

/// Whether a naming context is installed.
pub(crate) fn active() -> bool {
    current().is_some()
}

/// The name to print for a thread, if a context is installed.
pub(crate) fn thread_name(tid: ThreadId) -> Option<String> {
    current().map(|ctx| {
        ctx.threads
            .get(&tid)
            .cloned()
            .unwrap_or_else(|| format!("t{}", tid.to_number()))
    })
}

/// The name to print for a channel, if a context is installed.
pub(crate) fn channel_name(loc: &Loc) -> Option<String> {
    current().map(|ctx| {
        ctx.channels
            .get(loc)
            .cloned()
            .unwrap_or_else(|| format!("{}", loc))
    })
}
