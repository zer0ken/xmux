//! The `xmux` binary: a thin shim over `xmux::cli::run`. The single-threaded
//! runtime is deliberate — every blocking I/O runs on a dedicated OS thread, so
//! the async loop never blocks; a multi-thread runtime would only add scheduler
//! overhead and Send bounds for no gain.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::process::exit(xmux::cli::run().await);
}
