//! SSH authentication diagnostics kept outside pipes inherited by a shared master.

pub(crate) struct AuthLog(std::path::PathBuf);

impl AuthLog {
    pub(crate) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "xmux-ssh-auth-{stamp}-{}-{sequence}.log",
            std::process::id()
        )))
    }

    pub(crate) fn args(&self) -> [String; 3] {
        [
            "-v".into(),
            "-E".into(),
            self.0.to_string_lossy().into_owned(),
        ]
    }

    /// Read on a worker thread, never on the application runtime.
    pub(crate) fn read(&self) -> String {
        std::fs::read_to_string(&self.0).unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for AuthLog {
    fn drop(&mut self) {
        let path = self.0.clone();
        std::thread::spawn(move || {
            let _ = std::fs::remove_file(path);
        });
    }
}
