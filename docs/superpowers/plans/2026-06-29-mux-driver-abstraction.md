# MuxDriver 추상화 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** supervisor가 mux 종류를 모른 채 "의도(표시할 세션·윈도우)"만 전달하도록, 호스트당 `Box<dyn MuxDriver>` 경계를 도입한다 — 단계 2(동작 보존 seam)를 무회귀로 완성한 뒤, 가능하면 단계 3(결정 이동)을 라이브 게이트 직전까지 진행한다.

**Architecture:** `MuxDriver` trait + 주입식 `DriverCtx`(스폰 능력·레지스트리·transport·control·attach_seq를 supervisor가 빌려줌). 단계 2에서는 단일 `SeamDriver`가 현재 free 함수(`select_attach`/`registry.grid`/`registry.input`/reap)에 **그대로 위임**해 동작을 비트 단위로 보존한다. 단계 3에서 tmux/psmux별 driver가 결정과 `host.display` 북키핑을 흡수하고 `SelectOutcome` match를 supervisor에서 제거한다.

**Tech Stack:** Rust, tokio current-thread, ratatui/crossterm, portable-pty(ConPTY), async-trait.

## Global Constraints

- 실툴체인만: `~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/{cargo,rustc,rustdoc}.exe`, `RUSTC`/`RUSTDOC` 설정. `cargo test`는 바이너리를 리빌드하지 않음 — 라이브 실행 전 별도 `cargo build`.
- 게이트: 542 테스트 / clippy 0 / fmt 0. 각 태스크 종료 시 전부 그린.
- `4a5f053`(로컬 psmux per-session attach 정합성 수정)은 **절대 회귀 금지**.
- LSP/subagent "green" 주장 불신 — 실제 `cargo`로 검증.
- AS-IS 문서/주석 — 변경 서사 금지(git이 기록).
- 헤드리스 `psmux attach` 실험 금지(라이브 psmux 오염). read-only 프로브만.
- 커밋은 태스크별 자율; push는 사람 승인. 커밋 트레일러 필수(Co-Authored-By + Claude-Session).
- big-bang 금지 — 단계별 그린 게이트.

---

## File Structure

- `src/driver.rs` (Create): `MuxDriver` trait, `Target`, `DriverCtx<'_>`, `SeamDriver`. 단계 2의 동작 보존 어댑터.
- `src/lib.rs` (Modify): `pub mod driver;` 등록.
- `src/cockpit.rs` (Modify): 루프의 `select_attach`/`registry.grid`/`registry.input`/reap 호출 지점을 `driver.*`로 라우팅. 단계 3에서 `SelectOutcome` match 6곳 제거.

단계 2의 핵심 판단: `SeamDriver`는 per-host 상태를 **아직 들지 않는다.** supervisor 소유 자원(`&mut Hosts`/`&mut AttachRegistry`/`&DisplayWorker`/`&HostManager`/`&mut u64`)을 `DriverCtx`로 빌려 기존 free 함수를 그대로 호출한다 — borrow 형태가 오늘과 동일해 무회귀.

---

## Task 1: MuxDriver trait + Target + DriverCtx + SeamDriver (위임 어댑터)

**Files:**
- Create: `src/driver.rs`
- Modify: `src/lib.rs` (mod 등록)
- Test: `src/driver.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::cockpit::{Selection, select_attach, display_key}`, `crate::model::Hosts`, `crate::proxy::registry::AttachRegistry`, `crate::display::DisplayWorker`, `crate::host::HostManager`, `crate::proxy::run::PtyEvent`, `crate::proxy::screen::Grid`.
- Produces:
  - `Target { session: String, window: Option<i64> }` (+ `from_selection(&Selection) -> Target`, `into_selection(&self, source: &str) -> Selection`).
  - `trait MuxDriver { fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool; fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>>; fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx); }`
  - `struct SeamDriver;` impl `MuxDriver`.
  - `struct DriverCtx<'a> { … }` (필드는 Step 3 참조).

설계 메모: 단계 2의 trait는 supervisor가 실제 호출하는 메서드만 둔다(`show`/`grid`/`input`). `pump`/`sync`/`shown`/`reattach`는 후속 태스크에서 라우팅을 붙일 때 추가한다 — 미사용 메서드를 미리 만들지 않는다(YAGNI). `show`의 시그니처는 기존 `select_attach`와 동일한 결과(bool: 표시할 세션 있음)를 반환해 호출부 후처리(`registry.contains` 확정)를 보존한다.

- [ ] **Step 1: Write the failing test**

`src/driver.rs` 하단에 추가(파일 본체는 Step 3에서 작성하므로, 먼저 테스트만 두고 컴파일 실패를 확인):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Selection;

    #[test]
    fn target_round_trips_through_selection() {
        let sel = Selection { source: "jup".into(), session: "api".into(), window: Some(2) };
        let t = Target::from_selection(&sel);
        assert_eq!(t.session, "api");
        assert_eq!(t.window, Some(2));
        assert_eq!(t.into_selection("jup"), sel);
    }

    #[test]
    fn seam_driver_is_object_safe() {
        // 핵심: Box<dyn MuxDriver>가 컴파일되어야 한다. trait이 dispatch 불가
        // 메서드를 얻으면 이 테스트가 깨진다.
        let _d: Box<dyn MuxDriver> = Box::new(SeamDriver);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:
```bash
export RUSTC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
export RUSTDOC="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
CARGO="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/cargo.exe"
"$CARGO" test --lib driver 2>&1 | tail -20
```
Expected: FAIL — `cannot find type Target` / `cannot find trait MuxDriver` (또는 `src/driver.rs` 미등록 컴파일 에러).

- [ ] **Step 3: Write minimal implementation**

`src/driver.rs` 본체(테스트 위에):

```rust
//! The mux DRIVER boundary: the supervisor passes INTENT (display this
//! session+window) and reads back a grid; HOW (attach / switch-client /
//! reattach / select-window) lives behind `MuxDriver`. `DriverCtx` injects the
//! supervisor-owned spawn capability + registry so the driver owns the DECISION
//! and per-host display STATE while the PTY infrastructure stays in the loop.
//!
//! `SeamDriver` is the behavior-preserving adapter: it holds NO per-host state
//! and delegates straight to the existing free functions, so introducing the
//! boundary changes no behavior. tmux/psmux-specific drivers that own the
//! decision come in a later step.

use std::sync::{Arc, Mutex};

use crate::cockpit::Selection;
use crate::display::DisplayWorker;
use crate::host::HostManager;
use crate::model::Hosts;
use crate::proxy::registry::AttachRegistry;
use crate::proxy::run::PtyEvent;
use crate::proxy::screen::Grid;

/// A supervisor INTENT: show this session (and optionally land on a window). The
/// generic shape the supervisor knows; the driver maps it onto mux mechanics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: String,
    pub window: Option<i64>,
}

impl Target {
    pub fn from_selection(sel: &Selection) -> Self {
        Target { session: sel.session.clone(), window: sel.window }
    }
    pub fn into_selection(&self, source: &str) -> Selection {
        Selection { source: source.to_string(), session: self.session.clone(), window: self.window }
    }
}

/// The generic capabilities the supervisor injects into a driver call: the
/// off-loop spawner, the attachment registry it fills, the transport that lowers
/// argv, the open control channel (if any), the view size, and the attach seq.
/// The driver owns the DECISION + per-host display state; these stay supervisor-owned.
pub struct DriverCtx<'a> {
    pub registry: &'a mut AttachRegistry,
    pub hosts: &'a mut Hosts,
    pub worker: &'a DisplayWorker,
    pub mgr: &'a HostManager,
    pub attach_seq: &'a mut u64,
    pub cols: u16,
    pub body_rows: u16,
    pub tree_width: u16,
}

/// One mux driver per host: intent in, screen out.
pub trait MuxDriver {
    /// Make the selected session live and landed on its window. Returns true when
    /// the selection has a session to show (so the caller can confirm the display).
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool;
    /// The grid the supervisor renders for the selection, if a live attach exists.
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>>;
    /// Forward input bytes to the selected session's attachment.
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx);
}

/// The behavior-preserving adapter: delegates to the existing free functions with
/// the same arguments, so the boundary changes no behavior. Holds no state.
pub struct SeamDriver;

impl MuxDriver for SeamDriver {
    fn show(&mut self, sel: &Selection, ctx: &mut DriverCtx) -> bool {
        crate::cockpit::select_attach(
            ctx.registry, ctx.hosts, sel, ctx.worker, ctx.attach_seq,
            ctx.cols, ctx.body_rows, ctx.tree_width, ctx.mgr,
        )
    }
    fn grid(&self, sel: &Selection, ctx: &DriverCtx) -> Option<Arc<Mutex<Grid>>> {
        ctx.registry.grid(&crate::cockpit::display_key(ctx.hosts, sel))
    }
    fn input(&mut self, sel: &Selection, bytes: Vec<u8>, ctx: &DriverCtx) {
        ctx.registry.input(&crate::cockpit::display_key(ctx.hosts, sel), bytes);
    }
}

// `PtyEvent` import kept for the pump method added when the loop routes reaps.
#[allow(unused_imports)]
use PtyEvent as _PtyEventReserved;
```

주의: `select_attach`·`display_key`는 현재 `cockpit.rs`에서 `fn`(비공개)이다. 이 태스크에서 둘을 `pub(crate)`로 올린다(공개 표면 변화 아님 — crate 내부 가시성만). `lib.rs`에 `pub mod driver;` 추가.

마지막의 `_PtyEventReserved` 더미는 작성하지 말 것 — `PtyEvent` import는 다음 태스크에서 `pump`가 쓸 때 추가한다. Step 3에서는 `PtyEvent`/`HostManager` 등 아직 안 쓰는 import를 넣지 말고, 실제 사용하는 것만 import한다(clippy `unused_imports` 0 유지).

- [ ] **Step 4: Run test to verify it passes**

Run:
```bash
"$CARGO" test --lib driver 2>&1 | tail -20
```
Expected: PASS — `target_round_trips_through_selection`, `seam_driver_is_object_safe` 그린.

- [ ] **Step 5: Full gate**

Run:
```bash
"$CARGO" test 2>&1 | tail -5
"$CARGO" clippy --all-targets 2>&1 | tail -5
"$CARGO" fmt --check 2>&1 | tail -5
```
Expected: 542+2 테스트 그린(신규 2개 추가), clippy 경고 0, fmt 차이 0.

- [ ] **Step 6: Commit**

```bash
git add src/driver.rs src/lib.rs src/cockpit.rs
git commit  # 메시지: refactor(driver): introduce MuxDriver boundary + SeamDriver adapter
```

---

## Task 2: route the show-intent path through the driver

루프의 두 `select_attach` 호출 지점(`Command::Attach` 처리, reconnect 스윕)을 `driver.show()`로 라우팅한다. `driver`는 루프 시작 시 `let mut driver: Box<dyn MuxDriver> = Box::new(SeamDriver);`로 만든다. 동작은 동일(SeamDriver가 같은 free 함수를 같은 인자로 호출).

**Files:**
- Modify: `src/cockpit.rs` (run 루프: driver 생성 + 2개 `select_attach` 호출 교체)

**Interfaces:**
- Consumes: Task 1의 `MuxDriver`, `SeamDriver`, `DriverCtx`.

- [ ] **Step 1: 회귀 게이트 기준선 확인 (테스트 추가 없음 — 기존 회귀 테스트가 이 경로를 덮음)**

`select_attach`를 직접 부르는 기존 테스트(`psmux_select_attach_*`, `local_tmux_shared_*`)는 그대로 둔다(seam은 그 위 호출부만 바꿈). 이 태스크의 "실패하는 테스트"는 컴파일 단계 — driver 경유가 기존 호출과 동치임을 기존 회귀 테스트가 증명한다.

- [ ] **Step 2: driver 생성 + Command::Attach 경로 교체**

`run`의 루프 진입 전(상태 초기화부)에 추가:
```rust
let mut driver: Box<dyn crate::driver::MuxDriver> = Box::new(crate::driver::SeamDriver);
```

`Command::Attach(sel)` 처리(현 L2194~2216)의 `select_attach(...)` 호출을 driver 경유로:
```rust
crate::model::Command::Attach(sel) => {
    let t = std::time::Instant::now();
    let mut ctx = crate::driver::DriverCtx {
        registry: &mut registry, hosts: &mut hosts, worker: &worker, mgr: &mgr,
        attach_seq: &mut attach_seq, cols, body_rows, tree_width,
    };
    if driver.show(&sel, &mut ctx) {
        if registry.contains(&display_key(&hosts, &sel)) {
            state.displayed = sel.clone();
        }
    }
    dbg_ms(&env.xmux_dir, "select_attach", t);
    dirty = true;
    dbg_log(&env.xmux_dir, &format!("state.selection -> key={} sess={}", display_key(&hosts, &sel), sel.session));
}
```
주의: `ctx`가 `&mut registry`/`&mut hosts`를 빌리므로, `if driver.show(...)` 블록 안에서 다시 `registry.contains(&display_key(&hosts, …))`를 부르려면 `ctx`를 먼저 drop해야 한다(NLL상 `driver.show` 반환 직후 `ctx`의 borrow가 끝나면 OK). 컴파일 에러가 나면 `let shown = driver.show(&sel, &mut ctx); drop(ctx);` 후 `if shown { … }`로 분리한다.

- [ ] **Step 3: reconnect 스윕 경로 교체**

reconnect 스윕의 `select_attach(...)`(현 L2625~2628)도 동일하게:
```rust
if !registry.contains(&key) && !in_flight_for_key {
    let mut ctx = crate::driver::DriverCtx {
        registry: &mut registry, hosts: &mut hosts, worker: &worker, mgr: &mgr,
        attach_seq: &mut attach_seq, cols, body_rows, tree_width,
    };
    driver.show(&state.selection, &mut ctx);
}
```

- [ ] **Step 4: 게이트**

Run:
```bash
"$CARGO" test 2>&1 | tail -5
"$CARGO" clippy --all-targets 2>&1 | tail -5
"$CARGO" fmt --check 2>&1 | tail -5
```
Expected: 모든 테스트 그린(특히 `4a5f053` 회귀 테스트), clippy 0, fmt 0.

- [ ] **Step 5: Commit**

```bash
git add src/cockpit.rs
git commit  # refactor(driver): route the show-intent path through MuxDriver
```

---

## Task 3: route grid/input reads through the driver

표시 읽기(`grid`)와 입력 포워딩(`input`)을 driver 경유로. 이 둘은 순수 위임이라 동작 동일.

**Files:**
- Modify: `src/cockpit.rs` (draw·dump의 `registry.grid(...)`, stdin·ctl의 `registry.input(...)`)

- [ ] **Step 1: draw 경로의 grid 읽기 교체**

draw 게이트(현 L2259~2262)와 `Cmd::Dump`(현 L2499~2502)의
`registry.grid(&display_key(&hosts, &state.selection))`를 driver 경유로. 단,
`display_matches_selection().then(|| …)` 게이트는 보존:
```rust
let grid_arc = state.display_matches_selection().then(|| {
    let ctx = crate::driver::DriverCtx {
        registry: &mut registry, hosts: &mut hosts, worker: &worker, mgr: &mgr,
        attach_seq: &mut attach_seq, cols, body_rows, tree_width,
    };
    driver.grid(&state.selection, &ctx)
}).flatten();
```
주의: `grid()`는 `&DriverCtx`(불변)만 받지만 `DriverCtx` 필드가 `&mut`라 가변 빌림이 필요하다. draw 블록 안에서 이후 `registry`/`hosts`를 다시 쓰므로 `ctx`는 grid 추출 직후 drop되어야 한다(`.then(|| { … })` 클로저 끝에서 drop됨 — OK). 컴파일 에러 시 `grid_arc`를 클로저 밖에서 미리 계산하고 클로저는 값만 옮기도록 분리.

`input.rs` 스코프 충돌 회피: `grid()`의 `ctx`는 단명. 만약 borrow 충돌이 끈질기면, 이 태스크에서 `grid()`/`input()`의 `DriverCtx`를 **불변 참조 묶음**(`GridCtx { registry: &AttachRegistry, hosts: &Hosts }`)으로 좁히는 대안을 쓴다 — 하지만 우선 단일 `DriverCtx`로 시도하고, 충돌 시에만 분리.

- [ ] **Step 2: stdin·ctl 입력 포워딩 교체**

`Action::Forward(f)`(현 L1783)와 `Cmd::RawBytes`(현 L2534)의 `registry.input(&display_key(...), …)`를 `driver.input(...)` 경유로. `Action::Forward`는 `handle_stdin_bytes`/하위 함수 안이라 `driver`를 그 경로로 넘겨야 한다 — 시그니처 변경이 광범위하면 이 입력 경로는 **이번 태스크에서 건드리지 않고** ctl `RawBytes`만 교체하고, stdin Forward는 단계 3(결정 이동)에서 driver가 input을 진짜 소유할 때 함께 옮긴다. 정직하게: `registry.input`은 순수 위임이라 어디서 부르든 동작 동일하므로, 침투 비용이 큰 경로는 보류한다.

- [ ] **Step 3: 게이트**
```bash
"$CARGO" test 2>&1 | tail -5
"$CARGO" clippy --all-targets 2>&1 | tail -5
"$CARGO" fmt --check 2>&1 | tail -5
```
Expected: 그린/0/0.

- [ ] **Step 4: Commit**
```bash
git add src/cockpit.rs
git commit  # refactor(driver): read grid + forward ctl input through MuxDriver
```

---

## Task 4 (단계 3 시작 — 가능하면): per-mux 결정을 driver로

**STOP 규칙**: 이 태스크는 `SelectOutcome` match를 driver별 분기로 옮긴다. 정합성이 라이브 터미널을 필요로 하는 첫 지점(특히 단계 4의 in-place switch / 1a 갭)에 닿으면 **즉시 멈추고**, 그린인 것을 커밋하고, 라이브 게이트 절차를 보고한다.

이 태스크는 capacity가 남을 때만 시작한다. 시작한다면:

- [ ] **Step 1**: `MuxDriver`에 `sync`/`shown`/`reattach`/`pump`를 추가하고, `SeamDriver`가 각각 `sync_source_terminals`/`host.display.shows`/reattach-kick 로직/reap 로직에 위임. (라우팅만 — 동작 동일, 그린 게이트.)
- [ ] **Step 2**: host당 실제 driver를 `Host`가 들도록 전환할지 평가 — 단, 이는 `host.display` 북키핑 소유 이전을 수반하고 borrow 재배선이 광범위하므로, **여기서 STOP하고 보고**한다(라이브 게이트 + 설계 판단이 필요).

---

## Self-Review

- **Spec coverage**: 스펙 §5 단계 2(동작 보존 seam)는 Task 1~3이 구현. 스펙 §5 단계 3(결정 이동)은 Task 4가 시작점만(STOP 규칙). §2 trait 표면은 Task 1이 `show`/`grid`/`input`로 착수, 나머지는 사용 시점에 추가(YAGNI). §6 검증된 사실은 코드 변경 없음(보존). §7 테스트 전략: Task 1의 object-safety + round-trip, Task 2의 회귀(기존 `select_attach` 테스트가 driver 경유 후에도 그린).
- **Placeholder scan**: 모든 코드 스텝에 실제 코드. borrow 충돌 같은 실제 위험은 폴백 경로까지 명시.
- **Type consistency**: `Selection`/`Target`/`DriverCtx` 필드명·시그니처가 Task 간 일치. `select_attach`/`display_key`는 `pub(crate)`로 승격(Task 1 Step 3).
