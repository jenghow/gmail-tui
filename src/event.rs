//! Terminal event plumbing.
//!
//! crossterm's async event stream and a redraw timer are folded into a single
//! `AppEvent` channel so the main loop only has to await one thing.

use std::time::Duration;

use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A raw terminal event: key press, resize, paste, ...
    Term(Event),
    /// Periodic tick; keeps the loop responsive and leaves room for
    /// animations / timeouts later.
    Tick,
}

pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Kept alive for as long as the handler lives.
    _task: JoinHandle<()>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let task = tokio::spawn(async move {
            let mut term_events = EventStream::new();
            let mut ticker = tokio::time::interval(tick_rate);

            loop {
                let event = tokio::select! {
                    _ = ticker.tick() => AppEvent::Tick,
                    next = term_events.next() => match next {
                        Some(Ok(event)) => AppEvent::Term(event),
                        // Backend error or the stream ended: stop reading.
                        _ => break,
                    },
                };

                if tx.send(event).is_err() {
                    break;
                }
            }
        });

        Self { rx, _task: task }
    }

    /// Await the next event. `None` means the reader task has stopped.
    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }
}
