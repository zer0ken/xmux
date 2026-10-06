//! The ssh option that makes OpenSSH report which method authenticated a connection.

/// `VERBOSE` is the lowest level at which OpenSSH reports the method, and the report
/// stays on the command's own stderr beside ssh's errors. A ControlPersist master keeps
/// the stderr it inherited only at a debug level, so a probe's pipes still close when
/// the probe exits, and no file outlives the command that writes it. A command that
/// rides an existing master authenticates nothing, so it may report no method.
pub(crate) fn args() -> [String; 2] {
    ["-o".into(), "LogLevel=VERBOSE".into()]
}
