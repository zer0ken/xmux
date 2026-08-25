//! The `xmux update` command: detects how xmux was installed from the running
//! executable's path, delegates to the owning package manager (cargo, winget,
//! Homebrew) when one can be identified, and otherwise replaces the binary in
//! place with a checksum-verified build from the latest GitHub release.

pub struct Args {
    pub check: bool,
    pub method: Option<String>,
}

pub async fn run(_args: Args) -> i32 {
    eprintln!("xmux update: not implemented");
    1
}
