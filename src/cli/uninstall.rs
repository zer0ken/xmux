//! The `xmux uninstall` command. It reads how xmux was installed the same way
//! `xmux update` does, from the running executable's path, and removes it the way
//! that install method placed it: an install the script placed loses its version
//! directories, its launcher and marker, and the `PATH` change the script made; a
//! cargo, winget, or Homebrew install is handed to its package manager; and a binary
//! the user copied is deleted.
//!
//! Nothing is removed without a yes to `Remove xmux? (y/N)`, and the settings and
//! data directories are a second question whose default keeps them. The command
//! refuses while an xmux instance is running, because deleting the files under a
//! live instance would leave it running on a removed install. It asks again right
//! before each removal, so an instance started while a question waited is not run
//! over.
//!
//! The command reaches no other machine. A key this PC registered on a host stays
//! there, and the app's logout removes it one host at a time.
//!
//! On Windows a running process cannot delete its own image, so the removal of files
//! (and a package manager's uninstall, which deletes the running image) is handed to
//! the same detached helper `xmux update` uses, which waits for every xmux process
//! to exit first and lists what it could not remove in a log. The user `PATH` is
//! edited before this process exits, because a registry value is not locked by a
//! running image.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use super::update::{self, InstallMethod, Platform};

pub struct Args {
    /// Answers yes to `Remove xmux?`.
    pub yes: bool,
    /// Answers yes to `Also remove settings and data?`.
    pub purge: bool,
}

/// The lines `install.sh` writes around the `PATH` line it appends to a shell
/// profile. Everything between them, and the two lines themselves, are the
/// script's own; nothing outside them is.
const BLOCK_BEGIN: &str = "# >>> xmux installer >>>";
const BLOCK_END: &str = "# <<< xmux installer <<<";

/// The file `install.ps1` writes in the install root when it appends the launcher
/// directory to the user `PATH`, holding that exact entry. Without it there is no
/// telling the script's entry from one the user added, so the entry stays.
const PATH_RECORD: &str = "path-added";

/// The first line of the `PATH` record. A file of that name without it is not the
/// script's, so it is neither trusted nor deleted.
const PATH_RECORD_HEADER: &str = "# xmux installer: PATH entry added";

/// Every profile `install.sh` may have chosen. It picks one by the login shell at
/// install time, and the shell may have changed since, so each is read and only a
/// file holding the marked block is edited.
const PROFILES: &[&str] = &[
    ".profile",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".config/fish/config.fish",
];

/// What removing the program consists of, per install method.
#[derive(Debug, PartialEq, Eq)]
enum Program {
    Script(Box<ScriptInstall>),
    /// A package manager owns the install, so it is the one that removes it.
    Delegate {
        program: &'static str,
        args: &'static [&'static str],
    },
    /// A binary the user placed: the executable and any sidecar an update left
    /// beside it.
    Binary {
        files: Vec<PathBuf>,
    },
}

/// The paths an install the script placed owns. Each is named explicitly, so the
/// removal deletes these and nothing else: an install root the user pointed at a
/// directory of their own keeps every file the script did not write.
#[derive(Debug, PartialEq, Eq)]
struct ScriptInstall {
    root: PathBuf,
    /// `<root>/versions`.
    versions: PathBuf,
    /// The version directories the script wrote, each removed with its contents:
    /// a version-named directory holding the xmux binary. Anything else under
    /// `versions` is not the script's.
    version_dirs: Vec<PathBuf>,
    /// Launchers, the markers beside them, the sidecars an update renamed aside,
    /// and the `PATH` record.
    files: Vec<PathBuf>,
    /// Directories removed only when nothing is left in them: `versions`,
    /// `<root>/bin`, and `<root>`.
    dirs_if_empty: Vec<PathBuf>,
    /// Files named like a launcher inside the root that nothing proves the script
    /// placed. They stay, and the plan says so.
    kept: Vec<PathBuf>,
    /// Shell profiles holding a block `install.sh` appended for this install (unix).
    profiles: Vec<PathBuf>,
    /// The launcher directories, as written. Only a profile block naming one of them
    /// exactly is this install's.
    block_dirs: Vec<String>,
    /// User `PATH` entries the script recorded appending (Windows).
    user_path_entries: Vec<String>,
    /// Launcher directories on the user `PATH` with no record that the script added
    /// them (Windows). They stay, and the plan names them for the user to remove.
    path_left: Vec<PathBuf>,
}

struct Plan {
    method: InstallMethod,
    program: Program,
}

/// What the plan reads from outside the executable's own path. A parameter so the
/// plan can be built against temporary directories in tests.
struct Surroundings<'a> {
    home: Option<&'a Path>,
    /// The directories on `PATH`, searched for launchers the script placed.
    path_dirs: &'a [PathBuf],
    /// `XMUX_BIN_DIR`, the launcher directory the script was told to use.
    bin_dir: Option<&'a Path>,
    /// The user `PATH` value from the registry (Windows).
    user_path: Option<&'a str>,
}

fn launcher_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "xmux.exe",
        Platform::Unix => "xmux",
    }
}

fn plan(
    exe: &Path,
    method: InstallMethod,
    platform: Platform,
    s: &Surroundings,
) -> Result<Plan, String> {
    let program = match method {
        InstallMethod::Script => Program::Script(Box::new(script_install(exe, platform, s)?)),
        InstallMethod::Cargo => Program::Delegate {
            program: "cargo",
            args: &["uninstall", "xmux"],
        },
        InstallMethod::Winget => Program::Delegate {
            program: "winget",
            args: &["uninstall", "--id", "zer0ken.xmux"],
        },
        InstallMethod::Brew => Program::Delegate {
            program: "brew",
            args: &["uninstall", "xmux"],
        },
        InstallMethod::Self_ => {
            let mut files = vec![exe.to_path_buf()];
            files.extend(sidecars_of(exe)?);
            Program::Binary { files }
        }
    };
    Ok(Plan { method, program })
}

/// Comparable form of a path: resolved when it exists, with the Windows verbatim
/// prefix dropped, and case-folded on Windows where the file system ignores case.
fn canon(p: &Path) -> PathBuf {
    let resolved = update::plain(&p.canonicalize().unwrap_or_else(|_| p.to_path_buf()));
    if cfg!(windows) {
        PathBuf::from(resolved.to_string_lossy().to_lowercase())
    } else {
        resolved
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    canon(a) == canon(b)
}

fn marker_of(launcher: &Path) -> PathBuf {
    let name = launcher
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    launcher.with_file_name(format!("{name}{}", update::INSTALL_MARKER_SUFFIX))
}

/// Whether `candidate` is a launcher the script placed for `root`, proven by one of
/// three things: its marker names that root, it is a symlink resolving into the
/// root's versions, or it sits in `<root>/bin` as a copy byte for byte of the
/// binary in one of the version directories being removed. A copy elsewhere with no
/// marker may be one the user made, and a file proven by none of them belongs to
/// something else; both are left alone.
fn is_launcher_of(candidate: &Path, root: &Path, versions: &Path, binaries: &[PathBuf]) -> bool {
    let Ok(meta) = candidate.symlink_metadata() else {
        return false;
    };
    if meta.is_dir() {
        return false;
    }
    let versions = canon(versions);
    if candidate
        .parent()
        .is_some_and(|d| canon(d).starts_with(&versions))
    {
        // The executable inside a version directory, removed with that directory.
        return false;
    }
    if marker_names(candidate, root) {
        return true;
    }
    if meta.file_type().is_symlink() {
        return canon(candidate).starts_with(&versions);
    }
    meta.is_file()
        && candidate
            .parent()
            .is_some_and(|d| same_path(d, &root.join("bin")))
        && binaries.iter().any(|b| same_bytes(candidate, b))
}

/// Whether the marker beside `launcher` names `root`.
fn marker_names(launcher: &Path, root: &Path) -> bool {
    let marker = marker_of(launcher);
    marker.is_file()
        && std::fs::read_to_string(&marker)
            .is_ok_and(|t| same_path(Path::new(t.trim_start_matches('\u{feff}').trim()), root))
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

/// Whether `name` reads as a version the script installs under: `x.y.z`, with an
/// optional pre-release or build suffix.
fn is_version_name(name: &str) -> bool {
    let (core, suffix) = match name.find(['-', '+']) {
        Some(i) => (&name[..i], Some(&name[i + 1..])),
        None => (name, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && suffix.is_none_or(|x| {
            !x.is_empty()
                && x.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        })
}

/// The version directories the script wrote under `versions`: real directories with
/// a version name that hold the xmux binary as a regular file. A `versions` it
/// cannot list is an error, never an empty answer, because an empty answer would
/// leave every version installed while the removal reported success.
fn version_dirs_in(versions: &Path, binary: &str) -> Result<Vec<PathBuf>, String> {
    let cannot = |e: std::io::Error| format!("cannot list {}: {e}", versions.display());
    let entries = match std::fs::read_dir(versions) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(cannot(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(cannot)?;
        if !entry.file_type().map_err(cannot)?.is_dir()
            || !is_version_name(&entry.file_name().to_string_lossy())
        {
            continue;
        }
        let bin = entry.path().join(binary);
        match bin.symlink_metadata() {
            Ok(m) if m.is_file() => found.push(entry.path()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read {}: {e}", bin.display())),
        }
    }
    found.sort();
    Ok(found)
}

/// The sidecars an update renamed aside next to `exe` (`<name>.old-<pid>`). A
/// directory it cannot list is an error, for the same reason as the versions.
fn sidecars_of(exe: &Path) -> Result<Vec<PathBuf>, String> {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return Ok(Vec::new());
    };
    let name = name.to_string_lossy();
    let cannot = |e: std::io::Error| format!("cannot list {}: {e}", dir.display());
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(cannot(e)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(cannot)?;
        if entry.file_type().map_err(cannot)?.is_file()
            && update::is_stale_sidecar(&name, &entry.file_name().to_string_lossy())
        {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

fn script_install(
    exe: &Path,
    platform: Platform,
    s: &Surroundings,
) -> Result<ScriptInstall, String> {
    let root = update::script_root(exe)
        .ok_or_else(|| format!("cannot find the install root of {}", exe.display()))?;
    let versions = root.join("versions");
    let name = launcher_name(platform);
    let version_dirs = version_dirs_in(&versions, name)?;
    let binaries: Vec<PathBuf> = version_dirs.iter().map(|d| d.join(name)).collect();

    let in_root = root.join("bin").join(name);
    let mut candidates = vec![exe.to_path_buf(), in_root.clone()];
    if let Some(b) = s.bin_dir {
        candidates.push(b.join(name));
    }
    if let (Platform::Unix, Some(h)) = (platform, s.home) {
        candidates.push(h.join(".local").join("bin").join(name));
    }
    candidates.extend(s.path_dirs.iter().map(|d| d.join(name)));

    let mut launchers: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if !launchers.iter().any(|l| same_path(l, &c))
            && is_launcher_of(&c, &root, &versions, &binaries)
        {
            launchers.push(c);
        }
    }
    let mut kept = Vec::new();
    if in_root.symlink_metadata().is_ok() && !launchers.iter().any(|l| same_path(l, &in_root)) {
        kept.push(in_root);
    }

    let mut files = Vec::new();
    for l in &launchers {
        files.push(l.clone());
        if marker_names(l, &root) {
            files.push(marker_of(l));
        }
        files.extend(sidecars_of(l)?);
    }

    let block_dirs: Vec<String> = launchers
        .iter()
        .filter_map(|l| l.parent())
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    let mut profiles = Vec::new();
    if let (Platform::Unix, Some(h)) = (platform, s.home) {
        for p in PROFILES {
            let path = h.join(p);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            };
            if remove_marked_block(&text, &block_dirs).is_some() {
                profiles.push(path);
            }
        }
    }

    // The script appends the launcher directory to the user PATH only when it is not
    // there already, and records the entry when it does. Only a recorded entry is
    // the script's; any other may be one the user added, so it stays and is named.
    let mut user_path_entries: Vec<String> = Vec::new();
    let mut path_left: Vec<PathBuf> = Vec::new();
    if platform == Platform::Windows {
        let record_file = root.join(PATH_RECORD);
        let recorded = if record_file.is_file() {
            std::fs::read_to_string(&record_file)
                .ok()
                .and_then(|t| parse_path_record(&t))
        } else {
            None
        };
        if recorded.is_some() {
            files.push(record_file);
        }
        for l in &launchers {
            let Some(dir) = l.parent() else { continue };
            let on_path = s.user_path.is_some_and(|v| path_has_dir(v, dir));
            if !on_path {
                continue;
            }
            match &recorded {
                Some(entry)
                    if same_entry(entry, &dir.to_string_lossy())
                        && s.user_path
                            .is_some_and(|v| remove_recorded_entry(v, entry).is_some()) =>
                {
                    if !user_path_entries.contains(entry) {
                        user_path_entries.push(entry.clone());
                    }
                }
                _ => {
                    if !path_left.iter().any(|d| same_path(d, dir)) {
                        path_left.push(dir.to_path_buf());
                    }
                }
            }
        }
    }

    Ok(ScriptInstall {
        dirs_if_empty: vec![versions.clone(), root.join("bin"), root.clone()],
        root,
        versions,
        version_dirs,
        files,
        kept,
        profiles,
        block_dirs,
        user_path_entries,
        path_left,
    })
}

/// `text` without the blocks `install.sh` appended for one of `dirs`, or `None`
/// when it holds no such block. A block is this install's only when its one line
/// adds a directory in `dirs`, written exactly; a block for another install, a
/// block the user edited, and a begin line with no end line are all left as they
/// are, because removing them could take away the user's own `PATH`.
fn remove_marked_block(text: &str, dirs: &[String]) -> Option<String> {
    let mut out = text.to_string();
    let mut changed = false;
    let mut from = 0;
    while let Some((range, body)) = find_block(&out, from) {
        if block_dir(&out[body]).is_some_and(|d| dirs.iter().any(|x| *x == d)) {
            from = range.start;
            out.replace_range(range, "");
            changed = true;
        } else {
            from = range.end;
        }
    }
    changed.then_some(out)
}

/// The directory a block's body adds to `PATH`, in either form `install.sh`
/// writes, or `None` for any other body.
fn block_dir(body: &str) -> Option<&str> {
    let mut lines = body.lines().map(|l| l.trim_end_matches('\r'));
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    line.strip_prefix("export PATH=\"")
        .and_then(|r| r.strip_suffix(":$PATH\""))
        .or_else(|| {
            line.strip_prefix("fish_add_path \"")
                .and_then(|r| r.strip_suffix('"'))
        })
}

/// The first complete block at or after byte `from`: its byte range, including the
/// line break the script wrote before it, and the range of the lines between its
/// two markers. The script writes `\n` and then the block, so that break is taken
/// back when it is a blank line of its own or when the block ends the file; in the
/// middle of a file whose blank line was deleted, it is the break that ends the
/// user's previous line and stays.
fn find_block(text: &str, from: usize) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let mut begin: Option<(usize, usize)> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let line_start = pos;
        pos += line.len();
        if line_start < from {
            continue;
        }
        let content = line.trim_end_matches('\n').trim_end_matches('\r');
        match begin {
            None if content == BLOCK_BEGIN => begin = Some((line_start, pos)),
            Some((b, body_start)) if content == BLOCK_END => {
                let end = pos;
                let bytes = text.as_bytes();
                let blank_before = b >= 2 && bytes[b - 1] == b'\n' && bytes[b - 2] == b'\n';
                let at_start_blank = b == 1 && bytes[0] == b'\n';
                let at_eof = end == text.len() && b >= 1 && bytes[b - 1] == b'\n';
                let start = if blank_before || at_start_blank || at_eof {
                    b - 1
                } else {
                    b
                };
                return Some((start..end, body_start..line_start));
            }
            _ => {}
        }
    }
    None
}

/// The entry a `PATH` record names, when the file carries the script's header.
fn parse_path_record(text: &str) -> Option<String> {
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next()?.trim_end() != PATH_RECORD_HEADER {
        return None;
    }
    let entry = lines.next()?.trim_end_matches('\r');
    (!entry.is_empty()).then(|| entry.to_string())
}

/// Whether `value` (a `;`-separated Windows `PATH`) has an entry naming `dir`,
/// compared as `install.ps1` compares them: case-insensitively, ignoring a trailing
/// separator. Only used to name an entry the user may remove, never to remove one.
fn path_has_dir(value: &str, dir: &Path) -> bool {
    let want = dir.to_string_lossy();
    value
        .split(';')
        .any(|e| !e.is_empty() && same_entry(e, &want))
}

/// Whether two `PATH` entries name the same directory, compared as `install.ps1`
/// compares them.
fn same_entry(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

/// `value` without the entry the script appended, or `None` when it has none. The
/// script appended `entry` exactly as recorded, so only an entry equal to it
/// (ignoring case, as Windows does) counts, and only the last one, which is the one
/// it appended. Every other entry is kept exactly as written.
fn remove_recorded_entry(value: &str, entry: &str) -> Option<String> {
    let mut parts: Vec<&str> = value.split(';').collect();
    let want = entry.to_lowercase();
    let at = parts.iter().rposition(|e| e.to_lowercase() == want)?;
    parts.remove(at);
    Some(parts.join(";"))
}

/// The lines that tell the user what the first question would remove.
fn describe(plan: &Plan) -> Vec<String> {
    let mut lines = vec![format!("install: {}", plan.method.label())];
    match &plan.program {
        Program::Script(s) => {
            lines.push(format!("install root: {}", s.root.display()));
            for d in &s.version_dirs {
                lines.push(format!("remove: {}", d.display()));
            }
            for f in &s.files {
                lines.push(format!("remove: {}", f.display()));
            }
            for d in &s.dirs_if_empty {
                lines.push(format!("remove if empty: {}", d.display()));
            }
            for p in &s.profiles {
                lines.push(format!(
                    "PATH: remove the xmux installer block from {}",
                    p.display()
                ));
            }
            for k in &s.kept {
                lines.push(format!(
                    "keep: {} (nothing shows this install placed it)",
                    k.display()
                ));
            }
            for e in &s.user_path_entries {
                lines.push(format!("PATH: remove {e} from the user PATH"));
            }
            for d in &s.path_left {
                lines.push(format!(
                    "PATH: {} stays on the user PATH (the installer left no record of adding it); \
                     remove it yourself if nothing else needs it",
                    d.display()
                ));
            }
        }
        Program::Delegate { program, args } => {
            lines.push(format!("run: {program} {}", args.join(" ")));
        }
        Program::Binary { files } => {
            for f in files {
                lines.push(format!("remove: {}", f.display()));
            }
        }
    }
    lines
}

/// How a question is answered: by a flag, by the person at the terminal, or by
/// default when nobody is there to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Yes,
    No,
    Ask,
}

fn remove_answer(args: &Args, interactive: bool) -> Answer {
    if args.yes {
        Answer::Yes
    } else if interactive {
        Answer::Ask
    } else {
        Answer::No
    }
}

/// `--yes` answers only the first question, so with it the settings stay unless
/// `--purge` is also given.
fn purge_answer(args: &Args, interactive: bool) -> Answer {
    if args.purge {
        Answer::Yes
    } else if args.yes || !interactive {
        Answer::No
    } else {
        Answer::Ask
    }
}

/// Only `y` or `yes`, in any case, is a yes. An empty line, anything else, and end
/// of input are a no.
fn is_yes(line: Option<&str>) -> bool {
    line.map(|l| l.trim().to_ascii_lowercase())
        .is_some_and(|l| l == "y" || l == "yes")
}

/// Prints `question` and reads one line of answer.
fn ask(question: &str, input: &mut dyn BufRead, out: &mut dyn Write) -> bool {
    let _ = write!(out, "{question} (y/N) ");
    let _ = out.flush();
    let mut line = String::new();
    let read = match input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.as_str()),
    };
    let yes = is_yes(read);
    if read.is_none() {
        let _ = writeln!(out);
    }
    yes
}

fn resolve(answer: Answer, question: &str, input: &mut dyn BufRead, out: &mut dyn Write) -> bool {
    match answer {
        Answer::Yes => true,
        Answer::No => false,
        Answer::Ask => ask(question, input, out),
    }
}

/// Removes an install the script placed, from the paths the plan named. A path
/// that is already gone is not an error, so a second run finishes what a first one
/// left.
fn remove_script_install(s: &ScriptInstall) -> Result<(), String> {
    let mut errors = Vec::new();
    for f in &s.files {
        if let Err(e) = remove_file(f) {
            errors.push(e);
        }
    }
    for d in &s.version_dirs {
        if let Err(e) = remove_tree(d) {
            errors.push(e);
        }
    }
    for d in &s.dirs_if_empty {
        let _ = std::fs::remove_dir(d);
    }
    for p in &s.profiles {
        if let Err(e) = remove_block_from(p, &s.block_dirs) {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

fn remove_tree(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// Rewrites `profile` without this install's block. Writing through the path keeps
/// a profile that is a symlink a symlink, and keeps its permissions.
fn remove_block_from(profile: &Path, dirs: &[String]) -> Result<(), String> {
    let text = match std::fs::read_to_string(profile) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read {}: {e}", profile.display())),
    };
    match remove_marked_block(&text, dirs) {
        Some(updated) => std::fs::write(profile, updated)
            .map_err(|e| format!("cannot write {}: {e}", profile.display())),
        None => Ok(()),
    }
}

/// The two places xmux keeps settings and data: the state directory and the
/// directory holding the config file.
fn data_dirs() -> Vec<PathBuf> {
    let mut v = vec![crate::provision::env::xmux_dir_path()];
    if let Some(cfg_dir) = crate::provision::env::config_path().parent() {
        v.push(cfg_dir.to_path_buf());
    }
    v
}

fn remove_data(dirs: &[PathBuf]) -> Result<(), String> {
    let errors: Vec<String> = dirs.iter().filter_map(|d| remove_tree(d).err()).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Quotes a path for a cmd script line. `%` is doubled because cmd expands it even
/// inside double quotes.
fn cmd_path(p: &Path) -> String {
    format!("\"{}\"", p.display().to_string().replace('%', "%%"))
}

/// The detached helper that finishes a Windows uninstall once every xmux process
/// has exited: it deletes the files a running image keeps locked, runs a package
/// manager's uninstall, and removes the settings and data when they were asked for.
///
/// The script is UTF-8 and switches the console to that code page before the first
/// path, so a path outside the ANSI code page is read as written. Every path that
/// is still there afterwards, and a package manager's output, go to `log`, which
/// sits outside the helper's own directory and ends with a line saying the helper
/// finished.
fn windows_helper_script(
    program: &Program,
    purge: &[PathBuf],
    log: &Path,
    own_dir: &Path,
) -> String {
    let log_q = cmd_path(log);
    let mut s = String::from(update::UPDATER_WAIT_PREAMBLE);
    s.push_str("chcp 65001 >nul\r\n");
    let del = |s: &mut String, f: &Path| {
        let q = cmd_path(f);
        s.push_str(&format!("del /F /Q {q} >nul 2>&1\r\n"));
        s.push_str(&format!("if exist {q} echo not removed: {q}>>{log_q}\r\n"));
    };
    let tree = |s: &mut String, d: &Path| {
        let q = cmd_path(d);
        s.push_str(&format!("rmdir /S /Q {q} >nul 2>&1\r\n"));
        s.push_str(&format!("if exist {q} echo not removed: {q}>>{log_q}\r\n"));
    };
    match program {
        Program::Script(si) => {
            for f in &si.files {
                del(&mut s, f);
            }
            for d in &si.version_dirs {
                tree(&mut s, d);
            }
            for d in &si.dirs_if_empty {
                s.push_str(&format!("rmdir {} >nul 2>&1\r\n", cmd_path(d)));
            }
        }
        Program::Delegate { program, args } => {
            let cmd = format!("{program} {}", args.join(" "));
            s.push_str(&format!(
                "{cmd} >>{log_q} 2>&1 || echo failed: {cmd}>>{log_q}\r\n"
            ));
        }
        Program::Binary { files } => {
            for f in files {
                del(&mut s, f);
            }
        }
    }
    for d in purge {
        tree(&mut s, d);
    }
    s.push_str(&format!("echo xmux uninstall finished>>{log_q}\r\n"));
    s.push_str(&format!("rmdir /S /Q {} >nul 2>&1\r\n", cmd_path(own_dir)));
    s
}

/// Reads the user `PATH` exactly as stored, without expanding the variables in it,
/// so writing it back changes only the entry removed.
#[cfg(windows)]
fn read_user_path() -> Option<String> {
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
        $k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment'); \
        if ($k) { [Console]::Out.Write([string]$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)) }";
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(windows))]
fn read_user_path() -> Option<String> {
    None
}

/// Writes the user `PATH` back with its registry value kind unchanged. The value
/// rides in the environment rather than in the command line, so no `PATH` entry
/// becomes part of a PowerShell expression. Deleting a variable that does not exist
/// through the environment API is what announces the change to Explorer, so a
/// terminal opened afterwards reads the new `PATH`.
#[cfg(windows)]
fn write_user_path(value: &str) -> Result<(), String> {
    let script = "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true); \
        $kind = try { $k.GetValueKind('Path') } catch { [Microsoft.Win32.RegistryValueKind]::ExpandString }; \
        $k.SetValue('Path',$env:XMUX_USER_PATH,$kind); \
        [Environment]::SetEnvironmentVariable('XMUX_UNINSTALL_NOTIFY',$null,'User')";
    let status = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("XMUX_USER_PATH", value)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("cannot run powershell: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "powershell exited with {status} while writing the user PATH"
        ))
    }
}

#[cfg(not(windows))]
fn write_user_path(_value: &str) -> Result<(), String> {
    unreachable!("the user PATH is written only on Windows")
}

/// Drops `entries` from the user `PATH`, reading it again so an edit made since the
/// plan was printed is kept.
fn remove_from_user_path(entries: &[String]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut value = read_user_path().ok_or("cannot read the user PATH")?;
    let mut changed = false;
    for e in entries {
        if let Some(v) = remove_recorded_entry(&value, e) {
            value = v;
            changed = true;
        }
    }
    if changed {
        write_user_path(&value)?;
    }
    Ok(())
}

/// The outcome the exit code reports: removed, or declined with nothing changed.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Removed,
    Declined,
}

/// The message refusing to go on while instances run, or `None` when none does.
fn running_error(live: &[(PathBuf, String)]) -> Option<String> {
    let names: Vec<&str> = live.iter().map(|(_, n)| n.as_str()).collect();
    let first = names.first()?;
    Some(format!(
        "xmux is running ({}); quit it first, e.g. `xmux send {first} quit`",
        names.join(", ")
    ))
}

/// The removal steps `confirm_and_remove` runs, each supplied by the caller.
struct Steps<'a> {
    /// Asks whether an instance is running. Runs right before each removal, so one
    /// started while a question waited stops the command before it deletes
    /// anything under it.
    guard: &'a mut dyn FnMut() -> Result<(), String>,
    /// Removes the program.
    remove_program: &'a mut dyn FnMut(&mut dyn Write) -> Result<(), String>,
    /// Removes the settings and data when its argument is true, and completes what
    /// the platform defers.
    finish: &'a mut dyn FnMut(bool, &mut dyn Write) -> Result<(), String>,
}

/// The two questions and the removals they allow, in order.
fn confirm_and_remove(
    remove: Answer,
    purge: Answer,
    data: &[PathBuf],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    steps: Steps,
) -> Result<Outcome, String> {
    if !resolve(remove, "Remove xmux?", input, out) {
        let _ = writeln!(out, "nothing was removed");
        return Ok(Outcome::Declined);
    }
    (steps.guard)()?;
    (steps.remove_program)(out)?;

    let _ = writeln!(out, "settings and data:");
    for d in data {
        let _ = writeln!(out, "  {}", d.display());
    }
    let mut purge = resolve(purge, "Also remove settings and data?", input, out);
    // The program removal was confirmed and has begun (on Windows the user PATH is
    // already changed), so an instance found here stops only the purge: the program
    // removal still completes, and only the settings and data stay.
    let mut blocked = None;
    if purge {
        if let Err(e) = (steps.guard)() {
            blocked = Some(e);
            purge = false;
        }
    }
    (steps.finish)(purge, out)?;
    if !purge {
        let _ = writeln!(out, "kept settings and data:");
        for d in data.iter().filter(|d| d.exists()) {
            let _ = writeln!(out, "  {}", d.display());
        }
    }
    match blocked {
        Some(e) => Err(format!(
            "the program removal went ahead, but settings and data were kept because {e}"
        )),
        None => Ok(Outcome::Removed),
    }
}

fn run_blocking(args: &Args, handle: tokio::runtime::Handle) -> Result<Outcome, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    let platform = update::platform();
    let method = update::classify(&exe, &update::cargo_bins(), platform);

    let home = crate::provision::env::resolved_home();
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let bin_dir = std::env::var_os("XMUX_BIN_DIR").map(PathBuf::from);
    let user_path = match (platform, method) {
        (Platform::Windows, InstallMethod::Script) => read_user_path(),
        _ => None,
    };
    let plan = plan(
        &exe,
        method,
        platform,
        &Surroundings {
            home: home.as_deref(),
            path_dirs: &path_dirs,
            bin_dir: bin_dir.as_deref(),
            user_path: user_path.as_deref(),
        },
    )?;
    if let Program::Delegate { program, .. } = &plan.program {
        if !update::tool_on_path(program) {
            return Err(format!(
                "xmux is installed via {program}, but {program} is not on PATH"
            ));
        }
    }

    for line in describe(&plan) {
        println!("{line}");
    }
    println!(
        "Remote hosts are not touched: a key this PC registered on a host stays there. \
         `prefix L` in xmux removes it, one host at a time."
    );

    let interactive = std::io::stdin().is_terminal();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let xmux_dir = crate::provision::env::xmux_dir_path();
    let data = data_dirs();

    let mut guard = || match running_error(&handle.block_on(super::live_instances(&xmux_dir))) {
        Some(e) => Err(e),
        None => Ok(()),
    };
    let mut remove_program = |out: &mut dyn Write| -> Result<(), String> {
        match (platform, &plan.program) {
            (Platform::Windows, Program::Script(s)) => remove_from_user_path(&s.user_path_entries),
            (Platform::Windows, _) => Ok(()),
            (Platform::Unix, Program::Script(s)) => {
                remove_script_install(s)?;
                let _ = writeln!(out, "xmux is removed");
                Ok(())
            }
            (Platform::Unix, Program::Delegate { program, args }) => {
                update::run_delegated(program, args)?;
                let _ = writeln!(out, "xmux is removed");
                Ok(())
            }
            (Platform::Unix, Program::Binary { files }) => {
                for f in files {
                    remove_file(f)?;
                }
                let _ = writeln!(out, "xmux is removed");
                Ok(())
            }
        }
    };
    let mut finish = |purge: bool, out: &mut dyn Write| -> Result<(), String> {
        match platform {
            Platform::Unix => {
                if purge {
                    remove_data(&data)?;
                    let _ = writeln!(out, "settings and data removed");
                }
                Ok(())
            }
            Platform::Windows => {
                let temp = std::env::temp_dir();
                let dir = temp.join(format!("xmux-uninstall-{}", std::process::id()));
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("cannot create helper dir {}: {e}", dir.display()))?;
                let log = temp.join(format!("xmux-uninstall-{}.log", std::process::id()));
                let purged: &[PathBuf] = if purge { &data } else { &[] };
                update::spawn_detached_cmd(
                    &dir,
                    windows_helper_script(&plan.program, purged, &log, &dir),
                )?;
                let _ = writeln!(
                    out,
                    "program files are removed once every xmux process exits, including this one; \
                     it writes what it could not remove, and a line when it finishes, to {}",
                    log.display()
                );
                Ok(())
            }
        }
    };
    confirm_and_remove(
        remove_answer(args, interactive),
        purge_answer(args, interactive),
        &data,
        &mut input,
        &mut out,
        Steps {
            guard: &mut guard,
            remove_program: &mut remove_program,
            finish: &mut finish,
        },
    )
}

/// The public entry. It refuses while an xmux instance is running, then runs the
/// blocking removal on a worker thread so no process or file work sits on the
/// async runtime path. A declined removal exits non-zero, so a script can tell it
/// from a removal.
pub async fn run(args: Args) -> i32 {
    let live = super::live_instances(&crate::provision::env::xmux_dir_path()).await;
    if let Some(e) = running_error(&live) {
        eprintln!("xmux uninstall: {e}");
        return 1;
    }
    let handle = tokio::runtime::Handle::current();
    match tokio::task::spawn_blocking(move || run_blocking(&args, handle)).await {
        Ok(Ok(Outcome::Removed)) => 0,
        Ok(Ok(Outcome::Declined)) => 1,
        Ok(Err(e)) => {
            eprintln!("xmux uninstall: {e}");
            1
        }
        Err(_) => {
            eprintln!("xmux uninstall: internal panic");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp directory, named for one test.
    /// A fresh temp directory, spelled as it resolves. A temp directory can be reached
    /// through an 8.3 short name or a symlink, and the plan names paths as they
    /// resolve, so the tests build their expectations from the same spelling.
    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xmux-uninstall-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        update::plain(&dir.canonicalize().unwrap())
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn none() -> Surroundings<'static> {
        Surroundings {
            home: None,
            path_dirs: &[],
            bin_dir: None,
            user_path: None,
        }
    }

    #[test]
    fn only_y_and_yes_are_yes() {
        for yes in ["y", "Y", "yes", "YES", "Yes", " y \n", "yes\r\n"] {
            assert!(is_yes(Some(yes)), "{yes:?}");
        }
        for no in ["", "\n", "n", "N", "no", "ye", "yess", "sure", "y y"] {
            assert!(!is_yes(Some(no)), "{no:?}");
        }
        assert!(!is_yes(None), "end of input is a no");
    }

    #[test]
    fn ask_reads_one_line_and_defaults_to_no() {
        let mut out = Vec::new();
        assert!(ask("Remove xmux?", &mut "yes\n".as_bytes(), &mut out));
        assert_eq!(String::from_utf8(out).unwrap(), "Remove xmux? (y/N) ");
        let mut out = Vec::new();
        assert!(
            !ask("Remove xmux?", &mut "\n".as_bytes(), &mut out),
            "Enter"
        );
        let mut out = Vec::new();
        assert!(!ask("Remove xmux?", &mut "".as_bytes(), &mut out), "EOF");
        let mut out = Vec::new();
        assert!(!ask("Remove xmux?", &mut "N\n".as_bytes(), &mut out));
    }

    #[test]
    fn yes_answers_only_the_first_question() {
        let a = |yes, purge| Args { yes, purge };
        // Nobody at the terminal and no flag: nothing is removed.
        assert_eq!(remove_answer(&a(false, false), false), Answer::No);
        assert_eq!(remove_answer(&a(false, false), true), Answer::Ask);
        assert_eq!(remove_answer(&a(true, false), false), Answer::Yes);
        // `--yes` keeps settings unless `--purge` is also given, even at a terminal.
        assert_eq!(purge_answer(&a(true, false), true), Answer::No);
        assert_eq!(purge_answer(&a(true, false), false), Answer::No);
        assert_eq!(purge_answer(&a(true, true), false), Answer::Yes);
        assert_eq!(purge_answer(&a(false, true), true), Answer::Yes);
        // Without flags the second question is asked only when someone can answer.
        assert_eq!(purge_answer(&a(false, false), true), Answer::Ask);
        assert_eq!(purge_answer(&a(false, false), false), Answer::No);
        // `--purge` alone does not answer the first question.
        assert_eq!(remove_answer(&a(false, true), false), Answer::No);
    }

    const BLOCK: &str = "# >>> xmux installer >>>\nexport PATH=\"/home/u/.local/bin:$PATH\"\n# <<< xmux installer <<<\n";

    /// The launcher directory `BLOCK` adds.
    fn dirs() -> Vec<String> {
        vec!["/home/u/.local/bin".to_string()]
    }

    /// The block `install.sh` appends for `dir`.
    fn block_for(dir: &Path) -> String {
        format!(
            "# >>> xmux installer >>>\nexport PATH=\"{}:$PATH\"\n# <<< xmux installer <<<\n",
            dir.display()
        )
    }

    #[test]
    fn a_block_for_another_install_stays() {
        let other = "# >>> xmux installer >>>\nexport PATH=\"/opt/xmux/bin:$PATH\"\n# <<< xmux installer <<<\n";
        let fish = "# >>> xmux installer >>>\nfish_add_path \"/home/u/.local/bin\"\n# <<< xmux installer <<<\n";
        let edited = "# >>> xmux installer >>>\nexport PATH=\"/home/u/.local/bin:$PATH\"\nexport X=1\n# <<< xmux installer <<<\n";
        let text = format!("a\n\n{other}\n{BLOCK}b\n\n{edited}");
        assert_eq!(
            remove_marked_block(&text, &dirs()).as_deref(),
            Some(format!("a\n\n{other}b\n\n{edited}").as_str()),
            "only the block adding this install's directory goes"
        );
        assert_eq!(remove_marked_block(&format!("x\n\n{other}"), &dirs()), None);
        assert_eq!(
            remove_marked_block(&format!("x\n\n{fish}"), &dirs()).as_deref(),
            Some("x\n"),
            "the fish form is recognised"
        );
        // A directory spelled differently is not this install's.
        assert_eq!(
            remove_marked_block(
                &format!("x\n\n{BLOCK}"),
                &["/home/u/.local/bin/".to_string()]
            ),
            None
        );
    }

    #[test]
    fn removing_the_block_restores_what_the_installer_appended_to() {
        // The installer writes "\n" and then the block, whatever the file ended with.
        for original in ["", "alias ll='ls -l'\n", "no trailing newline", "a\n\n"] {
            let installed = format!("{original}\n{BLOCK}");
            assert_eq!(
                remove_marked_block(&installed, &dirs()).as_deref(),
                Some(original),
                "{original:?}"
            );
        }
    }

    #[test]
    fn removing_the_block_keeps_every_other_line() {
        let text = format!("a\n\n{BLOCK}b\n# >>> not ours\nc\n");
        assert_eq!(
            remove_marked_block(&text, &dirs()).as_deref(),
            Some("a\nb\n# >>> not ours\nc\n")
        );
        // A block whose blank line the user deleted keeps the line break ending
        // the user's own previous line.
        let text = format!("a\n{BLOCK}b\n");
        assert_eq!(
            remove_marked_block(&text, &dirs()).as_deref(),
            Some("a\nb\n")
        );
    }

    #[test]
    fn removing_the_block_is_idempotent_and_ignores_an_unclosed_block() {
        let once = remove_marked_block(&format!("x\n\n{BLOCK}"), &dirs()).unwrap();
        assert_eq!(
            remove_marked_block(&once, &dirs()),
            None,
            "a second pass changes nothing"
        );
        assert_eq!(remove_marked_block("plain\nprofile\n", &dirs()), None);
        let unclosed = "x\n# >>> xmux installer >>>\nexport PATH=y\n";
        assert_eq!(remove_marked_block(unclosed, &dirs()), None);
    }

    #[test]
    fn removing_the_block_from_a_file_rewrites_only_that_file() {
        let dir = temp("profile");
        let profile = dir.join(".bashrc");
        write(&profile, &format!("export A=1\n\n{BLOCK}"));
        remove_block_from(&profile, &dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&profile).unwrap(), "export A=1\n");
        remove_block_from(&profile, &dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&profile).unwrap(), "export A=1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_recorded_entry_is_removed_and_only_once() {
        let rec = r"C:\Tools\xmux";
        assert_eq!(
            remove_recorded_entry(r"C:\Windows;C:\Tools\xmux", rec).as_deref(),
            Some(r"C:\Windows")
        );
        // Case is ignored, as Windows ignores it.
        assert_eq!(
            remove_recorded_entry(r"A;c:\tools\XMUX", rec).as_deref(),
            Some("A")
        );
        // A spelling that differs in anything else is the user's own entry.
        assert_eq!(remove_recorded_entry(r"A;C:\Tools\xmux\", rec), None);
        assert_eq!(remove_recorded_entry(r"A;C:\Tools\xmux/", rec), None);
        assert_eq!(remove_recorded_entry(r"A;C:\Tools\xmux2", rec), None);
        // A user's earlier copy stays; the installer's appended one, the last, goes.
        assert_eq!(
            remove_recorded_entry(r"C:\Tools\xmux;B;C:\Tools\xmux", rec).as_deref(),
            Some(r"C:\Tools\xmux;B")
        );
        // Every other entry, empty ones included, is kept as written.
        assert_eq!(
            remove_recorded_entry(r"A;;%USERPROFILE%\bin;C:\Tools\xmux;B", rec).as_deref(),
            Some(r"A;;%USERPROFILE%\bin;B")
        );
        assert_eq!(remove_recorded_entry(rec, rec).as_deref(), Some(""));
    }

    #[test]
    fn a_path_record_needs_its_header() {
        assert_eq!(
            parse_path_record(&format!("{PATH_RECORD_HEADER}\r\nC:\\Tools\\xmux\r\n")).as_deref(),
            Some(r"C:\Tools\xmux")
        );
        assert_eq!(
            parse_path_record(&format!("\u{feff}{PATH_RECORD_HEADER}\nC:\\a b\n")).as_deref(),
            Some(r"C:\a b")
        );
        assert_eq!(parse_path_record("C:\\Tools\\xmux\r\n"), None);
        assert_eq!(parse_path_record(&format!("{PATH_RECORD_HEADER}\n")), None);
        assert_eq!(parse_path_record("my notes\nC:\\Tools\\xmux\n"), None);
    }

    #[test]
    fn a_path_has_dir_check_names_entries_loosely() {
        let dir = Path::new(r"C:\Users\u\AppData\Local\xmux\bin");
        assert!(path_has_dir(
            r"C:\T;c:\users\U\appdata\local\XMUX\BIN\",
            dir
        ));
        assert!(!path_has_dir(r"C:\Users\u\AppData\Local\xmux\bin2", dir));
    }

    #[test]
    fn package_managers_remove_their_own_installs() {
        let exe = Path::new("/somewhere/xmux");
        let delegate = |m| plan(exe, m, Platform::Unix, &none()).unwrap().program;
        assert_eq!(
            delegate(InstallMethod::Cargo),
            Program::Delegate {
                program: "cargo",
                args: &["uninstall", "xmux"]
            }
        );
        assert_eq!(
            delegate(InstallMethod::Winget),
            Program::Delegate {
                program: "winget",
                args: &["uninstall", "--id", "zer0ken.xmux"]
            }
        );
        assert_eq!(
            delegate(InstallMethod::Brew),
            Program::Delegate {
                program: "brew",
                args: &["uninstall", "xmux"]
            }
        );
    }

    #[test]
    fn a_copied_binary_removes_itself_and_its_sidecars() {
        let dir = temp("self");
        let exe = dir.join("xmux.exe");
        write(&exe, "");
        write(&dir.join("xmux.exe.old-42"), "");
        write(&dir.join("xmux.exe.bak"), "");
        write(&dir.join("other.exe"), "");
        let p = plan(&exe, InstallMethod::Self_, Platform::Windows, &none()).unwrap();
        assert_eq!(
            p.program,
            Program::Binary {
                files: vec![exe.clone(), dir.join("xmux.exe.old-42")]
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_windows_script_install_names_its_launcher_marker_and_path_entry() {
        let base = temp("win-script");
        let root = base.join("xmux");
        write(&root.join("versions").join("1.0.0").join("xmux.exe"), "");
        let bin = root.join("bin");
        let exe = bin.join("xmux.exe");
        write(&exe, "");
        write(
            &bin.join("xmux.exe.install"),
            &format!("{}\n", root.display()),
        );
        write(&bin.join("xmux.exe.old-7"), "");
        write(
            &root.join(PATH_RECORD),
            &format!("\u{feff}{PATH_RECORD_HEADER}\r\n{}\r\n", bin.display()),
        );
        let user_path = format!(r"C:\Tools;{}", bin.display());
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Windows,
            &Surroundings {
                home: None,
                path_dirs: std::slice::from_ref(&bin),
                bin_dir: None,
                user_path: Some(&user_path),
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert!(same_path(&s.root, &root));
        assert_eq!(
            s.files,
            vec![
                exe.clone(),
                bin.join("xmux.exe.install"),
                bin.join("xmux.exe.old-7"),
                root.join(PATH_RECORD)
            ],
            "the PATH dir repeats the launcher, which is listed once"
        );
        assert_eq!(s.user_path_entries, vec![bin.display().to_string()]);
        assert!(s.path_left.is_empty());
        assert!(s.profiles.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unrecorded_user_path_entry_stays_and_is_named() {
        // Without the record the script writes when it appends the entry, the entry
        // may be the user's own, so it stays and the plan names it.
        let base = temp("win-shared");
        let root = base.join("xmux");
        write(&root.join("versions").join("1.0.0").join("xmux.exe"), "");
        let tools = base.join("tools");
        let exe = tools.join("xmux.exe");
        write(&exe, "");
        write(&tools.join("xmux.exe.install"), &root.display().to_string());
        write(&tools.join("other.exe"), "");
        let user_path = tools.display().to_string();
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Windows,
            &Surroundings {
                home: None,
                path_dirs: &[],
                bin_dir: None,
                user_path: Some(&user_path),
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert_eq!(s.files, vec![exe.clone(), tools.join("xmux.exe.install")]);
        assert!(s.user_path_entries.is_empty());
        assert_eq!(s.path_left, vec![tools.clone()]);
        assert!(describe(&Plan {
            method: InstallMethod::Script,
            program: Program::Script(s)
        })
        .iter()
        .any(|l| l.contains("stays on the user PATH")));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The unix layout, with the launcher found through its marker on PATH (a
    /// symlink needs a privilege a Windows test host may lack).
    fn unix_install(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = temp(tag);
        let root = base.join("share").join("xmux");
        let exe = root.join("versions").join("1.0.0").join("xmux");
        write(&exe, "");
        let bin = base.join("bin");
        write(&bin.join("xmux"), "");
        write(&bin.join("xmux.install"), &format!("{}\n", root.display()));
        let home = base.join("home");
        write(
            &home.join(".bashrc"),
            &format!("export A=1\n\n{}", block_for(&bin)),
        );
        write(&home.join(".zshrc"), &format!("export B=2\n\n{BLOCK}"));
        (base, root, exe, bin, home)
    }

    #[test]
    fn a_unix_script_install_names_launcher_marker_and_profile_only() {
        let (base, root, exe, bin, home) = unix_install("unix-plan");
        // Other `xmux` files on PATH that the script did not place are not touched,
        // even one identical to the installed build.
        let foreign = base.join("foreign");
        write(&foreign.join("xmux"), "");
        let other = base.join("other");
        write(&other.join("xmux"), "a different program");
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Unix,
            &Surroundings {
                home: Some(&home),
                path_dirs: &[foreign.clone(), other.clone(), bin.clone()],
                bin_dir: None,
                user_path: None,
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert_eq!(s.files, vec![bin.join("xmux"), bin.join("xmux.install")]);
        assert_eq!(s.profiles, vec![home.join(".bashrc")]);
        assert!(s.user_path_entries.is_empty() && s.path_left.is_empty());
        assert_eq!(
            s.dirs_if_empty,
            vec![s.versions.clone(), s.root.join("bin"), s.root.clone()]
        );
        assert_eq!(s.version_dirs.len(), 1);
        assert!(same_path(&s.versions, &root.join("versions")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn removing_a_script_install_deletes_only_what_the_script_owns() {
        let (base, root, exe, bin, home) = unix_install("unix-remove");
        write(&bin.join("other-tool"), "keep");
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Unix,
            &Surroundings {
                home: Some(&home),
                path_dirs: std::slice::from_ref(&bin),
                bin_dir: None,
                user_path: None,
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        remove_script_install(&s).unwrap();
        assert!(!root.exists(), "an emptied root is removed");
        assert!(!bin.join("xmux").exists() && !bin.join("xmux.install").exists());
        assert!(bin.join("other-tool").exists());
        assert_eq!(
            std::fs::read_to_string(home.join(".bashrc")).unwrap(),
            "export A=1\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(".zshrc")).unwrap(),
            format!("export B=2\n\n{BLOCK}"),
            "a block for another install stays"
        );
        // A second run finds everything already gone and succeeds.
        remove_script_install(&s).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_root_holding_the_users_files_keeps_them() {
        let (base, root, exe, bin, home) = unix_install("unix-shared-root");
        write(&root.join("notes.txt"), "mine");
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Unix,
            &Surroundings {
                home: Some(&home),
                path_dirs: std::slice::from_ref(&bin),
                bin_dir: None,
                user_path: None,
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        remove_script_install(&s).unwrap();
        assert!(!root.join("versions").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "mine"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn purging_removes_exactly_the_named_dirs() {
        let base = temp("purge");
        let state = base.join(".xmux");
        let config = base.join(".config").join("xmux");
        write(&state.join("prefs"), "x");
        write(&config.join("config.toml"), "x");
        write(&base.join(".config").join("other").join("a"), "keep");
        remove_data(&[state.clone(), config.clone()]).unwrap();
        assert!(!state.exists() && !config.exists());
        assert!(base.join(".config").join("other").join("a").exists());
        remove_data(&[state, config]).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_windows_helper_waits_then_removes_the_planned_paths() {
        let si = ScriptInstall {
            root: PathBuf::from(r"C:\x"),
            versions: PathBuf::from(r"C:\x\versions"),
            version_dirs: vec![PathBuf::from(r"C:\x\versions\1.0.0")],
            files: vec![PathBuf::from(r"C:\x\bin\xmux.exe")],
            dirs_if_empty: vec![
                PathBuf::from(r"C:\x\versions"),
                PathBuf::from(r"C:\x\bin"),
                PathBuf::from(r"C:\x"),
            ],
            kept: vec![],
            profiles: vec![],
            block_dirs: vec![],
            user_path_entries: vec![],
            path_left: vec![],
        };
        let s = windows_helper_script(
            &Program::Script(Box::new(si)),
            &[PathBuf::from(r"C:\Users\u\.xmux")],
            Path::new(r"C:\l\u.log"),
            Path::new(r"C:\t"),
        );
        assert!(s.starts_with(update::UPDATER_WAIT_PREAMBLE));
        let chcp = s.find("chcp 65001 >nul\r\n").expect("the code page is set");
        assert!(chcp < s.find("C:\\x").unwrap(), "before the first path");
        assert!(s.contains("del /F /Q \"C:\\x\\bin\\xmux.exe\" >nul 2>&1\r\n"));
        assert!(s.contains(
            "if exist \"C:\\x\\bin\\xmux.exe\" echo not removed: \"C:\\x\\bin\\xmux.exe\">>\"C:\\l\\u.log\"\r\n"
        ));
        assert!(s.contains("rmdir /S /Q \"C:\\x\\versions\\1.0.0\""));
        assert!(
            !s.contains("rmdir /S /Q \"C:\\x\\versions\" "),
            "the versions directory itself goes only when empty"
        );
        assert!(s.contains("rmdir \"C:\\x\\versions\" >nul"));
        assert!(s.contains("rmdir \"C:\\x\\bin\" >nul"));
        assert!(s.contains("rmdir /S /Q \"C:\\Users\\u\\.xmux\""));
        assert!(s.contains("if exist \"C:\\Users\\u\\.xmux\" echo not removed"));
        assert!(s.ends_with(
            "echo xmux uninstall finished>>\"C:\\l\\u.log\"\r\nrmdir /S /Q \"C:\\t\" >nul 2>&1\r\n"
        ));

        let s = windows_helper_script(
            &Program::Delegate {
                program: "cargo",
                args: &["uninstall", "xmux"],
            },
            &[],
            Path::new(r"C:\l\u.log"),
            Path::new(r"C:\t"),
        );
        assert!(s.contains(
            "cargo uninstall xmux >>\"C:\\l\\u.log\" 2>&1 || echo failed: cargo uninstall xmux>>\"C:\\l\\u.log\"\r\n"
        ));
    }

    #[test]
    fn the_windows_helper_keeps_a_non_ascii_path_as_utf8() {
        let s = windows_helper_script(
            &Program::Binary {
                files: vec![PathBuf::from("C:\\도구\\xmux.exe")],
            },
            &[],
            Path::new(r"C:\l\u.log"),
            Path::new(r"C:\t"),
        );
        assert!(s.contains("del /F /Q \"C:\\도구\\xmux.exe\""));
        assert!(!s.starts_with('\u{feff}'), "no byte order mark");
    }

    #[test]
    fn version_names_are_what_the_script_installs_under() {
        for v in [
            "0.13.0",
            "1.2.3",
            "10.20.30",
            "0.14.0-rc.1",
            "1.0.0+build.5",
        ] {
            assert!(is_version_name(v), "{v}");
        }
        for v in [
            "other-app",
            "1.0",
            "1.0.0.0",
            "v1.0.0",
            "1.0.x",
            "1.0.0-",
            ".staging.42",
            "",
        ] {
            assert!(!is_version_name(v), "{v}");
        }
    }

    #[test]
    fn only_the_scripts_version_dirs_are_removed_from_a_shared_versions_dir() {
        let (base, root, exe, bin, home) = unix_install("unix-shared-versions");
        let versions = root.join("versions");
        write(&versions.join("other-app").join("data"), "keep");
        write(
            &versions.join("2.0.0").join("readme"),
            "no xmux binary here",
        );
        std::fs::create_dir_all(versions.join("3.0.0").join("xmux")).unwrap();
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Unix,
            &Surroundings {
                home: Some(&home),
                path_dirs: std::slice::from_ref(&bin),
                bin_dir: None,
                user_path: None,
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert_eq!(s.version_dirs.len(), 1);
        assert!(same_path(&s.version_dirs[0], &versions.join("1.0.0")));
        remove_script_install(&s).unwrap();
        assert!(!versions.join("1.0.0").exists());
        assert!(versions.join("other-app").join("data").exists());
        assert!(versions.join("2.0.0").join("readme").exists());
        assert!(versions.join("3.0.0").join("xmux").is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_directory_named_like_a_sidecar_is_not_collected() {
        let dir = temp("sidecar-dir");
        let exe = dir.join("xmux.exe");
        write(&exe, "");
        write(&dir.join("xmux.exe.old-7").join("users-file"), "keep");
        write(&dir.join("xmux.exe.old-8"), "");
        assert_eq!(sidecars_of(&exe).unwrap(), vec![dir.join("xmux.exe.old-8")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launcher_in_the_root_needs_proof() {
        // `<root>/bin/xmux.exe` with no marker: a byte-for-byte copy of an installed
        // version is the script's launcher, anything else stays and is named.
        let base = temp("win-proof");
        let root = base.join("xmux");
        let version = root.join("versions").join("1.0.0");
        write(&version.join("xmux.exe"), "the build");
        let exe = version.join("xmux.exe");
        let launcher = root.join("bin").join("xmux.exe");
        write(&launcher, "something else");
        let windows = |user_path| Surroundings {
            home: None,
            path_dirs: &[],
            bin_dir: None,
            user_path,
        };
        let script = |p: Plan| match p.program {
            Program::Script(s) => s,
            _ => panic!("a script install"),
        };
        let s = script(
            plan(
                &exe,
                InstallMethod::Script,
                Platform::Windows,
                &windows(None),
            )
            .unwrap(),
        );
        assert!(s.files.is_empty());
        assert_eq!(s.kept, vec![launcher.clone()]);

        write(&launcher, "the build");
        let s = script(
            plan(
                &exe,
                InstallMethod::Script,
                Platform::Windows,
                &windows(None),
            )
            .unwrap(),
        );
        assert_eq!(s.files, vec![launcher.clone()]);
        assert!(s.kept.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_record_naming_another_entry_leaves_the_path_alone() {
        let base = temp("win-record-mismatch");
        let root = base.join("xmux");
        write(&root.join("versions").join("1.0.0").join("xmux.exe"), "");
        let bin = root.join("bin");
        let exe = bin.join("xmux.exe");
        write(&exe, "");
        write(&bin.join("xmux.exe.install"), &root.display().to_string());
        write(
            &root.join(PATH_RECORD),
            &format!("{PATH_RECORD_HEADER}\nC:\\Somewhere\\Else\n"),
        );
        let user_path = bin.display().to_string();
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Windows,
            &Surroundings {
                home: None,
                path_dirs: &[],
                bin_dir: None,
                user_path: Some(&user_path),
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert!(s.user_path_entries.is_empty());
        assert_eq!(s.path_left, vec![bin.clone()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_record_without_its_header_is_neither_trusted_nor_deleted() {
        let base = temp("win-record-foreign");
        let root = base.join("xmux");
        write(&root.join("versions").join("1.0.0").join("xmux.exe"), "");
        let bin = root.join("bin");
        let exe = bin.join("xmux.exe");
        write(&exe, "");
        write(&bin.join("xmux.exe.install"), &root.display().to_string());
        write(&root.join(PATH_RECORD), &format!("{}\n", bin.display()));
        let user_path = bin.display().to_string();
        let p = plan(
            &exe,
            InstallMethod::Script,
            Platform::Windows,
            &Surroundings {
                home: None,
                path_dirs: &[],
                bin_dir: None,
                user_path: Some(&user_path),
            },
        )
        .unwrap();
        let Program::Script(s) = p.program else {
            panic!("a script install")
        };
        assert!(!s.files.contains(&root.join(PATH_RECORD)));
        assert!(s.user_path_entries.is_empty());
        assert_eq!(s.path_left, vec![bin.clone()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_directory_that_cannot_be_listed_is_an_error() {
        // A path that is a file cannot be listed on any platform; the answer must be
        // an error, never an empty list that lets the removal report success.
        let base = temp("unlistable");
        let not_a_dir = base.join("versions");
        write(&not_a_dir, "a file");
        let e = version_dirs_in(&not_a_dir, "xmux").unwrap_err();
        assert!(e.contains("cannot list"), "{e}");
        let e = sidecars_of(&not_a_dir.join("xmux")).unwrap_err();
        assert!(e.contains("cannot list"), "{e}");
        // A directory that is not there has nothing in it.
        assert_eq!(version_dirs_in(&base.join("gone"), "xmux"), Ok(vec![]));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_versions_dir_stops_the_plan() {
        use std::os::unix::fs::PermissionsExt;
        let (base, root, exe, bin, home) = unix_install("unix-unlistable");
        let versions = root.join("versions");
        std::fs::set_permissions(&versions, std::fs::Permissions::from_mode(0o111)).unwrap();
        let r = plan(
            &exe,
            InstallMethod::Script,
            Platform::Unix,
            &Surroundings {
                home: Some(&home),
                path_dirs: std::slice::from_ref(&bin),
                bin_dir: None,
                user_path: None,
            },
        );
        std::fs::set_permissions(&versions, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root reads anything, so only a refused read is checked.
        if std::fs::read_dir(&versions).is_ok() && r.is_ok() {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        assert!(r.err().unwrap().contains("cannot list"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn install_ps1_records_the_entry_it_appends() {
        // The record uninstall reads is written by the installer, under this name,
        // only where it appends the entry.
        let script = include_str!("../../scripts/install/install.ps1");
        let add = &script[script.find("function Add-ToUserPath").unwrap()..];
        let add = &add[..add.find("\n}\n").or_else(|| add.find("\r\n}\r\n")).unwrap()];
        let set = add
            .find("SetEnvironmentVariable('Path', $updated, 'User')")
            .unwrap();
        let record = add.find("$RecordFile").unwrap();
        assert!(record < set);
        assert!(add.contains(&format!("$recordHeader = '{PATH_RECORD_HEADER}'")));
        let after = &add[set..];
        let check = after.find("-TotalCount 1) -ceq $recordHeader").unwrap();
        let write = after
            .find("Set-Content -LiteralPath $RecordFile -Value @($recordHeader, $Dir)")
            .unwrap();
        assert!(
            check < write,
            "an existing file is checked before it is written"
        );
        assert!(script.contains(&format!("Join-Path $Root '{PATH_RECORD}'")));
    }

    /// Runs `confirm_and_remove` with recording steps; `running_from` is the guard
    /// call (1-based) from which an instance is running.
    fn run_steps(
        remove: Answer,
        purge: Answer,
        input: &str,
        running_from: usize,
    ) -> (Result<Outcome, String>, Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let guards = std::cell::Cell::new(0);
        let mut guard = || {
            guards.set(guards.get() + 1);
            calls.borrow_mut().push("guard".to_string());
            if guards.get() >= running_from {
                Err("xmux is running (amber-otter)".to_string())
            } else {
                Ok(())
            }
        };
        let mut remove_program = |_: &mut dyn Write| {
            calls.borrow_mut().push("program".to_string());
            Ok(())
        };
        let mut finish = |purge: bool, _: &mut dyn Write| {
            calls.borrow_mut().push(format!("finish {purge}"));
            Ok(())
        };
        let mut out = Vec::new();
        let r = confirm_and_remove(
            remove,
            purge,
            &[],
            &mut input.as_bytes(),
            &mut out,
            Steps {
                guard: &mut guard,
                remove_program: &mut remove_program,
                finish: &mut finish,
            },
        );
        (r, calls.into_inner())
    }

    #[test]
    fn running_instances_are_checked_again_before_each_removal() {
        let (r, calls) = run_steps(Answer::Ask, Answer::Ask, "y\ny\n", usize::MAX);
        assert_eq!(r, Ok(Outcome::Removed));
        assert_eq!(calls, ["guard", "program", "guard", "finish true"]);

        // An instance started while the first question waited: nothing is removed.
        let (r, calls) = run_steps(Answer::Ask, Answer::Ask, "y\ny\n", 1);
        assert!(r.unwrap_err().contains("amber-otter"));
        assert_eq!(calls, ["guard"]);

        // One started while the second question waited: its state stays, and the
        // confirmed program removal still completes rather than stopping half done.
        let (r, calls) = run_steps(Answer::Yes, Answer::Yes, "", 2);
        let e = r.unwrap_err();
        assert!(e.contains("settings and data were kept because") && e.contains("amber-otter"));
        assert_eq!(calls, ["guard", "program", "guard", "finish false"]);

        // Keeping the settings needs no second check.
        let (r, calls) = run_steps(Answer::Ask, Answer::Ask, "yes\n\n", 2);
        assert_eq!(r, Ok(Outcome::Removed));
        assert_eq!(calls, ["guard", "program", "finish false"]);

        // A no removes nothing and checks nothing.
        let (r, calls) = run_steps(Answer::Ask, Answer::Ask, "", usize::MAX);
        assert_eq!(r, Ok(Outcome::Declined));
        assert!(calls.is_empty());
    }

    #[test]
    fn the_running_error_names_every_instance() {
        assert_eq!(running_error(&[]), None);
        let live = [
            (PathBuf::from("a"), "amber-otter".to_string()),
            (PathBuf::from("b"), "brisk-wren".to_string()),
        ];
        let e = running_error(&live).unwrap();
        assert!(e.contains("amber-otter, brisk-wren") && e.contains("xmux send amber-otter quit"));
    }

    #[test]
    fn a_percent_in_a_path_survives_cmd_expansion() {
        assert_eq!(cmd_path(Path::new(r"C:\a%b%")), "\"C:\\a%%b%%\"");
    }
}
