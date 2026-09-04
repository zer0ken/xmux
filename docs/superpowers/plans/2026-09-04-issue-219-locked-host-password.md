# Issue 219: 암호 필요 ssh 호스트의 locked 상태와 잠금 해제

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 암호 인증이 필요한 ssh 원격 머신을 unreachable과 구분되는 locked 상태로 표시하고, 사용자가 id와 pw를 직접 입력해 ControlMaster 인증 연결을 세우는 잠금 해제 경로를 제공한다.

**Architecture:** 분류는 ssh가 실제로 보고한 실패 텍스트만 사용한다(추측 없음). poll 경로는 이미 원시 stderr가 `Group.err`로 흐르고, 제어 모드 reader가 ssh 자식의 실제 실패 줄을 표면화하도록 확장한다. 잠금 해제는 한 번의 PTY 프롬프트 응답으로 `ControlMaster=yes` 마스터를 세우고, 이후 모든 `BatchMode=yes` 메타 채널과 `ssh -t` 부착이 같은 컨트롤 소켓을 재사용한다. id와 pw는 사용자가 두 단계 입력으로 직접 제공하고, 암호는 런 동안 메모리에만 유지하며 dump/status/log/argv에 흐르지 않는다.

**Tech Stack:** Rust, cargo test, portable-pty. 검증: `src/link/reader.rs`의 canned-lines reader 테스트, `src/mux/mod.rs`의 분류 테스트, `src/ui/tree.rs`/`src/ui/switcher/tests.rs`/`src/app/runtime/tests.rs`의 Harness, 그리고 로컬 sshd를 상대로 한 `#[ignore]` 라이브 게이트(`tmp/pw_unlock_proto.py`로 가능성을 검증함).

---

## Research Findings (feasibility 검증 결과)

로컬 sshd에 테스트 계정을 만들어 다음을 실제로 확인했다.

1. **분류 텍스트가 정확하다.** `BatchMode=yes` 인증 실패는 exit 255 + `user@host: Permission denied (publickey,password).`(정형 서명). 연결 거부는 `Connection refused`, 타임아웃 `Connection timed out`, 경로 없음 `No route to host`, 호스트키 불일치는 `Host key verification failed.`. 모두 `Permission denied (` 서명으로 locked만 정확히 식별된다.
2. **PTY 프롬프트 응답으로 ControlMaster를 세울 수 있다.** `ssh -o ControlMaster=yes -o ControlPath=<cp> -o ControlPersist=60s <host> true`를 pty(controlling tty) 위에서 띄우고 `password:` 프롬프트에 답하면, ssh가 인증 후 마스터를 세우고 포그라운드가 exit 0으로 종료된다(좀비 후 reap, `ps`로 확인). 호스트키 프롬프트(`yes/no`)는 `yes\n`으로 답하면 통과한다.
3. **BatchMode=yes가 인증된 마스터를 재사용한다.** 잠금 해제로 세운 마스터 소켓에 대해 `ssh -o BatchMode=yes -o ControlMaster=auto -o ControlPath=<같은 cp> <host> true`가 인증 프롬프트 없이 exit 0으로 통과한다. 이게 이 설계의 핵심이다.
4. **잘못된 암호는 빠르게 판정된다.** 첫 응답 후 `Permission denied, please try again.` + 재프롬프트가 오므로, 재프롬프트를 보면 즉시 인증 실패로 끝낸다(3회 시도 루프 없음).
5. **제어 모드 reader가 원시 실패 줄을 볼 수 있다.** `ssh -tt -o BatchMode=yes … tmux -CC`의 인증 실패 출력은 pty 스트림에 `user@host: Permission denied (publickey,password).`로 나타나고, 현재 reader는 이를 `Line::Body`로 버린다(Task 2의 캡처 지점).
6. **xmux의 기존 PTY 원시자료를 재사용한다.** `src/link/client.rs`의 `spawn_pty_child`(portable-pty `openpty`)가 정확히 필요한 프리미티브다(Task 9).

### 상태 분류 단일 사이트

`Group.err: Option<String>`이 실패 텍스트를 운반한다. `is_locked(text)`는 ssh 정형 서명(`Permission denied (`)만 매칭해, 도달 가능하지만 인증 실패인 머신을 definitive하게 locked로 분류한다. 그 외(refused/timeout/no route/host key)는 전부 unreachable로 남는다. 거짓양성(도달 실패를 locked로)은 암호 입력을 유도하므로 최악이라, 정형 서명만 매칭해 회피한다.

### 잠금 해제 = ControlMaster 단일 인증

xmux의 ssh 옵션은 이미 머신별로 `ControlMaster=auto` + `ControlPath=<xmux_dir>/cm-<machine>.sock` + `ControlPersist=60s`를 공유한다(프로브/메타/부착 전부). 잠금 해제는 `ControlMaster=yes`로 한 번 인증된 마스터를 세우는 것뿐이고, 이후 모든 채널이 같은 소켓을 재사용해 인증 없이 통과한다. unlock 이후 재스캔(`Command::Rescan`)이 호스트를 Live로 되돌린다.

### 플랫폼 경계 (정의됨, 추측 없음)

- **POSIX (Linux/macOS)**: ControlMaster 사용 가능 → 잠금 해제가 동작한다. 이 박스(로컬 sshd)에서 라이브로 검증한다.
- **Windows**: `Ssh::ssh_opts`가 `os == "windows"`면 ControlMaster를 아예 생략한다. 재사용할 마스터가 없으므로 잠금 해제는 의미가 없다. `Transport::unlock_argv`가 Windows에서 `None`을 반환하고, locked 뷰는 "Windows에서는 암호 잠금 해제를 지원하지 않는다"를 안내한다. 상태 분류 자체는 플랫폼 중립이라 locked 카드는 Windows에서도 정확히 표시되되 해제 경로만 닫힌다.
- **WSL/로컬**: 암호 없음. `unlock_argv` 기본값 `None`.

### 암호 수명 (이슈 설계 항목 결정)

런 동안 `State::passwords: HashMap<source, (user, secret)>`에만 유지한다. 같은 호스트의 재시도(마스터 만료 후 재잠금)에 재사용한다: unlock 입력을 다시 열면 두 필드가 프리필되고 Enter 한 번으로 재전송된다. 호스트가 locked가 아닌 unreachable로 전환되면(인증 문제가 아니라 죽음) 암호를 지운다. 앱 종료 시 전부 소멸(영속화 없음). `passwords`는 dump/status/log에 직렬화되지 않으며, 화면에는 마스킹된 `•`만 그려진다(Task 13이 고정).

### 입력: id와 pw 둘 다 사용자가 입력 (추측 없음)

두 단계 modal 입력을 재사용한다. `InputMode::User`(id, ssh config의 `User`를 편집 가능한 프리필로 제시) → Enter → `InputMode::Password`(마스킹). 제출된 id·pw만 unlock에 사용되고, unlock은 `ssh -l <입력 id>`로 실행해 `login as:` 프롬프트를 아예 없앤다. ssh가 실제로 띄우는 `password:`/passphrase/호스트키 프롬프트에만 응답한다. 어떤 값도 추측하지 않는다.

---

### Task 1: `mux::is_locked` 분류자 (TDD)

**Files:**
- Modify: `src/mux/mod.rs` (next to `reason_is_no_sessions`, `src/mux/mod.rs:85`)
- Test: `src/mux/mod.rs` `mod tests`

- [ ] **1. Add the failing test** after the `reason_is_no_sessions` tests:
```rust
#[test]
fn is_locked_matches_only_the_ssh_auth_failure_signature() {
    // The canonical ssh auth-failure line (locked), and the exact "(" after
    // "Permission denied" that distinguishes it from a generic mux permission error.
    assert!(is_locked("pwtest@127.0.0.1: Permission denied (publickey,password)."));
    assert!(is_locked("command failed (exit 255): pwtest@127.0.0.1: Permission denied (publickey)."));
    assert!(is_locked("Permission denied (publickey,password,keyboard-interactive)."));
    // Reach failures and non-ssh permission errors are NOT locked.
    assert!(!is_locked("ssh: connect to host 192.0.2.1 port 22: Connection timed out"));
    assert!(!is_locked("Host key verification failed."));
    assert!(!is_locked("tmux: open /tmp/tmux-0/default: Permission denied"));
    assert!(!is_locked("no server running on /tmp/tmux-1000/default"));
}
```
- [ ] **2. Run: `cargo test mux::mod::tests::is_locked`**. Expected: compile failure (no function).
- [ ] **3. Implement** right after `reason_is_no_sessions` (same `pub(crate)` level):
```rust
/// True when `text` is ssh's canonical AUTH-failure line (`Permission denied (…`
/// with the rejected-methods list), meaning the host was REACHED but refused the
/// credentials: the locked state, distinct from unreachable. The `(` after
/// "Permission denied" is ssh's own signature; a generic mux permission error or
/// a reach failure ("Connection refused" / "Host key verification failed") does not
/// carry it. Conservative on purpose: a false positive invites a password entry on
/// a host that is merely down.
pub(crate) fn is_locked(text: &str) -> bool {
    text.contains("Permission denied (")
}
```
- [ ] **4. Verify: `cargo test mux::mod::tests::is_locked`** passes.
- [ ] **5. Commit: `feat(mux): classify the ssh auth-failure signature as locked`**

### Task 2: control reader surfaces the connection child's failure line (TDD)

**Files:**
- Modify: `src/link/reader.rs` (outside-block match at 82-87, EOF emit at 92-97)
- Test: `src/link/reader.rs` `mod tests`

- [ ] **1. Add the failing test** near the existing `reason`-carrying tests (feed canned lines to `run_reader`, assert the `Exited` reason):
```rust
#[test]
fn exited_reason_falls_back_to_the_connection_failure_line() {
    let mut reason = None;
    run_reader(
        "host",
        &test_control_proto(),
        ["pwtest@127.0.0.1: Permission denied (publickey,password).".to_string()]
            .into_iter(),
        &test_state(80, 24),
        &InFlight::default(),
        |ev| {
            if let HostEvent::Exited { reason: r, .. } = ev {
                reason = r;
            }
        },
    );
    assert_eq!(
        reason.as_deref(),
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).")
    );
}

#[test]
fn exited_reason_keeps_no_connection_failure_when_only_protocol_or_idle_lines_arrive() {
    let mut reason = None;
    run_reader(
        "host",
        &test_control_proto(),
        ["0: ksh* (1 panes)".to_string(), "".to_string()].into_iter(),
        &test_state(80, 24),
        &InFlight::default(),
        |ev| {
            if let HostEvent::Exited { reason: r, .. } = ev {
                reason = r;
            }
        },
    );
    assert_eq!(reason, None, "idle/protocol body lines are not a connection failure");
}
```
  (Check `test_control_proto`, `ReaderState::default`, `InFlight::default` availability in reader.rs tests; the crate's `link::test_control_proto` is used in `client.rs` tests via `use crate::link::test_control_proto`.)
- [ ] **2. Run: `cargo test link::reader`**. Expected: FAIL (reason is `None`).
- [ ] **3. Implement** in `run_reader`:
  - Add a local before the loop (next to `last_error`):
```rust
// The connection child's OWN failure line (ssh writes "Permission denied (…)"
// / "Connection refused" / … to the stream before dying), surfaced as the
// Exited reason only when no protocol %error named one - so an auth failure
// is told from a host that died silently.
let mut last_failure: Option<String> = None;
```
  - Change the outside-block match arm at 82-87:
```rust
// Stray frame/body outside a block.
Line::End { .. } | Line::Error { .. } => {}
Line::Body(line) => {
    if is_connection_failure(line) {
        last_failure = Some(line.trim().to_string());
    }
}
```
  - Change the EOF emit at 92-97:
```rust
// Iterator ended = child stdout EOF.
emit(HostEvent::Exited {
    host: host.to_string(),
    reason: last_error.or(last_failure),
});
```
  - Add the free helper in the same file:
```rust
/// True when `line` is the ssh connection child's own failure line (an auth
/// failure, a refused/timed-out connect, or a host-key trust failure) rather than
/// mux protocol content. The control stream for a REMOTE host is the ssh child's
/// pty, so its death messages arrive here as raw body lines.
fn is_connection_failure(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("permission denied")
        || l.contains("connection refused")
        || l.contains("connection timed out")
        || l.contains("no route to host")
        || l.contains("network is unreachable")
        || l.contains("host key verification failed")
        || l.contains("name or service not known")
}
```
- [ ] **4. Verify: `cargo test link::reader`** passes (and the existing `%error`-reason tests stay green).
- [ ] **5. Commit: `feat(link): surface the connection child's failure line as the exit reason`**

### Task 3: `Liveness::Locked` and the scan projection (TDD)

**Files:**
- Modify: `src/model/host.rs` (enum at 15-34), `src/provision/discovery.rs` (32)
- Test: `src/model/host.rs` `mod tests`

- [ ] **1. Add the failing test** in `host.rs` `mod tests`:
```rust
#[test]
fn from_scan_err_projects_auth_failure_to_locked_and_reach_failure_to_unreachable() {
    assert_eq!(
        Liveness::from_scan_err(&Some(
            "pwtest@127.0.0.1: Permission denied (publickey,password).".into()
        )),
        Liveness::Locked
    );
    assert_eq!(
        Liveness::from_scan_err(&Some("ssh: connect to host x: Connection refused".into())),
        Liveness::Unreachable
    );
    assert_eq!(Liveness::from_scan_err(&None), Liveness::Live);
}
```
- [ ] **2. Run: `cargo test model::host::tests::from_scan_err`**. Expected: compile failure (no `Locked`).
- [ ] **3. Implement**:
  - Add the variant and the projection:
```rust
pub enum Liveness {
    Connecting,
    Live,
    /// Reached but the credentials were refused (`Permission denied`): a locked
    /// host is the entry point to the password unlock, never a dead one.
    Locked,
    Unreachable,
}

impl Liveness {
    /// Projects a scan/ls outcome's optional error into reachability: `None` ⇒
    /// `Live`; an ssh auth-failure text ⇒ `Locked`; any other failure ⇒
    /// `Unreachable`. The scan path has no "connecting" state, so this is a
    /// three-way projection; the failure message itself is kept alongside
    /// (`Liveness` is `Copy` and holds none).
    pub fn from_scan_err(err: &Option<String>) -> Liveness {
        match err {
            Some(text) if crate::mux::is_locked(text) => Liveness::Locked,
            Some(_) => Liveness::Unreachable,
            None => Liveness::Live,
        }
    }
}
```
- [ ] **4. Verify: `cargo test model::host`** passes.
- [ ] **5. Commit: `feat(model): add the Locked liveness for auth-failed hosts`**

### Task 4: the row-model locked state (TDD)

**Files:**
- Modify: `src/ui/tree.rs` (RowRef at 199-204, flatten host-state card at 414-423, `host_state_word` at 338-344, `drop_hidden_unreachable`)
- Test: `src/ui/tree.rs` `mod tests`

- [ ] **1. Add the failing tests** (reuse the existing `sess`/`kind`/`drop_hidden_setup` helpers):
```rust
#[test]
fn flatten_marks_a_locked_host_as_locked() {
    let groups = vec![Group {
        source: "pwbox".into(),
        err: Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        sessions: vec![],
    }];
    let rows = flatten(&groups, &HashSet::new(), "", false, &mux_of_source);
    match &rows[0].reference {
        RowRef::Host { locked, unreachable, .. } => {
            assert!(*locked);
            assert!(*unreachable, "a locked host is still a failure (err set)");
        }
        other => panic!("expected a host card, got {other:?}"),
    }
}

#[test]
fn drop_hidden_unreachable_keeps_a_locked_host() {
    // hide=true prunes unreachable hosts, but a locked host is actionable (its
    // unlock view is the one entry point), so it must survive the prune.
    let groups = vec![
        Group { source: "local".into(), err: None, sessions: vec![sess("local", "web")] },
        Group {
            source: "pwbox".into(),
            err: Some("Permission denied (publickey,password).".into()),
            sessions: vec![],
        },
        Group { source: "deadhost".into(), err: Some("refused".into()), sessions: vec![] },
    ];
    let kept = drop_hidden_unreachable(&groups, &HashSet::new(), "");
    let sources: Vec<&str> = kept.iter().map(|g| g.source.as_str()).collect();
    assert_eq!(sources, vec!["local", "pwbox"], "locked survives, dead does not: {sources:?}");
}

#[test]
fn host_state_word_names_locked() {
    assert_eq!(host_state_word(true, false), "🔒 locked");
    assert_eq!(host_state_word(false, true), "⚠ unreachable");
    assert_eq!(host_state_word(false, false), "no sessions");
}
```
- [ ] **2. Run: `cargo test ui::tree`**. Expected: compile failures (no `locked` field, no new params).
- [ ] **3. Implement**:
  - `RowRef::Host` gains the field:
```rust
Host {
    source: String,
    unreachable: bool,
    locked: bool,
    scanning: bool,
},
```
  - `host_state_word` becomes two-argument:
```rust
pub(crate) fn host_state_word(locked: bool, unreachable: bool) -> &'static str {
    if locked {
        "🔒 locked"
    } else if unreachable {
        "⚠ unreachable"
    } else {
        "no sessions"
    }
}
```
  - In `flatten`'s host-state card block, derive `locked` alongside `unreachable` and pass it:
```rust
let unreachable = g.err.is_some();
let locked = g.err.as_deref().is_some_and(crate::mux::is_locked);
```
    and the `RowRef::Host { source: g.source.clone(), unreachable, locked, scanning: is_scanning }`.
  - `drop_hidden_unreachable` keeps a locked group (add one filter condition after the `scanning` clause):
```rust
|| crate::mux::is_locked(g.err.as_deref().unwrap_or_default())
```
  - Update the existing `host_state_word` call sites in `chrome.rs` (`word` at 257 and `siblings` at 297/300): `host_state_word(locked, other == ViewScreen::Unreachable)` and `host_state_word(is_locked, true)` / `host_state_word(is_locked, false)`. Compute `is_locked` from the same `g.err` the sibling rows read (Task 6 will touch `chrome.rs`; do the signature fix here so it compiles).
- [ ] **4. Verify: `cargo test ui::tree`** and `cargo build` pass.
- [ ] **5. Commit: `feat(ui): mark locked hosts in the row model and keep them out of the hide`**

### Task 5: the locked card mark (TDD)

**Files:**
- Modify: `src/ui/switcher/render.rs` (host-state card, the `⚠` branch at 703-710)
- Test: `src/ui/switcher/tests.rs`

- [ ] **1. Add the failing test** after the existing unreachable-card tests:
```rust
#[tokio::test]
async fn a_locked_host_card_reads_locked_with_the_lock_mark() {
    let mut h = Harness::new(sample());
    h.sw.apply_source_result(
        "pwbox".into(),
        Vec::new(),
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut h.state,
    );
    h.draw();
    let t = h.nav_text();
    assert!(t.contains("pwbox"), "the locked host keeps its card:\n{t}");
    assert!(t.contains("🔒"), "the card carries the lock mark:\n{t}");
}
```
  (Add a `"pwbox"` source to the harness sample or apply the result to an existing source; the exact harness helpers follow the sibling tests in `src/ui/switcher/tests.rs`.)
- [ ] **2. Run: `cargo test ui::switcher::tests::locked`**. Expected: FAIL (no lock mark).
- [ ] **3. Implement** in `render.rs` `nav_row_lines`, host-state card:
```rust
if *locked {
    // The lock mark rides the host row flush after the host name, in the same
    // danger colour as the unreachable mark: a locked host is a failure the
    // user can act on.
    line.push(Span::styled(
        "🔒",
        Style::default().fg(palette::get().warning),
    ));
} else if *unreachable {
    line.push(Span::styled(
        "⚠",
        Style::default().fg(palette::get().warning),
    ));
}
```
  (Update the existing `if *unreachable { … }` block to the `if *locked … else if *unreachable …` shape. The `RowRef::Host` destructure in `nav_row_lines` must add `locked`.)
- [ ] **4. Verify: `cargo test ui::switcher`** passes.
- [ ] **5. Commit: `feat(ui): render the locked host card with the lock mark`**

### Task 6: the locked view screen and the input entry (TDD)

**Files:**
- Modify: `src/ui/chrome.rs` (`ViewScreen` at 238-260, `render_view_screen` / `view_screen_lines` at 638-790), `src/ui/switcher/mod.rs` (`current_view_screen` at 727-757)
- Test: `src/app/runtime/tests.rs` (dump_screen)

- [ ] **1. Add the failing test** in `src/app/runtime/tests.rs`:
```rust
#[test]
fn a_locked_host_shows_the_locked_view_screen() {
    use crate::ui::run::dump_screen;
    let env = std::sync::Arc::new(fake_env_with_sources(&["pwbox"]));
    let (mut rt, _io) = Runtime::new(env);
    rt.switcher.apply_source_result(
        "pwbox".into(),
        Vec::new(),
        Some("pwtest@127.0.0.1: Permission denied (publickey,password).".into()),
        &mut rt.state,
    );
    let out = dump_screen(&mut rt.switcher, None, 80, 24, &rt.state);
    assert!(out.contains("locked"), "the locked view names its state:\n{out}");
    assert!(out.contains("pwbox"), "the host is on its own screen:\n{out}");
}
```
- [ ] **2. Run: `cargo test app::runtime::tests::locked`**. Expected: FAIL (no locked view; the card renders as `⚠ unreachable`).
- [ ] **3. Implement**:
  - `ViewScreen` gains a variant and a word:
```rust
/// The host answered the network but refused the credentials (`Permission
/// denied`): a locked host is reachable and awaiting the password unlock.
Locked,
```
```rust
fn word(self) -> &'static str {
    match self {
        ViewScreen::SelfSession => "running xmux",
        ViewScreen::Locked => "🔒 locked",
        other => crate::ui::tree::host_state_word(false, other == ViewScreen::Unreachable),
    }
}
```
  - `current_view_screen` in `switcher/mod.rs`: after the `unreachable` check, classify a locked `RowRef::Host`:
```rust
let (unreachable, locked) = match self.current_ref() {
    Some(RowRef::Host { unreachable, locked, .. }) => (*unreachable, *locked),
    _ => (false, false),
};
if locked {
    return Some(ViewScreen::Locked);
}
if unreachable {
    return Some(ViewScreen::Unreachable);
}
```
  - `view_screen_lines` in `chrome.rs`: the `Locked` branch renders the same rows the Unreachable branch builds (reason, failures, mux/machine/socket/probe, provider, ssh config stanza, same-machine siblings, log), followed by one extra row naming the unlock command. Concretely, extract the Unreachable row-building into a helper and call it from both branches:
```rust
} else if kind == ViewScreen::Unreachable || kind == ViewScreen::Locked {
    // WHY the host is not live: the failure text ssh reported, then what was
    // asked and of what, then where it came from and how it is configured.
    // Locked adds one row after the log: the unlock command the user's id and
    // password will drive.
    let reason = state.groups.iter().find(|g| g.source == source)
        .and_then(|g| g.err.clone())
        .unwrap_or_else(|| "connection closed".into());
    rows.push((ScreenCell::Label("reason"), reason));
    if let Some(runs) = state.failure_runs.get(source) {
        rows.push((ScreenCell::Label("failures"), failure_run_words(*runs)));
    }
    rows.push((ScreenCell::Gap, String::new()));
    if let Some(reach) = self.source_reach.get(source) {
        if !reach.mux.is_empty() { rows.push((ScreenCell::Label("mux"), reach.mux.clone())); }
        if !reach.machine.is_empty() { rows.push((ScreenCell::Label("machine"), reach.machine.clone())); }
        if !reach.socket.is_empty() { rows.push((ScreenCell::Label("socket"), reach.socket.clone())); }
        if !reach.probe.is_empty() { rows.push((ScreenCell::Label("probe"), reach.probe.clone())); }
    }
    // … provider / ssh config stanza / same-machine siblings / log rows (same as
    // the existing Unreachable branch) …
    if kind == ViewScreen::Locked {
        rows.push((ScreenCell::Label("unlock"),
            "Enter a username, then the masked password; xmux answers the ssh prompt and establishes one authenticated connection the rest reuses".into()));
        // When the transport has no reusable master (Windows), `source_reach`
        // carries no socket row and the unlock command is unavailable: add a row
        // stating it, so the screen never invites an action that cannot work.
        if self.source_reach.get(source).map(|r| r.socket.is_empty()).unwrap_or(true) {
            rows.push((ScreenCell::Label("unlock"),
                "unavailable on this platform: ssh has no reusable ControlMaster here".into()));
        }
    }
    rows.push((ScreenCell::Gap, String::new()));
} else { … }
```
- [ ] **4. Verify: `cargo test app::runtime`** and `cargo test ui::switcher` pass.
- [ ] **5. Commit: `feat(ui): show the locked view screen for auth-failed hosts`**

### Task 7: the masked password input and the unlock submit (TDD)

**Files:**
- Modify: `src/ui/modal.rs` (`InputMode` at 46-65, `input_title` at 603-609, `input_segments` at 458-498), `src/ui/switcher/input.rs` (submit at 279-316, `open_*` helpers)
- Test: `src/ui/modal.rs` `mod tests`, `src/ui/switcher/tests.rs`

- [ ] **1. Add the failing tests**:
```rust
// modal.rs
#[test]
fn a_password_input_renders_only_bullets_for_its_buffer() {
    let mut input = Input::new(InputMode::Password, " password".into(), "hunter2".into(), None);
    assert_eq!(input_hint_text(&input, 40), "[password] password: •••••••");
    // The caret still tracks the real buffer, not the bullets.
    input.cursor = 0;
    let (_, _, before, at, after) = input_segments(&input, 40);
    assert_eq!(format!("{before}{at}{after}"), "•••••••");
}
```
```rust
// switcher/tests.rs - the two-step unlock input flow
#[tokio::test]
async fn locked_host_enter_opens_the_user_input_then_the_masked_password_input() {
    let mut h = Harness::new(sample());
    h.sw.apply_source_result("pwbox".into(), Vec::new(),
        Some("Permission denied (publickey,password).".into()), &mut h.state);
    h.draw();
    h.key(KeyCode::Enter).await;           // open the unlock input (User step)
    assert!(h.sw.state().modal.is_some(), "the user input modal is open");
    // type the id, Enter → the password step
    h.ch('a'); h.ch('l'); h.ch('i'); h.ch('c'); h.ch('e');
    h.key(KeyCode::Enter).await;
    let pw = h.sw.state().modal.as_ref().and_then(|m| match m {
        Modal::Input(i) if i.mode == InputMode::Password => Some(i.buffer.clone()),
        _ => None,
    });
    assert!(pw.is_some(), "the masked password modal is open");
    assert_eq!(h.hint_bar_text(), /* masked bullets, no plaintext */ "");
}
```
  (Exact harness calls follow the sibling create-input tests; `state.modal` is read via `h.sw.state()`, or the Switcher exposes the modal through the existing test-support helpers.)
- [ ] **2. Run: `cargo test ui::modal ui::switcher`**. Expected: FAIL (no `InputMode::User`/`Password`).
- [ ] **3. Implement**:
  - `InputMode` gains two variants:
```rust
/// The unlock username step (unmasked, prefilled from the run's saved secret for
/// this host, else empty - the user always confirms the id by typing/submitting
/// it). Distinct from `New`: its Enter opens the password step, not a create.
User,
/// The unlock password step: the buffer renders masked (bullets) and its Enter
/// submits the unlock.
Password,
```
  - `input_title`:
```rust
InputMode::User => "user",
InputMode::Password => "password",
```
  - `input_segments`: mask the buffer for `Password`:
```rust
let chars: Vec<char> = if input.mode == InputMode::Password {
    input.buffer.chars().map(|_| '•').collect()
} else {
    input.buffer.chars().collect()
};
```
  - `input.rs` Enter submit: route `Password` to `queue_unlock(source, &user, &val, state)` and `User` to opening the password step (carrying the entered user). Add:
```rust
InputMode::User => {
    self.close_input(state);
    self.open_unlock_password(source, &val, state);
    Vec::new()
}
InputMode::Password => {
    self.close_input(state);
    self.queue_unlock(source, &val, state)
}
```
  - `queue_unlock` applies the action (Task 8's `Action::Unlock`):
```rust
fn queue_unlock(&mut self, source: Option<String>, password: &str, state: &mut State) -> Vec<Command> {
    let Some(source) = source else { return Vec::new(); };
    let user = state.current_unlock_user(&source);
    state.apply(crate::model::Action::Unlock { source, user, password: password.to_string() })
}
```
  - The two open helpers, mirroring `open_new` (capture the source up front):
```rust
pub(super) fn open_unlock_user(&mut self, state: &mut State) { /* prefilled user, mode User */ }
fn open_unlock_password(&mut self, source: Option<String>, user: &str, state: &mut State) { /* masked, label " password for <user>@<host>" */ }
```
- [ ] **4. Verify: `cargo test ui::modal ui::switcher`** pass.
- [ ] **5. Commit: `feat(ui): add the masked unlock input modes (user, password)`**

### Task 8: the unlock action, command, and in-memory secret (TDD)

**Files:**
- Modify: `src/model/action.rs` (`Action` at 31-99, `Command` at 104-130), `src/state/mod.rs` (fields at 11-54, `apply` at 160, `apply_source_result` at 884-910)
- Test: `src/state/mod.rs` `mod tests`

- [ ] **1. Add the failing test** in `state/mod.rs` `mod tests`:
```rust
#[test]
fn unlock_action_stores_the_secret_and_emits_the_run_command() {
    let mut state = crate::state::State::from_sources(vec!["pwbox".into()]);
    let cmds = state.apply(crate::model::Action::Unlock {
        source: "pwbox".into(),
        user: "alice".into(),
        password: "hunter2".into(),
    });
    assert!(matches!(&cmds[..], [crate::model::Command::RunUnlock { user, password, .. }]
        if user == "alice" && password == "hunter2"));
    assert_eq!(state.secret_for("pwbox").map(|s| (s.user.clone(), s.secret.clone())),
        Some(("alice".to_string(), "hunter2".to_string())));
}

#[test]
fn a_host_that_becomes_unreachable_forgets_its_secret() {
    let mut state = crate::state::State::from_sources(vec!["pwbox".into()]);
    state.apply(crate::model::Action::Unlock {
        source: "pwbox".into(), user: "alice".into(), password: "hunter2".into(),
    });
    // A non-locked failure (dead host) clears the secret; a locked failure keeps it.
    state.groups[0].err = Some("ssh: connect to host x: Connection refused".into());
    assert_eq!(state.secret_for("pwbox"), None, "unreachable forgets the secret");
}
```
- [ ] **2. Run: `cargo test state::mod::tests::unlock`**. Expected: compile failure.
- [ ] **3. Implement**:
  - `action.rs`:
```rust
/// Unlock a locked host: store the user's id+password in memory (the run's
/// secret, reused on retry) and run the off-loop unlock worker. The password is
/// never persisted and never leaves the process except into the unlock ssh's pty.
Unlock { source: String, user: String, password: String },
```
```rust
/// Run the off-loop ssh unlock for a locked host with the submitted id+password.
RunUnlock { source: String, user: String, password: String },
```
  - `state/mod.rs`: a field and accessors:
```rust
/// In-memory unlock secrets per source (id + password), kept for the run only:
/// reused when the same host relocks, cleared when the host dies (not locks),
/// and never rendered or serialized. `dump`/`status`/logs carry none of it.
pub(crate) secrets: HashMap<String, StoredSecret>,
```
```rust
#[derive(Clone)]
pub(crate) struct StoredSecret { pub(crate) user: String, pub(crate) secret: String }
```
  Accessors next to the other state getters:
```rust
/// The stored secret for `source`, if one is kept for the run.
pub(crate) fn secret_for(&self, source: &str) -> Option<&StoredSecret> {
    self.secrets.get(source)
}
/// The username an unlock starts with: the stored secret's user, else empty.
/// The user always confirms the id before it is used (nothing is guessed).
pub(crate) fn current_unlock_user(&self, source: &str) -> String {
    self.secrets.get(source).map(|s| s.user.clone()).unwrap_or_default()
}
```
  - In `State::apply`, the arm:
```rust
Action::Unlock { source, user, password } => {
    self.secrets.insert(source.clone(), StoredSecret { user, secret: password.clone() });
    vec![Command::RunUnlock { source, user, password }]
}
```
  - In `apply_source_result` (or the group-err setter), when a result's `err` is `Some` and NOT locked, forget the source's secret:
```rust
if let Some(e) = &err {
    if !crate::mux::is_locked(e) {
        self.secrets.remove(&source);
    }
}
```
- [ ] **4. Verify: `cargo test state`** passes.
- [ ] **5. Commit: `feat(model): unlock action with the in-memory run secret`**

### Task 9: the unlock worker (TDD)

**Files:**
- Create: `src/link/unlock.rs`
- Modify: `src/link/mod.rs` (declare `mod unlock`), `src/link/client.rs` (`spawn_pty_child` visibility `pub(super)`), `src/link/reader.rs` (nothing)
- Test: `src/link/unlock.rs` `mod tests` (pure answerer) + `#[ignore]` live gate

- [ ] **1. Add the pure answerer tests** (no pty; canned byte chunks):
```rust
#[test]
fn answerer_sends_yes_then_the_password_and_detects_wrong_password() {
    let mut a = Answerer::new("hunter2".into());
    // host-key prompt first, then the password prompt
    let writes = a.feed("The authenticity of host 'x' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ");
    assert_eq!(writes, vec![PromptWrite::HostKey]);
    let writes = a.feed("alice@x's password: ");
    assert_eq!(writes, vec![PromptWrite::Password]);
    // wrong password → the re-prompt after one try
    let writes = a.feed("Permission denied, please try again.\nalice@x's password: ");
    assert_eq!(writes, vec![PromptWrite::Done(UnlockOutcome::AuthFailed)]);
}

#[test]
fn answerer_reports_success_only_after_auth_resolved() {
    let mut a = Answerer::new("hunter2".into());
    let _ = a.feed("alice@x's password: ");
    assert_eq!(a.feed(""), vec![PromptWrite::None]);
    a.confirm_exit(0);
    assert_eq!(a.outcome(), Some(UnlockOutcome::Ok));
}
```
- [ ] **2. Run: `cargo test link::unlock`**. Expected: compile failure (no module).
- [ ] **3. Implement `src/link/unlock.rs`**:
  - The pure answerer (side-effect-free, unit-tested):
```rust
/// The pty prompt-answer state machine for one unlock: fed the ssh child's output
/// chunks, it returns what to write next, and finally the outcome. Pure so the
/// prompt logic is testable without a pty.
pub(crate) enum PromptWrite { None, HostKey, Password, Done(UnlockOutcome) }
pub(crate) enum UnlockOutcome { Ok, AuthFailed, Timeout, Unavailable, Failed(String) }

pub(crate) struct Answerer {
    secret: String,
    answered: bool,
    done: Option<UnlockOutcome>,
}

impl Answerer {
    pub(crate) fn new(secret: String) -> Self {
        Self { secret, answered: false, done: None }
    }
    pub(crate) fn feed(&mut self, chunk: &str) -> Vec<PromptWrite> {
        if self.done.is_some() {
            return Vec::new();
        }
        // A re-prompt right after "please try again" is a wrong password: fail fast
        // (no 3-attempt loop). Only one prompt is answered per unlock.
        if self.answered && chunk.contains("Permission denied, please try again.") {
            self.done = Some(UnlockOutcome::AuthFailed);
            return vec![PromptWrite::Done(UnlockOutcome::AuthFailed)];
        }
        if !self.answered {
            if chunk.contains("yes/no/[fingerprint]") {
                self.answered = true; // host key: answer, then expect the password
                return vec![PromptWrite::HostKey];
            }
            if chunk.contains("assword:") {
                self.answered = true;
                return vec![PromptWrite::Password];
            }
        }
        Vec::new()
    }
    pub(crate) fn confirm_exit(&mut self, code: i32) {
        if self.done.is_none() {
            self.done = Some(if code == 0 { UnlockOutcome::Ok } else { UnlockOutcome::Failed(format!("ssh exit {code}")) });
        }
    }
    pub(crate) fn outcome(&self) -> Option<UnlockOutcome> { self.done.clone() }
}
```
  - The pty wrapper (POSIX; reuses `spawn_pty_child`). The loop reads the master on a spawned thread (a blocking `read` with no bound must not run on the async task), feeds the answerer, writes what it returns, and watches the child exit + a `recv_timeout` deadline:
```rust
/// Runs the unlock over a pty: spawn the `ControlMaster=yes` ssh on a pty
/// (`spawn_pty_child` gives it the controlling tty), answer its prompts, and
/// return the outcome. Bounded by `timeout`; on success the ControlMaster
/// socket holds an authenticated connection every later channel reuses.
#[cfg(unix)]
pub(crate) async fn unlock_host(
    transport: &dyn Transport,
    user: &str,
    password: &str,
    timeout: std::time::Duration,
) -> UnlockOutcome {
    let Some(argv) = transport.unlock_argv(user) else {
        return UnlockOutcome::Unavailable;
    };
    // Spawn on a pty. Blocking pty I/O stays off the loop: this whole function
    // runs in the app's spawned task, and the read below is further moved onto a
    // thread so a stall cannot hold the tokio task.
    let spawned = match super::client::spawn_pty_child(&argv, &[], 80, 24) {
        Ok(s) => s,
        Err(e) => return UnlockOutcome::Failed(e.to_string()),
    };
    let mut child = spawned.child;
    let mut reader = spawned.stdout;
    let mut writer = spawned.stdin;
    let mut answerer = Answerer::new(password.to_string());

    // A thread drains the pty master into a channel; the loop below is the only
    // prompt logic.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => { if tx.send(buf[..n].to_vec()).is_err() { break; } }
                Err(_) => break,
            }
        }
    });

    loop {
        // Bound the whole exchange; nothing else can ever extend it.
        match rx.recv_timeout(timeout) {
            Ok(chunk) => {
                let text = String::from_utf8_lossy(&chunk);
                for action in answerer.feed(&text) {
                    match action {
                        PromptWrite::None => {}
                        PromptWrite::HostKey => { let _ = writer.write_all(b"yes\n"); }
                        PromptWrite::Password => {
                            let _ = writer.write_all(answerer.secret_with_newline());
                        }
                        PromptWrite::Done(o) => return o,
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                return UnlockOutcome::Timeout;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // The child exited: its exit code is the definitive auth verdict (0 = the
        // master is established).
        if let Some(code) = child.try_wait().ok().flatten() {
            answerer.confirm_exit(code.exit_code());
            return answerer.outcome().unwrap_or(UnlockOutcome::Failed(
                format!("ssh exit {}", code.exit_code()),
            ));
        }
    }
    answerer.outcome().unwrap_or(UnlockOutcome::Failed("unlock closed".into()))
}
```
  Add `Answerer::secret_with_newline()` returning `format!("{}\n", self.secret).into_bytes()` and `Answerer::feed` returns `Vec<PromptWrite>` (it already does). `child.kill()` is available via the `ChildKiller` trait portable-pty exposes on the boxed child.
- [ ] **4. Verify: `cargo test link::unlock`** passes. Add an `#[ignore]` live gate mirroring `tmp/pw_unlock_proto.py` (spawns the local `pwtest@127.0.0.1`, asserts `Ok` and that a following `BatchMode=yes` ssh reuses the socket), documented as the live verification.
- [ ] **5. Commit: `feat(link): the pty prompt-answer unlock worker`**

### Task 10: `Transport::unlock_argv` (TDD)

**Files:**
- Modify: `src/transport/mod.rs` (trait at 25-95), `src/transport/ssh.rs` (impl)
- Test: `src/transport/ssh.rs` `mod tests`

- [ ] **1. Add the failing tests**:
```rust
#[test]
fn ssh_unlock_argv_forces_a_master_with_the_same_control_path() {
    let got = ssh("prod", "linux", "/tmp/cm.sock").unlock_argv("alice").unwrap();
    assert_eq!(got[0], "ssh");
    let joined = got.join(" ");
    assert!(joined.contains("ControlMaster=yes"), "{joined}");
    assert!(joined.contains("ControlPath=/tmp/cm.sock"), "{joined}");
    assert!(joined.contains("-l alice"), "{joined}");
    assert!(!joined.contains("BatchMode"), "the unlock must be able to prompt: {joined}");
    assert_eq!(got.last().unwrap(), "true");
    assert!(joined.ends_with("-- prod true"), "{joined}");
}

#[test]
fn ssh_unlock_argv_is_none_on_windows() {
    assert_eq!(ssh("prod", "windows", "").unlock_argv("alice"), None,
        "no ControlMaster on Windows to reuse");
}
```
- [ ] **2. Run: `cargo test transport::ssh`**. Expected: compile failure (no method).
- [ ] **3. Implement**:
  - Trait (after `raw_shell_argv`):
```rust
/// The argv that establishes a password-authenticated ControlMaster for this
/// machine (the unlock), or `None` when the machine has no reusable master (a
/// local/WSL machine with no password, or Windows ssh without ControlMaster).
fn unlock_argv(&self, _user: &str) -> Option<Vec<String>> { None }
```
  - `Ssh` impl (reuses `CONNECT_TIMEOUT` and the same `control_path` every other ssh shares):
```rust
fn unlock_argv(&self, user: &str) -> Option<Vec<String>> {
    if self.os == "windows" {
        return None; // no ControlMaster socket to leave authenticated
    }
    let mut v = vec![
        "ssh".to_string(),
        "-o".into(),
        "ControlMaster=yes".into(),
        "-o".into(),
        format!("ControlPath={}", self.control_path),
        "-o".into(),
        "ControlPersist=60s".into(),
        "-o".into(),
        format!("ConnectTimeout={CONNECT_TIMEOUT}"),
        "-l".into(),
        user.to_string(),
        "--".into(),
        self.alias.clone(),
        "true".into(),
    ];
    Some(v)
}
```
  - `Box<dyn Transport>` delegation arm in `mod.rs`:
```rust
fn unlock_argv(&self, user: &str) -> Option<Vec<String>> {
    (**self).unlock_argv(user)
}
```
- [ ] **4. Verify: `cargo test transport`** passes.
- [ ] **5. Commit: `feat(transport): the ssh unlock argv (ControlMaster=yes, -l user)`**

### Task 11: the app wiring (TDD)

**Files:**
- Modify: `src/app/runtime/input.rs` (Enter routing at 286, dispatch), `src/app/runtime/mod.rs` (`dispatch_commands` at 192-230, a `spawn_unlock` next to `spawn_op` at 1342), `src/ui/switcher/mod.rs` (`current_host_unreachable` at 718-721, `open_new` gate at 153), `src/state/mod.rs` (`apply_source_result`)
- Test: `src/app/runtime/tests.rs`

- [ ] **1. Add the failing test** (the full flow, harness + fake transport):
```rust
#[test]
fn submitting_the_unlock_runs_the_worker_and_a_success_kicks_a_rescan() {
    // drive: locked host -> Enter opens user input -> type id -> Enter -> type
    // password -> Enter -> assert Command::RunUnlock was emitted; then simulate
    // an UnlockOutcome::Ok -> assert the rescan kick fired.
}
```
  (Concrete steps follow the create-op test shape in `src/app/runtime/tests.rs`; the unlock result is folded via a new op-style channel the app drains.)
- [ ] **2. Implement**:
  - In `handle_nav_bytes`, on `Some(Action::FocusTerminal)`: if `switcher.current_host_locked()`, open the unlock user input too:
```rust
Some(Action::FocusTerminal) => {
    focus_terminal = true;
    if switcher.current_host_locked() {
        key_cmds.extend(switcher.open_unlock_user(state));
    }
}
```
  - `current_host_locked` on the switcher (mirrors `current_host_unreachable`):
```rust
fn current_host_locked(&self) -> bool {
    matches!(self.current_ref(), Some(RowRef::Host { locked: true, .. }))
}
```
  - The blocked-host gate: `open_new`'s check becomes "unreachable OR locked" (a locked host cannot create a session before unlock):
```rust
if self.current_host_blocked() {
    state.flash("host locked or unreachable, cannot create here");
    return;
}
```
    with `current_host_blocked = current_host_unreachable() || current_host_locked()`.
  - `dispatch_commands` gains the `RunUnlock` arm:
```rust
Command::RunUnlock { source, user, password } => {
    spawn_unlock(hosts, &source, &user, &password, unlock_tx);
}
```
  - `spawn_unlock` (off the loop, mirroring `spawn_op`):
```rust
fn spawn_unlock(
    hosts: &crate::model::Hosts,
    source: &str,
    user: &str,
    password: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<UnlockResult>,
) {
    let Some(host) = hosts.get(source) else { return };
    let transport = host.transport.clone();
    let user = user.to_string();
    let password = password.to_string();
    let tx = tx.clone();
    tokio::spawn(async move {
        let outcome = crate::link::unlock::unlock_host(
            &*transport, &user, &password,
            std::time::Duration::from_secs(crate::transport::ssh::CONNECT_TIMEOUT.parse().unwrap_or(5) + 10),
        ).await;
        let _ = tx.send(UnlockResult { source: source.to_string(), outcome });
    });
}
```
  - The app loop drains `UnlockResult`; `Ok` → `switcher.request_rescan(state)` (kick the existing re-scan so the host re-enumerates over the master); `AuthFailed` → `state.flash("authentication failed")`; `Timeout` → flash; `Unavailable` → flash.
- [ ] **3. Verify: `cargo test app::runtime`** passes.
- [ ] **4. Commit: `feat(app): wire the unlock submit, worker, and post-unlock rescan`**

### Task 12: the live gate

- [ ] Run the `#[ignore]` live gate from Task 9 against the local sshd test account (mirrors `tmp/pw_unlock_proto.py`): it must establish the master and prove a following `BatchMode=yes` ssh reuses it. Also run the real classification against the account (`ssh -o BatchMode=yes pwtest@127.0.0.1 true` → `Permission denied (publickey,password)`). Report results in the PR body.

### Task 13: security pins (TDD)

- [ ] **1. Add the tests**:
  - `dump_screen` with an open password input shows only bullets, never the secret.
  - The ctl `status`/`dump` output with a stored secret contains no secret text.
  - A `StoredSecret` is not `Debug`-printed anywhere: `grep -rn "secrets" src/` review.
  - The unlock argv contains no password (the password goes only through the pty writer): assert `Ssh::unlock_argv(user)` argv has no `user`-password and `transport::ssh` tests pin the argv shape.
- [ ] **2. Verify: `cargo test`** passes.
- [ ] **3. Commit: `test(security): pin the unlock secret out of screens, logs, and argv`**

### Task 14: documentation

- [ ] **1.** `docs/requirements.md`: add FR-B25 (locked state, classification, unlock, memory-only secret, Windows boundary) after FR-B24.
- [ ] **2.** `CONTEXT.md`: add a `locked` glossary entry (reachable but auth-failed; the unlock entry point; not hidden by `hide-unreachable`).
- [ ] **3.** `README.md` / `README.ko.md`: one sentence on the locked host and the unlock key, in the same register as the existing ssh/remote prose.
- [ ] **4.** `src/ui/AGENTS.md`: Invariants - the locked host is a failure the user can act on (its card is never hidden, its switch/create is blocked, its unlock uses only the submitted id+password); the classification reads only ssh's own failure text.
- [ ] **5.** `src/link/AGENTS.md` / `src/model/AGENTS.md`: the reader surfaces the connection child's failure line; the unlock establishes the one authenticated master; the secret is in-memory only.
- [ ] **6.** Verify: `cargo fmt --check`, `cargo test`. Commit: `docs: document the locked host and the unlock`.

### Task 15: the full gate

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. App/UI/link changes are strongly coupled, so run the whole suite. Fix fmt/clippy in the same or a `style:` commit. No AI attribution trailers.

### Task 16: design consistency re-review before merge

- [ ] Re-read `CONTEXT.md`, `docs/requirements.md`, `src/ui/AGENTS.md`, `src/link/AGENTS.md` and confirm they match the code: the locked definition, the classification signal (ssh's own text only), the unlock mechanism (one authenticated ControlMaster), the memory-only secret, and the Windows boundary.

## Files to Modify

- `src/mux/mod.rs`: `is_locked` + tests.
- `src/link/reader.rs`: `last_failure` capture, `is_connection_failure`, Exited reason fallback + tests.
- `src/model/host.rs`: `Liveness::Locked` + `from_scan_err` + tests.
- `src/provision/discovery.rs`: (no change; `ScanResult::liveness` uses the updated `from_scan_err`).
- `src/ui/tree.rs`: `RowRef::Host { locked }`, `flatten` locked derivation, `host_state_word(locked, unreachable)`, `drop_hidden_unreachable` keeps locked + tests.
- `src/ui/switcher/render.rs`: the locked card mark (🔒).
- `src/ui/switcher/mod.rs`: `current_view_screen` → `ViewScreen::Locked`, `current_host_locked`, `open_new` gate, `open_unlock_user`/`open_unlock_password`.
- `src/ui/chrome.rs`: `ViewScreen::Locked`, `word`, `view_screen_lines` locked rows.
- `src/ui/modal.rs`: `InputMode::User`/`Password`, `input_title`, masked `input_segments`.
- `src/ui/switcher/input.rs`: submit routing, `queue_unlock`.
- `src/model/action.rs`: `Action::Unlock`, `Command::RunUnlock`.
- `src/state/mod.rs`: `secrets` field + `StoredSecret`, `apply` arm, `apply_source_result` forget-on-unreachable.
- `src/link/unlock.rs` (new): `Answerer`, `unlock_host`.
- `src/link/client.rs`: `spawn_pty_child` → `pub(super)`.
- `src/link/mod.rs`: declare `mod unlock`.
- `src/transport/mod.rs` + `src/transport/ssh.rs`: `unlock_argv`.
- `src/app/runtime/input.rs` + `mod.rs`: Enter-on-locked, `spawn_unlock`, rescan-on-success.
- `docs/requirements.md`, `CONTEXT.md`, `README.md`, `README.ko.md`, `src/ui/AGENTS.md`, `src/link/AGENTS.md`.

## New Files

- `src/link/unlock.rs`

## Risks

- **The reader's raw-line capture**: `is_connection_failure` matches ssh's own failure words in the control pty stream; a mux line that coincidentally contains one (unlikely: "no route to host" etc. are not mux vocabulary) could mislabel a reason. The fallback only fires when no protocol `%error` named a reason, and it only feeds `is_locked`'s strict signature, so a misfire is bounded to a cosmetic reason text.
- **False-positive locked**: `is_locked` matches only ssh's `Permission denied (` form, never a bare "permission denied", so a reachable host whose mux fails with a generic permission error stays unreachable, not locked. Pinned by Task 1 tests.
- **ControlPersist expiry**: the unlock master lives `ControlPersist=60s` after the last connection; idle past that, the host relocks and the user re-enters (the in-memory secret prefills the fields, so it is one Enter). This is the issue's memory-only lifetime, not a guessed value.
- **Windows**: unlock is unavailable where there is no ControlMaster (`unlock_argv` returns `None`; the locked view states it). The locked classification still works on Windows; only the unlock path is gated. This is a defined platform boundary, not a guess about Windows ssh behavior.
- **The Enter routing special case**: Enter on a locked card opens the unlock input in addition to focusing the terminal. The gate is `current_host_locked()`, which reads the same `RowRef` the render reads, so the routing and the screen cannot disagree.
- **Comment/document rules**: all src comments English, current-state only, no history narration, no em-dash/en-dash. Korean docs in neutral technical register.

## Execution notes (simplifications applied)

The implementation was simplified after the plan, per the review pass. The three
changes keep the feature complete while cutting dead code and a security surface:

- **`Liveness::Locked` dropped.** `Liveness` is written but never read in
  production (the tree derives the locked/unreachable state from the group `err`
  directly), so the added variant and its three-way `from_scan_err` were dead.
  The locked classification lives where it is read: `is_locked` at the tree,
  chrome, and state sites.
- **In-memory secret store dropped.** `state.secrets` / `StoredSecret` / the
  relock prefill are gone. The password rides only the transient
  `Command::RunUnlock` and the PTY writer, then is dropped; a relocked host asks
  for the credentials again. The id is still carried across the two input steps
  on the `Input` itself (never guessed), and `Action::Unlock` (a state-less
  pass-through) was removed in favour of emitting `Command::RunUnlock` directly.
- **The ControlMaster-socket success check dropped.** The pty-spawned child exits
  with 0 after a successful auth (verified by the live gate against a real
  password-only account), so the child's zero exit alone is the success signal.
- **FR-B25 was taken** (nav sides), so the requirement landed as FR-B26.

### The classification path was reordered (supersedes Task 2)

Live testing on a real remote (a tailnet peer with key auth disabled) showed the
locked host reading as unreachable. Task 2's premise, that the control reader would
see ssh's `Permission denied (` line and surface it as the exit reason, was wrong for
a remote: the `-CC` child is a PIPED spawn whose stderr is drained, and ssh writes an
auth failure to stderr, not the stdout the reader parses. The reader read zero lines,
hit EOF, and the exit fell back to "connection closed", which `is_locked` does not
match.

The fix reorders discovery so classification no longer depends on the control stream:

- **A machine reachability probe leads.** Each machine is probed once (`ssh <machine>
  true`) through the exec runner, which captures stderr, bounded at 12. Its outcome
  classifies the machine: a zero exit is connected, ssh's auth-failure line is locked,
  any other failure is unreachable. Only a connected machine goes on to mux discovery,
  detection, and its metadata channels; a failed one classifies its cards and opens no
  channel.
- **A failed detection opens no channel.** `DispatchScanned` ensures a channel only
  when the mux resolved, so a doomed control child never spawns to die and overwrite
  the machine's real reason with "connection closed".
- **The reader's connection-failure capture was removed** (`is_connection_failure`,
  `last_failure`, and their two tests). It was dead for a piped remote spawn, and the
  machine probe is now the single reachability classifier. The reader keeps only the
  protocol `%error` reason (a reachable-but-empty mux).
