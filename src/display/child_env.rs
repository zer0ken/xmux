//! Reading ONE environment variable out of a live attach child, mux-blind.
//!
//! Some muxes move their client to another session INSIDE the client process: it
//! detaches from one session's server and the same process reconnects to another, so
//! the child xmux spawned keeps its pid and its original argv and only its ENVIRONMENT
//! is rewritten. No server sees that move, so no control channel can report it, and the
//! live child's own environment is the only witness of which session it is really on.
//! This module reads that witness. It names no mux and no variable: which variable
//! carries the session name is the mux's knowledge, supplied by the caller.
//!
//! The read is a WINDOWS facility. A process's environment block lives in that process's
//! own address space, reachable through its PEB, so reading it means reading another
//! process's memory, and that only works for a process on THIS machine. Linux exposes
//! `/proc/<pid>/environ`, which is the environment the process was EXEC'd with and never
//! reflects a later in-process rewrite, so it would answer the session the client first
//! attached to forever, and a wrong answer is worse than none. Every other platform
//! therefore has no signal at all and says so by answering `None`.

/// The value of `name` in `child`'s LIVE environment, or `None` when there is no answer:
/// the child is gone, its memory cannot be read, the platform exposes no live
/// environment, or it holds no such variable. `None` always means "no signal", never
/// "the variable is empty" - an empty variable answers `Some("")`.
pub fn read(child: &(dyn portable_pty::Child + Send + Sync), name: &str) -> Option<String> {
    imp::read(child, name)
}

/// Finds `name` in a Windows environment BLOCK: `NAME=VALUE` entries in UTF-16, each
/// NUL-terminated, the run closed by an empty entry. Names are compared without case,
/// the way the Windows environment itself compares them.
///
/// An entry with no terminating NUL is a TRUNCATED tail (the block was read up to a page
/// that could not be read) and ends the scan without being interpreted: half a value
/// taken for a whole one would name a session that does not exist. An entry whose name
/// is empty is skipped rather than matched, because the block's leading per-drive
/// working directories are spelled that way.
pub(crate) fn lookup(block: &[u16], name: &str) -> Option<String> {
    let mut rest = block;
    loop {
        let end = rest.iter().position(|&unit| unit == 0)?;
        if end == 0 {
            return None; // the empty entry closing the block
        }
        let (entry, tail) = rest.split_at(end);
        rest = &tail[1..];
        let text = String::from_utf16_lossy(entry);
        let Some((key, value)) = text.split_once('=') else {
            continue;
        };
        if !key.is_empty() && key.eq_ignore_ascii_case(name) {
            return Some(value.to_string());
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        PEB, PROCESS_BASIC_INFORMATION, RTL_USER_PROCESS_PARAMETERS,
    };

    /// Where the `Environment` pointer sits inside `RTL_USER_PROCESS_PARAMETERS`.
    /// windows-sys declares that structure only as far as `CommandLine`, which is the
    /// field immediately before `Environment` in the OS layout, so the pointer sits
    /// exactly at the declared structure's size. Deriving the offset from `size_of`
    /// rather than writing the number keeps 32-bit and 64-bit right with no `cfg`.
    const ENVIRONMENT_OFFSET: usize = std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>();

    /// The offset the OS layout puts `Environment` at, per pointer width. The derivation
    /// above holds only while windows-sys keeps its declaration truncated at
    /// `CommandLine`; a version that declares more fields would move the offset silently
    /// and make every read return another field's bytes. This fails the BUILD instead.
    const _: () = assert!(
        ENVIRONMENT_OFFSET
            == if cfg!(target_pointer_width = "64") {
                0x80
            } else {
                0x48
            }
    );

    /// How much of the environment block to read, and in what steps. The block is one
    /// allocation whose length is recorded nowhere this code can reach, so it is read in
    /// steps until the closing empty entry appears. A step that runs off the end of the
    /// allocation fails as a whole, ending the read with whatever came before it, so the
    /// step is small enough that such a failure costs little and the cap is large enough
    /// to hold an environment far bigger than any real one.
    const READ_STEP: usize = 2048;
    const READ_CAP: usize = 64 * 1024;

    pub(super) fn read(
        child: &(dyn portable_pty::Child + Send + Sync),
        name: &str,
    ) -> Option<String> {
        // The handle `CreateProcess` returned for this child, which portable-pty already
        // holds and hands over. Opening a second handle would ask the OS for rights this
        // one was born with, and would have to name the process by pid, which is the racy
        // way to name one: a dead child's pid is reused.
        let handle = child.as_raw_handle()? as HANDLE;
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is a live process handle the child owns for the whole call
        // (the caller holds the attachment that owns it), every destination below is a
        // local buffer sized by `size_of`, and each read is checked before its result is
        // used. Nothing here writes to the other process.
        unsafe {
            let peb_base = peb_base(handle)?;
            let peb: PEB = read_struct(handle, peb_base as *const c_void)?;
            let params = peb.ProcessParameters as usize;
            if params == 0 {
                return None;
            }
            let env: usize = read_struct(handle, (params + ENVIRONMENT_OFFSET) as *const c_void)?;
            if env == 0 {
                return None;
            }
            super::lookup(&read_block(handle, env), name)
        }
    }

    /// The child's PEB address, out of its basic process information.
    unsafe fn peb_base(handle: HANDLE) -> Option<usize> {
        let mut info = PROCESS_BASIC_INFORMATION::default();
        let mut written = 0u32;
        let status = NtQueryInformationProcess(
            handle,
            ProcessBasicInformation,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut written,
        );
        if status != 0 {
            return None;
        }
        Some(info.PebBaseAddress as usize)
    }

    /// One `T` read out of the child's address space at `addr`, or `None` when the whole
    /// `T` could not be read.
    unsafe fn read_struct<T>(handle: HANDLE, addr: *const c_void) -> Option<T> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        let want = std::mem::size_of::<T>();
        let mut got = 0usize;
        let ok = ReadProcessMemory(
            handle,
            addr,
            value.as_mut_ptr() as *mut c_void,
            want,
            &mut got,
        );
        (ok != 0 && got == want).then(|| value.assume_init())
    }

    /// The child's environment block, read from `addr` in steps until the closing empty
    /// entry appears, a step fails, or the cap is reached. The result is UTF-16 units;
    /// interpreting them is `lookup`'s job, and it drops an unterminated tail, which is
    /// exactly what a failed step leaves behind.
    unsafe fn read_block(handle: HANDLE, addr: usize) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();
        let mut offset = 0usize;
        while offset < READ_CAP {
            let mut step = vec![0u16; READ_STEP / 2];
            let mut got = 0usize;
            let ok = ReadProcessMemory(
                handle,
                (addr + offset) as *const c_void,
                step.as_mut_ptr() as *mut c_void,
                READ_STEP,
                &mut got,
            );
            if ok == 0 || got == 0 {
                break;
            }
            step.truncate(got / 2);
            let closed = step.windows(2).any(|pair| pair == [0, 0]);
            block.extend_from_slice(&step);
            if closed {
                break;
            }
            offset += got;
        }
        block
    }
}

/// No live environment to read on this platform, so there is no signal: a process's
/// exec-time environment answers the session its client first attached to and never a
/// later in-process rewrite, which is a wrong answer rather than a missing one.
#[cfg(not(windows))]
mod imp {
    pub(super) fn read(
        _child: &(dyn portable_pty::Child + Send + Sync),
        _name: &str,
    ) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::lookup;

    /// Builds a Windows environment block out of `NAME=VALUE` entries: each entry
    /// NUL-terminated, the run closed by one more NUL.
    fn block(entries: &[&str]) -> Vec<u16> {
        let mut out: Vec<u16> = Vec::new();
        for e in entries {
            out.extend(e.encode_utf16());
            out.push(0);
        }
        out.push(0);
        out
    }

    #[test]
    fn lookup_finds_a_value_among_other_variables() {
        // The variable sits at neither end of the block, and the entries around it must
        // not be mistaken for it.
        let b = block(&[
            "=C:=C:\\work",
            "PATH=C:\\bin;C:\\tools",
            "PSMUX_SESSION_NAME=vfy-ps-b",
            "TERM=xterm-256color",
        ]);
        assert_eq!(
            lookup(&b, "PSMUX_SESSION_NAME").as_deref(),
            Some("vfy-ps-b")
        );
        assert_eq!(lookup(&b, "TERM").as_deref(), Some("xterm-256color"));
    }

    #[test]
    fn lookup_answers_none_for_a_variable_the_block_does_not_hold() {
        // A block with no such entry is no signal, which is the answer that makes the
        // caller reattach rather than believe a session name it never read.
        let b = block(&["PATH=C:\\bin", "TERM=xterm-256color"]);
        assert_eq!(lookup(&b, "PSMUX_SESSION_NAME"), None);
    }

    #[test]
    fn lookup_matches_the_name_without_case_and_keeps_the_value_verbatim() {
        // Windows compares environment names without case, so the lookup does too; the
        // value is a session name and comes back exactly as stored.
        let b = block(&["PSMUX_SESSION_NAME=Vfy-PS-B"]);
        assert_eq!(
            lookup(&b, "psmux_session_name").as_deref(),
            Some("Vfy-PS-B")
        );
    }

    #[test]
    fn lookup_reads_an_empty_value_as_a_value_not_as_absence() {
        // An empty variable is a real answer and stays distinguishable from "no signal":
        // the caller treats the two differently.
        let b = block(&["PSMUX_SESSION_NAME=", "TERM=xterm"]);
        assert_eq!(lookup(&b, "PSMUX_SESSION_NAME").as_deref(), Some(""));
    }

    #[test]
    fn lookup_stops_at_the_entry_closing_the_block() {
        // Whatever units follow the block's empty entry belong to no entry. Reading past
        // it would interpret unrelated memory as an environment variable.
        let mut b = block(&["PATH=C:\\bin"]);
        b.extend("PSMUX_SESSION_NAME=ghost".encode_utf16());
        b.push(0);
        assert_eq!(lookup(&b, "PSMUX_SESSION_NAME"), None);
    }

    #[test]
    fn lookup_discards_an_unterminated_tail() {
        // A read cut short at an unreadable page leaves half an entry behind. Half a
        // session name taken for a whole one would name a session that does not exist,
        // so the truncated tail is dropped instead of interpreted.
        let mut b: Vec<u16> = Vec::new();
        b.extend("PATH=C:\\bin".encode_utf16());
        b.push(0);
        b.extend("PSMUX_SESSION_NAME=vfy-ps".encode_utf16());
        assert_eq!(lookup(&b, "PSMUX_SESSION_NAME"), None);
        assert_eq!(lookup(&b, "PATH").as_deref(), Some("C:\\bin"));
    }
}

#[cfg(all(test, windows))]
mod live_tests {
    /// Proves the read reaches a REAL running process, which the block tests above cannot:
    /// they exercise the parser over a block built in this process, while everything that
    /// can be wrong about reaching another process (the information class, the PEB walk,
    /// the environment offset, the stepped read) lives outside them. A child is spawned
    /// through the same PTY machinery a display attach uses, carrying a marker variable,
    /// and the marker is read back out of it.
    #[test]
    fn reads_a_variable_out_of_a_real_child() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open a pty");
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "pause"]);
        cmd.env("XMUX_CHILD_ENV_PROBE", "vfy-ps-b");
        let child = pty.slave.spawn_command(cmd).expect("spawn a child");
        drop(pty.slave);

        // The environment block is set up before the image runs, but the spawn returns as
        // soon as the process object exists, so give the child a moment to get that far.
        let mut got = None;
        for _ in 0..50 {
            got = super::read(&*child, "XMUX_CHILD_ENV_PROBE");
            if got.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            got.as_deref(),
            Some("vfy-ps-b"),
            "the live child's own environment answers the variable it was given"
        );
    }
}
