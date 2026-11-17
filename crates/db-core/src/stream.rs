use crate::{Outcome, QueryEvent};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// The driver's side of the switch a [`QueryHandle`] holds.
pub trait QueryControl: Send + Sync + 'static {
    /// Stop the query on the server. Called at most once per query, from any
    /// thread; must not block.
    fn cancel(&self);

    /// The consumer took one batch out of the stream. The sidecar client
    /// grants one credit here; in-process drivers rely on the bounded channel
    /// and can ignore it.
    fn batch_consumed(&self) {}
}

/// The stream is closed: the consumer dropped its [`QueryHandle`]. The driver
/// should stop the query and return.
#[derive(Debug)]
pub struct SinkClosed;

/// Where a driver writes the events of one query.
pub struct EventSink {
    tx: mpsc::Sender<QueryEvent>,
}

impl EventSink {
    /// Waits while the consumer is `window` batches behind.
    pub async fn send(&self, event: QueryEvent) -> Result<(), SinkClosed> {
        self.tx.send(event).await.map_err(|_| SinkClosed)
    }

    /// For driver code that runs on a plain thread.
    pub fn blocking_send(&self, event: QueryEvent) -> Result<(), SinkClosed> {
        self.tx.blocking_send(event).map_err(|_| SinkClosed)
    }

    /// Resolves when the consumer dropped its handle.
    pub async fn closed(&self) {
        self.tx.closed().await
    }
}

/// Creates the two ends of one query's stream. `window` is how many events
/// may wait unread before [`EventSink::send`] blocks.
pub fn channel(window: usize, control: Arc<dyn QueryControl>) -> (EventSink, QueryHandle) {
    let (tx, rx) = mpsc::channel(window.max(1));
    (
        EventSink { tx },
        QueryHandle {
            rx,
            control,
            cancelled: Arc::new(AtomicBool::new(false)),
            finished: false,
        },
    )
}

/// The consumer's side of one running query.
///
/// Dropping the handle before `Finished` arrives cancels the query, so a
/// closed tab never leaves a statement running on the server.
pub struct QueryHandle {
    rx: mpsc::Receiver<QueryEvent>,
    control: Arc<dyn QueryControl>,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}

impl QueryHandle {
    /// Next event, or `None` after `Finished` (or if the driver died without
    /// sending it, which callers should treat as an internal error).
    /// Runtime-agnostic: works on GPUI's executor as well as tokio.
    pub async fn next(&mut self) -> Option<QueryEvent> {
        let event = self.rx.recv().await;
        self.note(&event);
        event
    }

    /// Like [`next`](Self::next) for a plain thread. Must not be called from
    /// inside an async runtime.
    pub fn blocking_next(&mut self) -> Option<QueryEvent> {
        let event = self.rx.blocking_recv();
        self.note(&event);
        event
    }

    fn note(&mut self, event: &Option<QueryEvent>) {
        match event {
            Some(QueryEvent::Batch(_)) => self.control.batch_consumed(),
            Some(QueryEvent::Finished(_)) | None => self.finished = true,
            _ => {}
        }
    }

    /// Ask the server to stop. The stream still ends with `Finished`:
    /// `Cancelled` once the server-side query is gone, or `Done` if it had
    /// already completed.
    pub fn cancel(&self) {
        self.canceller().cancel();
    }

    /// A clonable cancel switch that can outlive borrows of the handle, e.g.
    /// for a toolbar button while a task owns the handle.
    pub fn canceller(&self) -> Canceller {
        Canceller {
            control: self.control.clone(),
            cancelled: self.cancelled.clone(),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

impl Drop for QueryHandle {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel();
        }
    }
}

#[derive(Clone)]
pub struct Canceller {
    control: Arc<dyn QueryControl>,
    cancelled: Arc<AtomicBool>,
}

impl Canceller {
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.control.cancel();
        }
    }
}

/// A handle that is already finished, for errors found before any work starts.
pub fn finished(outcome: Outcome) -> QueryHandle {
    struct Noop;
    impl QueryControl for Noop {
        fn cancel(&self) {}
    }
    let (sink, handle) = channel(1, Arc::new(Noop));
    let _ = sink.tx.try_send(QueryEvent::Finished(outcome));
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Count {
        cancels: AtomicUsize,
        batches: AtomicUsize,
    }
    impl QueryControl for Count {
        fn cancel(&self) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }
        fn batch_consumed(&self) {
            self.batches.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn drop_before_finish_cancels_once() {
        let c = Arc::new(Count::default());
        let (_sink, handle) = channel(2, c.clone());
        handle.cancel();
        handle.canceller().cancel();
        drop(handle);
        assert_eq!(c.cancels.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn finished_handle_does_not_cancel_and_counts_batches() {
        let c = Arc::new(Count::default());
        let (sink, mut handle) = channel(4, c.clone());
        sink.blocking_send(QueryEvent::Batch(crate::BatchBuilder::new(0).finish()))
            .unwrap();
        sink.blocking_send(QueryEvent::Finished(Outcome::Done { rows: 0 }))
            .unwrap();
        assert!(matches!(handle.blocking_next(), Some(QueryEvent::Batch(_))));
        assert!(matches!(
            handle.blocking_next(),
            Some(QueryEvent::Finished(_))
        ));
        drop(handle);
        assert_eq!(c.cancels.load(Ordering::SeqCst), 0);
        assert_eq!(c.batches.load(Ordering::SeqCst), 1);
    }
}
