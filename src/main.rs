//! The `xmux` binary: a thin shim over `xmux::cli::run`. A multi-thread runtime
//! runs the spawned discovery tasks (roster resolve, reachability probes, mux
//! discovery, detection) on a real worker-thread pool, giving the scan genuine
//! thread-level concurrency and isolating it from the rendering loop on the main
//! thread. Blocking I/O still runs on dedicated OS threads (PTY pumps, stdin), so
//! the loop itself never blocks.

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    std::process::exit(xmux::cli::run().await);
}
