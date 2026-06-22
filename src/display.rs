use crate::proxy::registry::AttachRegistry;
use crate::proxy::run::{Attachment, PtyEvent};

pub struct DisplayEnsure {
    pub seq: u64,
    pub key: String,
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

pub enum DisplayEvent {
    Ready { seq: u64, key: String },
    Failed { seq: u64, key: String, message: String },
}

/// How the worker spawns an attachment off the runtime thread. Defaults to the live PTY
/// path (`spawn_attachment`) when wired in; a test injects a fake to exercise off-loop
/// responsiveness without a real PTY.
type AttachmentSpawner = Box<
    dyn Fn(
            &[String],
            u16,
            u16,
            u64,
            tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        ) -> anyhow::Result<Attachment>
        + Send
        + Sync
        + 'static,
>;

/// Owns an `AttachRegistry` on a dedicated OS thread. The runtime thread sends
/// `DisplayEnsure` requests and receives `DisplayEvent`s without ever blocking on the
/// synchronous PTY-spawn work.
pub struct DisplayWorker {
    tx: std::sync::mpsc::Sender<DisplayEnsure>,
    rx: tokio::sync::mpsc::UnboundedReceiver<DisplayEvent>,
}

impl DisplayWorker {
    pub fn with_spawner(spawner: AttachmentSpawner) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DisplayEnsure>();
        std::thread::spawn(move || {
            let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
            let mut registry = AttachRegistry::with_spawner(pty_tx, spawner);
            while let Ok(req) = cmd_rx.recv() {
                let result = registry.ensure(&req.key, &req.argv, req.cols, req.rows);
                let event = match result {
                    Ok(_) => DisplayEvent::Ready {
                        seq: req.seq,
                        key: req.key,
                    },
                    Err(e) => DisplayEvent::Failed {
                        seq: req.seq,
                        key: req.key,
                        message: e.to_string(),
                    },
                };
                let _ = event_tx.send(event);
            }
            registry.teardown_all();
        });
        DisplayWorker {
            tx: cmd_tx,
            rx: event_rx,
        }
    }

    pub fn ensure(&self, req: DisplayEnsure) {
        let _ = self.tx.send(req);
    }

    pub async fn recv(&mut self) -> Option<DisplayEvent> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "current_thread")]
    async fn display_worker_slow_ensure_does_not_block_runtime_ticks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_spawner = calls.clone();
        let mut worker = crate::display::DisplayWorker::with_spawner(Box::new(
            move |_argv, _cols, _rows, id, _events| {
                calls_for_spawner.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                Ok(crate::proxy::run::fake_attachment(id))
            },
        ));

        worker.ensure(crate::display::DisplayEnsure {
            seq: 7,
            key: "local/slow".to_string(),
            argv: vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()],
            cols: 80,
            rows: 24,
        });

        tokio::time::timeout(
            Duration::from_millis(20),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("runtime tick must not be blocked by slow ensure");

        let event = tokio::time::timeout(Duration::from_millis(300), worker.recv())
            .await
            .expect("worker returns an event")
            .expect("event channel remains open");

        assert!(matches!(
            event,
            crate::display::DisplayEvent::Ready { seq: 7, ref key } if key == "local/slow"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
