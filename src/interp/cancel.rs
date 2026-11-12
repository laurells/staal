// Cancelling a run from outside (RUNTIME.md section 22): a handle the program that started
// the run keeps, and the run checks. A cancelled run stops at its next step, or before its
// next operation, with a `cancelled` failure; an operation already under way is never
// interrupted (stopping an action part way would leave its outcome unknown), so it
// finishes or times out first. Like a run stopped in doubt, a cancelled run records no
// end in its journal: it can be resumed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// A run's cancel handle: clone it to keep one, and `cancel` to stop the run.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicU8>,
}

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Asks the run to stop. It stops at its next step, or before its next operation;
    /// asking again does nothing more.
    pub fn cancel(&self) {
        self.flag.store(1, Ordering::SeqCst);
    }

    /// Read through the same flag native code reads, so both engines see one value.
    pub fn is_cancelled(&self) -> bool {
        self.flag().load(Ordering::SeqCst) != 0
    }

    /// The flag itself: the interpreter reads it here, native code at each step.
    pub(crate) fn flag(&self) -> &AtomicU8 {
        &self.flag
    }
}

impl std::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cancel({})", if self.is_cancelled() { "cancelled" } else { "running" })
    }
}
