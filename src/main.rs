//! The `xmux` binary: a thin shim over `xmux::cli::run`. A multi-thread runtime
//! runs the spawned discovery tasks (roster resolve, reachability probes, mux
//! discovery, detection) on a worker-thread pool, isolating them from the rendering
//! loop on the main thread. Blocking work runs on the blocking pool and on dedicated
//! OS threads (PTY pumps, stdin), so the workers only drive I/O and two of them serve
//! any number of cores without one thread per core.

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    std::process::exit(xmux::cli::run().await);
}
