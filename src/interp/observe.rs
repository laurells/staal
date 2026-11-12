// The run observer (RUNTIME.md section 20): a run's events as they happen, for a program
// that shows a run live (Staal Studio). Its guarantees, each tested
// (tests/observer_test.rs):
//
//  - It cannot change a run: it receives copies of events, taken after secrets are
//    removed, and nothing it does or returns reaches the run's state, result, log or
//    journal.
//  - Its failure cannot fail the run: an observer that panics or returns an error is
//    detached, and the run goes on.
//  - It cannot block the run: events go into a bounded buffer, and the run only ever tries
//    to add one; when the buffer is full, the event is dropped and counted, and the next
//    event delivered says how many were dropped just before it, so a gap is shown as a
//    gap, never hidden. What was dropped after the last event delivered is told at the end.
//  - Every event carries the run's id, its task and the operation's id where it has one,
//    and the events of one task arrive in that task's order.
//  - The event schema is named, with the staalc version that emits it.
//
// What an observer sees is when events were seen. The run's log is the record: it is
// assembled in task order when the run ends, and its `seq` numbers are its own.

use serde_json::Value as Json;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

/// The schema of the events an observer receives; it changes with the staalc version that
/// emits them (`Observed::staalc`).
pub const SCHEMA: &str = "staalc-observer-1";

/// One event of a run, as it happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    /// `SCHEMA`.
    pub schema: &'static str,
    /// The staalc that emitted it.
    pub staalc: &'static str,
    pub run_id: String,
    /// The concurrent task that emitted it, "" for the program's own.
    pub task: String,
    /// The journal operation it is about, as the journal names it (`n`, or `task:n`): the
    /// one it names; the one being made, for an event emitted while it is made; or the one
    /// just made, for an event reporting what it gave. None for an event about no operation
    /// (a refusal, a validation, the run's start and end), and for one emitted before its
    /// operation begins (an action or a model request, which may yet be refused and never
    /// become one): the event reporting its outcome names it.
    pub operation: Option<String>,
    /// The event as the run's log holds it (its `event` name and fields, secrets removed),
    /// without the log's `seq`, which the log assigns when the run ends.
    pub event: Json,
    /// How many events were dropped just before this one because the buffer was full.
    pub missed_before: u64,
}

/// What an observer is told when the run is over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    /// Events it received and accepted.
    pub delivered: u64,
    /// Events dropped after the last one delivered: with every `missed_before`, all the
    /// events it did not receive.
    pub missed: u64,
    /// Why it was detached, if it was: what it returned, or that it panicked. Nothing was
    /// delivered to it after that.
    pub detached: Option<String>,
}

/// What watches a run. Called on a thread of its own, never the run's.
pub trait Observer: Send {
    /// One event. An error, or a panic, detaches the observer: it receives nothing more.
    fn event(&mut self, e: Observed) -> Result<(), String>;
    /// Called once, when the run is over and every event it was sent has been offered,
    /// whether or not it was detached.
    fn end(&mut self, _end: End) {}
}

/// An observer attached to a run, with the number of events its buffer holds (at least 1).
pub struct Observation {
    pub observer: Box<dyn Observer>,
    pub capacity: usize,
}

/// The run's side of an observation: shared by the run and its concurrent tasks.
pub(crate) struct Feed {
    run_id: OnceLock<String>,
    state: Mutex<Sending>,
    /// Events dropped after the last one sent, when the run lets the feed go.
    final_missed: Arc<std::sync::atomic::AtomicU64>,
}

struct Sending {
    tx: Option<SyncSender<Observed>>,
    /// Dropped since the last event sent.
    dropped: u64,
}

impl Feed {
    /// Starts the observer's thread. A run whose observer cannot be started does not start.
    pub(crate) fn start(o: Observation) -> Result<Arc<Feed>, String> {
        if o.capacity == 0 {
            return Err("an observer's buffer must hold at least one event".into());
        }
        let (tx, rx) = mpsc::sync_channel::<Observed>(o.capacity);
        let final_missed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let missed = final_missed.clone();
        let mut observer = o.observer;
        std::thread::Builder::new()
            .name("staalc-observer".into())
            .spawn(move || {
                let mut delivered = 0u64;
                let mut detached: Option<String> = None;
                for e in rx {
                    if detached.is_some() {
                        continue;
                    }
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.event(e))) {
                        Ok(Ok(())) => delivered += 1,
                        Ok(Err(why)) => detached = Some(why),
                        Err(p) => detached = Some(format!("it panicked: {}", panic_text(&p))),
                    }
                }
                // Every sender is gone: the run is over.
                let end = End { delivered, missed: missed.load(std::sync::atomic::Ordering::SeqCst), detached };
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.end(end)));
            })
            .map_err(|e| format!("could not start the observer's thread: {e}"))?;
        Ok(Arc::new(Feed { run_id: OnceLock::new(), state: Mutex::new(Sending { tx: Some(tx), dropped: 0 }), final_missed }))
    }

    /// The run's id, once its journal has given it.
    pub(crate) fn set_run_id(&self, id: &str) {
        let _ = self.run_id.set(id.to_string());
    }

    /// Offers `event` of `task`, about `operation`, never waiting for the observer.
    pub(crate) fn send(&self, task: &str, event: &Json, operation: Option<String>) {
        let mut copy = event.clone();
        if let Some(o) = copy.as_object_mut() {
            o.remove("seq");
        }
        let mut st = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(tx) = &st.tx else { return };
        let e = Observed {
            schema: SCHEMA,
            staalc: crate::VERSION,
            run_id: self.run_id.get().cloned().unwrap_or_default(),
            task: task.to_string(),
            operation,
            event: copy,
            missed_before: st.dropped,
        };
        match tx.try_send(e) {
            Ok(()) => st.dropped = 0,
            Err(TrySendError::Full(_)) => st.dropped += 1,
            // The observer's thread has ended; there is no one to tell.
            Err(TrySendError::Disconnected(_)) => st.tx = None,
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let mut st = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Before the sender goes, so the observer's thread reads it after the last event.
        self.final_missed.store(st.dropped, std::sync::atomic::Ordering::SeqCst);
        st.tx = None;
    }
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into())
}
