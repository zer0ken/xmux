# Working Notes: /scripts/e2e/windows

## Purpose

What the Windows client run builds on the Windows machine: an `ssh.exe` wrapper that
hands Windows OpenSSH the run's temporary ssh config.

## Module Seams

- `suite.py` compiles the wrapper with `rustc` into the run's temporary directory and
  puts it first on the `PATH` xmux sees.

## Invariants

- The wrapper adds only `-F <config>` and passes the terminal, the streams, and the exit
  code through unchanged.
