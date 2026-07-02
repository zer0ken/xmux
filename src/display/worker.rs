use crate::display::attachment::{spawn_attachment, Attachment, PtyEvent};

/// A request to attach a session's display PTY off the runtime thread. `id` is issued
/// by the app via `AttachRegistry::alloc_id` so the resulting attachment's id lines
/// up with `reap`. `seq` lets the app drop a stale reply after rapid navigation.
pub struct DisplayEnsure {
    pub seq: u64,
    pub key: String,
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub id: u64,
}

/// The worker's reply. `Ready` carries the finished `Attachment` for the app to
/// insert into the registry it owns (the worker never owns the registry).
pub enum DisplayEvent {
    Ready {
        seq: u64,
        key: String,
        attachment: Attachment,
    },
    Failed {
        seq: u64,
        key: String,
        message: String,
    },
}

/// How the worker spawns an attachment. Defaults to the live `spawn_attachment`; a test
/// injects a fake to exercise off-loop responsiveness without a real PTY.
type AttachmentSpawner = Box<
    dyn Fn(
            &[String],
            u16,
            u16,
            u64,
            tokio::sync::mpsc::UnboundedSender<PtyEvent>,
            &[String],
        ) -> anyhow::Result<Attachment>
        + Send
        + Sync
        + 'static,
>;

/// Runs `spawn_attachment` on a dedicated OS thread so the 10–94ms ConPTY open+spawn
/// never blocks the runtime. The app sends `DisplayEnsure` and receives
/// `DisplayEvent`; the worker is handed the app's `pty_tx`, so each spawned
/// attachment's pump feeds the app's grid (the worker keeps no registry of its own).
pub struct DisplayWorker {
    tx: std::sync::mpsc::Sender<DisplayEnsure>,
    /// The reply receiver. The run loop takes it out ([`take_events`](Self::take_events))
    /// so it can `select!` on replies while holding `&mut Runtime` for the arm body —
    /// the send half (`ensure`) stays on the worker the runtime owns.
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<DisplayEvent>>,
}

impl DisplayWorker {
    pub fn new(pty_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>) -> Self {
        Self::with_spawner(pty_tx, Box::new(spawn_attachment))
    }

    pub fn with_spawner(
        pty_tx: tokio::sync::mpsc::UnboundedSender<PtyEvent>,
        spawner: AttachmentSpawner,
    ) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<DisplayEnsure>();
        std::thread::spawn(move || {
            while let Ok(req) = cmd_rx.recv() {
                // Resolve the mux session vars to strip from the attach child here,
                // at the spawn call site, so the low-level `spawn_attachment` never
                // names a mux var — the vocabulary stays in `mux::vocab`.
                let env_clear =
                    crate::mux::vocab::mux_env_keys_to_clear(std::env::vars().map(|(k, _)| k));
                let event = match spawner(
                    &req.argv,
                    req.cols,
                    req.rows,
                    req.id,
                    pty_tx.clone(),
                    &env_clear,
                ) {
                    Ok(attachment) => DisplayEvent::Ready {
                        seq: req.seq,
                        key: req.key,
                        attachment,
                    },
                    Err(e) => DisplayEvent::Failed {
                        seq: req.seq,
                        key: req.key,
                        message: e.to_string(),
                    },
                };
                if event_tx.send(event).is_err() {
                    break; // the app is gone
                }
            }
        });
        DisplayWorker {
            tx: cmd_tx,
            rx: Some(event_rx),
        }
    }

    pub fn ensure(&self, req: DisplayEnsure) {
        let _ = self.tx.send(req);
    }

    /// Takes the reply receiver out for the run loop to poll as a loop-local (so a
    /// worker `Ready`/`Failed` can be `select!`ed while `&mut Runtime` is borrowed for the
    /// arm). Callable once; the worker keeps only its send half afterwards.
    pub fn take_events(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<DisplayEvent> {
        self.rx.take().expect("take_events called once")
    }

    pub async fn recv(&mut self) -> Option<DisplayEvent> {
        match self.rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn display_worker_slow_ensure_does_not_block_runtime_ticks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_spawner = calls.clone();
        let (pty_tx, _pty_rx) = tokio::sync::mpsc::unbounded_channel::<PtyEvent>();
        let mut worker = DisplayWorker::with_spawner(
            pty_tx,
            Box::new(move |_argv, _cols, _rows, id, _events, _env_clear| {
                calls_for_spawner.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                Ok(crate::display::attachment::fake_attachment(id))
            }),
        );

        worker.ensure(DisplayEnsure {
            seq: 7,
            key: "local/slow".to_string(),
            argv: vec!["cmd.exe".to_string(), "/c".to_string(), "rem".to_string()],
            cols: 80,
            rows: 24,
            id: 42,
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

        match event {
            DisplayEvent::Ready {
                seq,
                key,
                attachment,
            } => {
                assert_eq!(seq, 7);
                assert_eq!(key, "local/slow");
                assert_eq!(
                    attachment.id(),
                    42,
                    "the attachment carries the app-issued id"
                );
            }
            DisplayEvent::Failed { .. } => panic!("expected Ready"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
