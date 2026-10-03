# ADR 0006: Process-Memory SSH Password Authentication

## Status

Accepted

## Context

xmux must be able to use one submitted password for every direct ssh connection the
running app starts for that machine. A proxied destination requires key authentication
because its hop inherits askpass. Correctness cannot depend on connection multiplexing because
Windows OpenSSH does not provide ControlMaster and a shared master can disappear.

The password must remain in xmux process memory. It must not enter argv, child
environments, logs, rendered output, configuration, status output, or another persistent
file. ssh must try keys first, must never read a password from the user's terminal, and
must never accept a changed host key. A real password refusal must remove only the
credential used by that command. An unrelated failure or a late result from an older
command must preserve a newer credential.

Every ssh shape owned by the running app has this requirement: probes, enumeration,
metadata and control channels, mux operations, display attachments, terminal handover,
and public-key registration. A separate command-line invocation cannot access another
process's memory and uses keys or the platform ssh client's terminal prompt.

## Requirements

- R1: Every direct ssh started by the running app can use the held machine password without
  depending on connection sharing.
- R2: Passwords remain in process memory, never enter argv, child environments, logs,
  rendered output, status, or files. Held credential allocations and current password-field
  allocations are overwritten in full when released. Transient terminal and IPC buffers
  remain process memory.
- R3: Connection sharing remains a performance optimization where supported.
- R4: Login reports its own bounded, sanitized, categorized failure independently of
  later probes.
- R5: Key registration uses the same authentication path and reports its outcome.
- R6: Replacement, refusal, cancellation, roster removal, and process exit remove only
  the credential they own.
- R7: Authentication preserves asked-for requests, one request per machine, and the
  no-secret diagnostic boundary.

## Options

### SSH_ASKPASS with a private credential broker

xmux can hold one credential per host and expose it through private local IPC. Each ssh
child receives the xmux executable as `SSH_ASKPASS`, the broker endpoint, and an opaque
per-command token. The helper enters askpass mode before logging and command-line parsing,
validates the exact account and host in an OpenSSH password prompt, and asks the broker for the
password. Each token answers at most once and remains valid until its child is reaped.

This preserves OpenSSH configuration, keys, agents, known-host behavior, proxies, remote
shell behavior, and PTY integration. The secret is absent from argv and the ssh child
environment. Each command has an independent record of whether the broker supplied its
password. Bounded IPC and one password attempt prevent a silent wait.

### Password in the ssh child environment

xmux can place the password directly in an environment variable. This has less IPC code,
but exposes the secret to process environment inspection and to configurations that
forward matching variables. It fails the required secret boundary.

### PTY conversation for every ssh

xmux can allocate a hidden PTY for every ssh and answer prompts by parsing terminal
output. This avoids argv and disk, but makes localized wording, terminal controls, chunk
boundaries, every control channel, and every display attachment part of the authentication
protocol. It is difficult to prove that an unknown prompt cannot hang or receive a wrong
answer.

### Persistent authenticated master or agent

xmux can require a ControlMaster or place credentials in an agent. A master is unavailable
on Windows and can disappear independently. An agent changes external state and does not
represent a password-only server. Neither makes a correct password sufficient on every
supported client.

### Native SSH implementation

xmux can replace the platform client with an SSH library. It would then need to reproduce
OpenSSH configuration, known hosts, keys, agents, proxies, shell execution, PTY behavior,
and platform integration. That compatibility surface is disproportionate to the
authentication boundary.

## Evaluation

The askpass broker meets R1 because every direct app-owned ssh consults the same host-keyed store
at spawn time, including clients without multiplexing. It meets R2 through a process-only
secret, private IPC, per-command opaque tokens, strict prompt filtering, and bounded waits.
It meets R3 because ControlMaster remains available only as an optimization. It meets R4
and R5 because login and registration are ordinary bounded commands with separate,
sanitized outcomes. It meets R6 because pending promotion, replacement, refusal,
cancellation and roster removal are token-scoped. It meets R7 because the
execution boundary carries argv and child environment together without adding network
requests or secret-bearing debug values.

The environment option fails R2 and R7. The PTY option can meet R1 but makes R2 prompt
safety and R4 diagnosis depend on terminal parsing at every spawn. A master or agent fails
R1 on Windows, after master loss, and for password-only servers without external state. A
native implementation could meet R1 through R7 but carries a much larger compatibility
and maintenance cost.

## Decision

xmux uses `SSH_ASKPASS` with a private local IPC credential broker.

The broker holds one active credential per host and one pending replacement during a
login. A pending credential is available only to its login command. Success promotes that
exact token only when the broker served its password. The submitted address, port, and
user are included in a bounded effective ssh configuration query before the pending
credential is installed. Key authentication discards an unused pending password.
Failure, cancellation, or replacement removes only that token. A command
removes an active credential only when that command received the password, ssh exited
255, and ssh emitted its own final account-and-host authentication refusal line. A late
result cannot remove a newer credential. Removing or replacing a credential immediately
invalidates every outstanding token and releases the held plaintext. Process exit removes
every credential, token, and local broker endpoint. The held credential allocation and
current password-field allocation are overwritten in full when released. Transient
terminal and IPC buffers remain process memory, and operating-system crash dump policy is
outside xmux's control.

Every executable command carries argv plus child-only environment. With a held password,
ssh keeps its key-first order, permits password and keyboard-interactive authentication,
forces askpass, and uses `NumberOfPasswordPrompts=1`. The helper answers password prompts
only when the account and host exactly match the held account and target alias, resolved
host name, or host-key alias. A destination configured with `ProxyJump` or
`ProxyCommand` does not enter the password path because its hop would inherit askpass.
The helper refuses host-key confirmations, key passphrases,
passcodes, one-time codes, and every other prompt. Each
token answers at most once and remains valid until its child is reaped. The child receives
the broker endpoint and token, never the password.

Without a held password, app-owned ssh uses `BatchMode=yes`. On Unix, OpenSSH older than
8.4 runs non-interactive children in a new session, so it cannot read the controlling
terminal and can use askpass through the compatibility display value when needed. On
Windows, OpenSSH older than 8.4 does not enter the password path and the user is told to
update OpenSSH or register a key. A tty attachment on an older client stays
non-interactive rather than exposing the terminal to a password prompt.

The submitted login uses `StrictHostKeyChecking=accept-new` only when OpenSSH reports the
effective policy as `ask`. An explicit `yes` is never weakened; an unknown key under that
policy stays unreachable and reports an `ask` command that displays the fingerprint before
the user decides whether to add it. Background probes and later commands
preserve the user's host-key policy. A changed host key is never approved by the helper.
ControlMaster remains enabled where supported, with `cm-%C` as its path, but every command
remains independently capable of password authentication.

Login is one bounded ssh command. Public-key registration is another ordinary ssh command
using the promoted credential. It is built from the host's transport and submitted values
even when the host has no source yet. Registration reports registered, skipped with a
reason, or failed with the sanitized ssh reason in a completion message, the log, and host
information.

Displayed ssh diagnostics decode valid UTF-8 bytes that OpenSSH escaped in octal, remove
terminal controls, password prompts, and protocol markers, and are bounded. A
plain-language category precedes ssh's detail. Categories cover password refusal, name
resolution, unreachable host, changed host key, server close after password acceptance,
timeout, cancellation, and other ssh failure.

Login and registration results are stored separately from probe results. A later probe
cannot replace the login reason. A probe carries the credential generation from spawn, so
an older probe result cannot reclassify a machine after a newer login. A refusal
that did not receive the held password remains visible. Only ssh's own authentication
refusal and an approvable first-seen host key open the login pane; remote command
permissions, name resolution, connectivity, strict-policy host-key failures, and changed
host keys remain unreachable.

## Consequences

Every ssh spawn site must use the execution boundary that carries both argv and child
environment. A new spawn cannot safely accept bare argv.

The local broker endpoint is accessible only to the current account and the operating
system account where required. Unix uses a mode-0700 directory and a mode-0600 socket.
Windows uses a protected named-pipe descriptor naming the current user SID and SYSTEM.
Every accept failure marks credentials unavailable, drops the listener, and recreates the
endpoint with bounded backoff. Commands composed while the endpoint is unavailable use
batch mode and report that password login is unavailable.

The host-keyed store is consulted at spawn time. Sources created after login and sources
assembled for off-loop operations therefore receive the same credential without copying
the secret into source, login result, or debug values.

Multiplexed and non-multiplexed clients have the same authentication behavior. Losing a
master affects performance only. The standalone attach command runs in a fresh process,
so it uses keys or ssh's own interactive terminal prompt rather than the running app's
in-memory password.
