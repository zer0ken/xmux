//! Command `xmux` is a stateless cross-environment session switcher: one terminal
//! that sees and moves between every reachable tmux/psmux session — local and over
//! ssh — regardless of OS or mux kind.

fn main() {
    // Wiring (clap subcommands, the switcher, attach) is ported incrementally.
    // Until the CLI layer lands, the binary is a placeholder over the library.
    println!("xmux {}", env!("CARGO_PKG_VERSION"));
}
