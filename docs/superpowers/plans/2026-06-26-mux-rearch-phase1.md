# mux 백엔드·TUI 재아키텍처 — Phase 1: State 토대 도입

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (권장) 또는 superpowers:executing-plans 로 task 단위 실행. 단계는 체크박스(`- [ ]`)로 추적.
> **설계 단일 진실:** `docs/superpowers/specs/2026-06-26-mux-backend-tui-rearchitecture-design.md` (커밋 b9446ed).
> 이 문서는 5-Phase 중 **Phase 1만 상세**하다. Phase 2~5는 §로드맵의 한 단락씩이며, 각 Phase는 직전 Phase가 코드를 재편한 *후* 그 시점 코드 기준으로 상세화한다(아직 없는 타입을 추측하지 않음 — 계획의 YAGNI).

**Goal:** 새 아키텍처의 키스톤인 런타임 `State` store를 도입하고, cockpit 루프의 selection 도메인 지역변수를 그 안으로 모은다. **순수 리팩터 — 동작 0 변경, 기존 테스트 전부 그린 유지.**

**Architecture:** 새 `src/state/` 모듈에 런타임 도메인 `State`. 이번 Phase는 cockpit의 selection 도메인 지역변수(`selection`/`last_attached_sel`/`attach_deadline`/`last_saved_session`)만 담는다 — display/focus/inventory/popup/dirty는 해당 Phase가 와서 소비할 때 흡수(§4 spec). 기존 `src/state.rs`(디스크 prefs 영속화)는 이름 충돌을 피해 `src/prefs.rs`로 개명(역할과 이름 일치).

**Tech Stack:** Rust, tokio current-thread, ratatui/crossterm, vt100, `#[cfg(test)]` 단위테스트.

> **결함 A 범위 결정(사용자 확정):** 결함 A(표시중 세션 비동기화)는 이전 세션에서 충분히 검증됨. **현재 아키텍처에서 고치지 않고, 새 아키텍처 정립 후 다룬다.** 따라서 Phase 1은 `last_attached_sel` 게이트를 제거하거나 reconcile 로직을 바꾸지 않는다 — AS-IS 동작을 보존한 채 State로 구조만 옮긴다. (참고: 코드 추적 결과 `last_attached_sel` latch 자체는 결함 A 재현의 원인이 아니다 — session 전환 시 select_attach는 정상 호출되고 PerSession 분기·Ready 가드도 정상. 실제 원인은 새 아키텍처에서 State가 display 진실을 단일 소유하게 되면 구조적으로 해소된다.)

## Global Constraints

- 빌드: rustup shim 막힘 → real toolchain 직접 호출.
  `TC=~/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin; RUSTC="$TC/rustc.exe" RUSTDOC="$TC/rustdoc.exe" "$TC/cargo.exe" <cmd>`
- `cargo test`는 bin을 재빌드하지 않음 → 라이브 검증 전 `cargo build` 별도.
- AS-IS 원칙: 주석/문서에 "이전엔 X였다" 류 변경 서사 금지. 현재 상태만 기술.
- 각 task 끝에서 `cargo test` + `cargo clippy --all-targets -- -D warnings` 그린 유지.
- 동작 보존: 베이스라인 **499 passed / 0 failed / 6 ignored** 가 그대로 유지돼야 한다(순수 리팩터, 신규 테스트는 State::default 단위테스트 1개만).
- ⚠ 기존 uncommitted `src/` 변경(M 다수)은 이전 세션 작업 — 재아키텍처와 무관, 그 위에서 새로 시작한다.

---

## 현재 코드 앵커 (실행자 참고 — 읽고 시작)

- `src/cockpit.rs:226-258` — `Selection` 정의(`source`/`session`/`window`; `from_target`/`address`/`is_empty`; `Default` 파생 사용 중).
- `src/cockpit.rs:1769-1780` — 루프 지역변수 선언: `selection`(1769), `last_attached_sel`(1772), `attach_deadline`(1773), `last_saved_session`(1776). `attach_seq`(1780)는 attach 메커니즘 카운터 — 이번 hoist 대상 아님(루프 local 유지).
- `src/cockpit.rs:1902-1910` — `reattach_kick`: `last_attached_sel = Selection::default()`(1908), `attach_deadline = Some(...)`(1909).
- `src/cockpit.rs:1918-1925` — selection 파생 + deadline 무장.
- `src/cockpit.rs:1928-1967` — settle: `attach_deadline` 소비 + `last_saved_session` 저장 게이트 + `last_attached_sel` 비교/대입(1949·1957).
- `src/cockpit.rs:2140` — Ready arm 안의 `last_attached_sel = Selection::default()`.
- `src/state.rs` — 디스크 prefs(`load/save_last_session`·`tree_width`·`auto_hide_tree`). 호출처: `crate::state::*`.

---

## Task 1: 디스크 prefs 모듈 개명 `state.rs` → `prefs.rs`

**Files:**
- Rename: `src/state.rs` → `src/prefs.rs`
- Modify: `src/lib.rs` (`pub mod state;` → `pub mod prefs;`), 모든 `crate::state::{load,save}_*` 호출처 → `crate::prefs::`

**Interfaces:**
- Produces: `crate::prefs::{load_last_session, save_last_session, load_tree_width, save_tree_width, load_auto_hide_tree, save_auto_hide_tree}` — 시그니처 불변, 경로만 이동.

- [ ] **Step 1: 호출처 전수 확인** — `grep -rn "crate::state::" src/` 로 전 호출처 목록화(개수 파악).
- [ ] **Step 2: 파일 이동** — `git mv src/state.rs src/prefs.rs`.
- [ ] **Step 3: lib.rs 갱신** — `pub mod state;` → `pub mod prefs;` (이 단계에서 `state` 모듈은 사라짐; Task 2가 `src/state/`로 되살림).
- [ ] **Step 4: 호출처 치환** — `crate::state::` → `crate::prefs::` 전부. 모듈 내부 `super::`/테스트는 그대로.
- [ ] **Step 5: 빌드 + 테스트** — `build` / `test`. 기대: 499 그린(순수 이동).
- [ ] **Step 6: 커밋**

```bash
git add -A
git commit -m "refactor: rename disk-prefs module state.rs -> prefs.rs"
```

---

## Task 2: `state/` 런타임 store 도입 + selection 도메인 hoist

**Files:**
- Create: `src/state/mod.rs`
- Modify: `src/lib.rs` (`pub mod state;` 추가 — 이번엔 디렉토리 모듈)
- Modify: `src/cockpit.rs` (루프 지역변수 → `State` 필드, 본문 참조 치환)

**Interfaces:**
- Produces: `crate::state::State` — `pub selection: Selection`, `pub last_attached_sel: Selection`, `pub attach_deadline: Option<std::time::Instant>`, `pub last_saved_session: String`. `#[derive(Default)]`. (focus/inventory/display/popup/dirty/prefs는 §4 spec이 열거하나 **이번 Phase가 쓰지 않으므로 추가하지 않음** — `// ponytail:` 주석으로 명시.)
- Consumes: `crate::cockpit::Selection`.

- [ ] **Step 1: `state/mod.rs` 작성 + 단위테스트**

```rust
//! 런타임 도메인 상태의 단일 진실 store. 새 아키텍처의 키스톤 — 위 계층(Component)이
//! &State 로 읽는 토대. 이번 Phase는 cockpit 루프의 selection 도메인만 담는다.
use crate::cockpit::Selection;
use std::time::Instant;

#[derive(Default)]
pub struct State {
    /// 트리 커서가 가리키는 정규 선택.
    pub selection: Selection,
    /// 디바운스 attach: 마지막으로 attach/switch 한 선택(AS-IS — 결함 A는 새 아키텍처에서 다룸).
    pub last_attached_sel: Selection,
    /// 설정되면 그 시점 이후 settle에서 attach 적용.
    pub attach_deadline: Option<Instant>,
    /// 마지막으로 prefs에 저장한 세션 주소(주소 변경 시에만 재저장).
    pub last_saved_session: String,
    // ponytail: focus/inventory/display/popup/dirty/prefs(spec §4)는 해당 Phase에서 흡수.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert!(s.selection.is_empty());
        assert!(s.last_attached_sel.is_empty());
        assert!(s.attach_deadline.is_none());
        assert_eq!(s.last_saved_session, "");
    }
}
```
(`Selection`이 `pub`이고 `Default`인지 확인 — cockpit.rs:226. `Selection`은 `pub struct`지만 cockpit 내부 타입이므로 `crate::cockpit::Selection` 경로 사용. 비공개면 `pub` 추가.)

- [ ] **Step 2: lib.rs에 모듈 등록** — `pub mod state;` 추가(`src/state/mod.rs`를 가리킴).
- [ ] **Step 3: cockpit 루프 hoist** — `let mut selection`(1769)/`last_attached_sel`(1772)/`attach_deadline`(1773)/`last_saved_session`(1776) 선언을 `let mut state = crate::state::State::default();` 하나로 대체. 본문 전체에서 `selection`→`state.selection`, `last_attached_sel`→`state.last_attached_sel`, `attach_deadline`→`state.attach_deadline`, `last_saved_session`→`state.last_saved_session` 치환(1902-2140 + 렌더/핸들러 호출 인자 포함). **순수 기계적 치환 — 로직·조건·순서 변경 0.**
  - 주의: `selection`은 `handle_stdin_bytes`(2166) 등 함수에 `&selection`/`&mut`로 넘겨짐 — 호출부를 `&state.selection`으로. 함수 시그니처는 그대로(Selection를 받음).
- [ ] **Step 4: 빌드 + 테스트 + clippy** — `build` / `test` / `clippy --all-targets -- -D warnings`. 기대: 499 + State 테스트 1개 그린, clippy 0.
- [ ] **Step 5: 커밋**

```bash
git add -A
git commit -m "refactor(state): introduce runtime State store, hoist cockpit selection-domain locals"
```

---

## Task 3: 검증 게이트 — 순수 리팩터 무회귀 확인

**Files:** 없음(검증만).

- [ ] **Step 1:** `"$TC/cargo.exe" test` — 499 + 1 그린, 0 failed.
- [ ] **Step 2:** `"$TC/cargo.exe" clippy --all-targets -- -D warnings` — 0 warning.
- [ ] **Step 3:** `git diff --stat <Phase1 시작 커밋>..HEAD` 로 변경 범위가 cockpit.rs/lib.rs/state(prefs) 모듈에 국한됐는지 확인(다른 모듈 무손상).
- [ ] **Step 4:** (선택) 라이브 스모크 — 새 아키텍처 토대만 들어갔으므로 동작 동일해야 함. 시간 여유 시 cockpit 기동 + ctl `status`/`dump`로 기존과 동일 거동 1회 확인(결함 A 자체는 검증 대상 아님 — 미해결 상태 유지가 정상).

---

## 로드맵 — Phase 2~5 (각 Phase 차례에 직전 결과 기준으로 상세화)

> 아래는 의도와 범위만. exact 코드는 직전 Phase가 코드를 재편한 *후* 작성한다(없는 타입 추측 금지). 각 Phase는 cargo test·clippy 그린 + 해당되는 라이브 게이트 통과. **결함 A의 실제 해소는 State가 display 진실을 단일 소유하게 되는 시점(Phase 5 전후)에 자연 귀결로 다룬다 — 사용자 확정 범위.**

- **Phase 2 — Backend trait 추출.** 현 `Mux`(model/mux.rs)를 `Backend`로 확장하고 `src/backend/psmux/`·`src/backend/tmux/` 로 재배치. argv 빌더(mux.rs)·control reader(host.rs run_reader)·attach(proxy/run.rs)를 backend별로 이주. 분류 메서드(`event_source`/`shares_one_attachment`/`stable_per_session_attachments`)는 **아직 남겨둠**(Phase 3·4에서 흡수). **`(zellij/)` 슬롯은 만들지 않는다** — 실제 구현 시 추가(spec §10 비목표). Transport 직교 유지.
- **Phase 3 — `events()` 통일 (구조 통합, 사용자 승인 = "spec대로").** ⚠ **정정**: spec의 "psmux 변화감지 미구현" 전제는 부정확하다 — cockpit `local_poll`(`LOCAL_POLL_MS=1500`, PSMUX_POLL_MS와 동값)이 poll 소스마다 매 1.5s 재열거 + 모든 세션 list-panes 재실행하고, `apply_panes`가 `panes[addr].active`를 갱신, 루프 상단 `select_active_window()`(터미널 포커스 시)가 따라간다 → 외부 psmux 윈도우 전환은 **이미 전파됨**. 따라서 Phase 3의 진짜 가치 = **탐지 버그 수정이 아니라 `event_source` 누수 5곳 제거하는 구조 통합**.

  **상세 계획 (다음 세션 — 거의 원자적, 신선 컨텍스트 권장):**
  - **현 두 경로:** (control) HostManager가 HostClient(control reader thread) 소유 → `HostEvent`(Connected/Inventory/Changed/WindowChanged/Focus/Exited/ClientDetached) → `host_rx` → `handle_host_event`. 데이터는 reader가 shared `host.inventory`에 쓰고 시그널만 emit. (poll) cockpit `local_poll` 타이머 → `spawn_detected_enumeration`(tokio task) → `LocalEnum`(Scanned/Sessions/Panes) → `enum_rx` → `apply_source_result`/`apply_panes`/`sync_source_terminals`. 데이터는 이벤트에 실림.
  - **`event_source` 분기 5곳 (제거 대상):** `ensure_current_host`(cockpit.rs:665), `dispatch_detected_host`(750), `kick_rescan`(825), `local_poll.tick`(2519), `reconnect.tick`(2532).
  - **목표 설계:** HostManager가 poll task도 소유. `HostManager::ensure(host_id, host: &Host, src, cols, rows)`가 `host.backend.event_source()`를 **한 곳에서** 읽어 control→reader, Poll→자가-루프 poll task(간격=backend의 PSMUX_POLL_MS) 스폰. poll task는 `spawn_detected_enumeration` 로직을 옮겨 담고, 결과를 **HostEvent로 emit**(LocalEnum 변종 Sessions/Panes/Scanned를 HostEvent에 흡수 → `enum_rx`/`enum_tx`/`LocalEnum` 삭제, 단일 버스). cockpit은 `mgr.ensure`만 uniform 호출 + `handle_host_event`가 흡수한 데이터 변종 처리.
  - **제거:** cockpit `local_poll` 타이머 + 5개 분기. `dispatch_detected_host`/`scan_or_dispatch_host`는 detection(미탐지 시 `spawn_host_detection`)만 남기고 dispatch는 `mgr.ensure`로. `reconnect`/`ensure_current_host`도 `mgr.ensure` uniform. `kick_rescan`(`r`): poll은 task cancel+respawn으로 즉시 재열거, control은 `client.list_sessions()`.
  - **poll task 생명주기:** HostClient처럼 JoinHandle+cancel 보유, `reap`/`teardown_all`에 편입. detection 흐름(미탐지→`spawn_host_detection`→Scanned→`apply_scan_result`)은 event_source 분기 아니므로 유지.
  - **green-gate 주의:** 거의 원자적 — 채널/소유권을 함께 바꿔야 정합. 반쪽 커밋은 double-poll/broken loop. 500 tests 그린 유지 + clippy 0. **라이브 게이트:** 터미널에서 psmux 윈도우 전환 → 트리 추적(헤드리스 ctl + 사람 눈), 그리고 회귀 없음(control 호스트 정상).
  - **PSMUX_POLL_MS:** 현재 backend(`event_source()`)가 선언만 하고 cockpit이 `LOCAL_POLL_MS` 하드코딩. 통합 후 poll task가 backend의 interval을 실제 사용 → `LOCAL_POLL_MS` 제거.
- **Phase 4 — `select()` 통일.** `select_attach`의 3-way 분기를 `backend.select() -> SelectOutcome` 뒤로. `shares_one_attachment`/`stable_per_session_attachments` 제거. host_selection_key 분류 분기도 backend로 흡수.
- **Phase 5 — Component 분해 + State 완성.** `cockpit.rs`/`switcher.rs`를 `src/app.rs`(얇은 배선) + `src/ui/{tree,terminal,popup,status}.rs`(flat·대등 Component)로 분해. 각 Component가 `&State`/`&dyn Backend`를 직접 받음. **`State`가 display(표시중 attachment + 그 address)를 단일 소유** → "표시중 == 선택" 불변식이 구조적으로 성립, 결함 A 해소. draw 게이팅·마우스 라우팅 보존.

## Phase 3 — 실행 체크리스트 (현재 코드 기준 상세)

> 단일 버스로의 동시성 통합. **거의 원자적** — 채널·소유권을 함께 바꿔야 컴파일·정합한다(반쪽 = double-poll/broken loop). 기존 500 테스트가 무회귀 게이트.

**확정 앵커 (Phase 2 후 검증):** event_source 누수 5곳 = `ensure_current_host`(665) · `dispatch_detected_host`(750) · `kick_rescan`(825) · `local_poll.tick`(2519) · `reconnect.tick`(2532). poll 데이터 경로 = `LocalEnum`(1040) + `spawn_detected_enumeration`(704) + `enum_tx/enum_rx`(2000) + `local_poll` 타이머(2022). `dirty`는 select 후 `if !from_frame`(2590)로 일괄 — 채널 병합해도 보존.

**목표 설계:** `host_tx`/`host_rx` 단일 버스. `HostManager`가 control 클라이언트(`clients`)와 poll task(`polls: HashMap<String, JoinHandle<()>>`)를 둘 다 소유.
- `HostEvent`에 poll 데이터 변종 흡수: `Scanned{source, detected}` · `Sessions{source, sessions, err}` · `Panes{address, panes}` (host.rs).
- `run_poll(source, transport, kind, bin, interval_ms, events)` (host.rs): `spawn_detected_enumeration` 로직을 자가-루프로 — enumerate→`Sessions` emit→세션별 list-panes→`Panes` emit→`sleep(interval)`. `events.send` 실패 시 return(수신자 drop).
- `HostManager::ensure(id, host: &Host, src, cols, rows)`: `event_source()`를 **여기 한 곳에서만** 읽어 Control→`HostClient::spawn`, Poll→`tokio::spawn(run_poll(...))`. 이미 있으면 `Ok(false)`(idempotent).
- `HostManager::rescan(id, host, src, cols, rows)`: control(`clients`)면 `list_sessions()`, poll(`polls`)면 task `abort()`+respawn(즉시 재열거). **map 소속으로 분기 — event_source 안 읽음.**
- `HostManager::events()`: detection spawn(`spawn_host_detection`)용 sender clone.
- `reap`/`teardown_all`: control teardown + poll task `abort()`.

**cockpit 재배선 (event_source 분기 0):**
- `ensure_current_host`/`dispatch_detected_host`/`reconnect.tick`: `host.detected`면 `mgr.ensure(id, host, src, …)` uniform.
- `scan_or_dispatch_host`: 미탐지→`spawn_host_detection(src, mgr.events())`, 탐지→`dispatch_detected_host`(→`mgr.ensure`).
- `kick_rescan`: 미탐지→`scan_or_dispatch_host`, 탐지→`mgr.rescan`.
- `handle_host_event`에 `detecting: &mut HashSet<String>` 추가 + `Scanned`/`Sessions`/`Panes` arm 흡수(현 `enum_rx` arm 로직 그대로).
- **삭제:** `LocalEnum` · `spawn_detected_enumeration` · `enum_tx`/`enum_rx` · `local_poll` 타이머+arm · `enum_rx.recv()` arm · `LOCAL_POLL_MS` 상수(+ backend.rs:507 주석 갱신). `enum_tx`를 받던 함수 시그니처(`connect_all_sources`/`scan_or_dispatch_host`/`dispatch_detected_host`/`kick_rescan`/`handle_tree_bytes`/`handle_mouse_event`/`handle_stdin_bytes`)에서 제거 + 전 호출처·테스트 갱신.

**TDD:** 신규 테스트 1개(host.rs) — poll 호스트 lifecycle: `ensure`=Ok(true)→`get()`=None(control 클라이언트 없음)→재`ensure`=Ok(false)→`reap`→재`ensure`=Ok(true). `run_poll`의 emit은 코드 이동(동작 불변)이라 기존 live-enum + apply 테스트가 가드.

**green-gate:** `cargo build`/`test`(500+신규 그린, 0 fail)/`clippy --all-targets -- -D warnings`(0)/`fmt`. **라이브 게이트(사람 눈):** 터미널 psmux 윈도우 전환→트리 추적 + control 호스트 무회귀. 헤드리스 ctl(switch/status/dump)로 가능한 데까지 검증.

## Phase 4 — 실행 체크리스트 (Phase 3 후 현재 코드 기준 상세)

> `select_attach`의 분기를 `backend.select() -> SelectOutcome` 뒤로 옮기고 분류 메서드 2개(`ServerModel::shares_one_attachment`, `Backend::stable_per_session_attachments`)를 제거. spec §121이 `fn select(&self, addr) -> SelectOutcome`를 명시. **이 phase는 사용자가 보는 PTY attach/표시 경로를 건드린다 — 가장 높은 회귀 위험. TDD + 어드버사리얼 리뷰 + 라이브 시각 검증(사람) 필수.**

**⚠ spec 전제 정정 #3 (코드로 확인 — 결함 A·B에 이은 세 번째 불일치):** spec은 "3-way 분기"라 하나, 현재 백엔드로는 **2-way만 살아있고 1개는 죽은 코드**다:
- **tmux** = `Shared` → `shares_one_attachment()=true` → `shared` 분기. 키 = host-id.
- **psmux** = `PerSession` + `stable_per_session_attachments()=false`(backend/mod.rs:197 override) → `!shares_one && !stable` 중간 분기. 키 = host-id (`host_selection_key`: `false || !false = true`). **host당 PTY 1개, 세션 변경 시 remove+reattach**(per-session 서버라 switch-client 불가).
- **`else`(stable per-session pinned, per-session 키) 분기 = 도달 불가** — `PerSession`+`stable=true` 백엔드가 없음(기본 true지만 유일한 PerSession인 psmux가 false로 override). `select_attach`(477-492), `sync_source_terminals`(594-615), `host_selection_key`(314 `host.display_key`), `handle_host_event` Sessions arm(1135 `h.display_key` 가지), `ServerModel::display_key`의 PerSession arm이 모두 죽은 경로.

**권고 설계(가장 보수적 — YAGNI + spec §10 비목표):** 죽은 stable-per-session 가지를 **재현하지 말고 드롭**(실제 pinned-per-session 백엔드 생길 때 재도입; P2의 "(zellij/) 슬롯 안 만듦"과 동일 원칙). `SelectOutcome`은 2 변종:
```rust
pub enum SelectOutcome {
    SharedSwitch,        // tmux: host당 PTY 1개, switch-client로 in-place 이동
    PerSessionReattach,  // psmux: host당 PTY 1개, 세션 변경 시 remove+reattach
}
```
`Backend::select(&self) -> SelectOutcome` (인자 불필요 — server_model처럼 백엔드 상수). Tmux→SharedSwitch, Psmux→PerSessionReattach. **`server_model()`·`display_key`는 유지**(model identity·death/reap·host.rs:231·112에서 광범위 사용); 제거 대상은 분류 헬퍼 2개뿐.

**제거할 메서드 2개의 읽기 사이트 5곳 → `select()` match로 치환:**
1. `host_selection_key`(cockpit.rs:309-316): 두 살아있는 백엔드 다 host-id → `match host.mux.select() { SharedSwitch|PerSessionReattach => host.id() }`로 단순화(죽은 per-session 키 가지 드롭). 실행 시 `host.display_key`/`ServerModel::display_key`의 다른 사용처(host.rs:112/251, cockpit.rs:1092 ClientDetached=Shared host-id LIVE) 안 깨지는지 확인.
2. `select_attach`(388-389 + 391/452/477 3-way): `match host.mux.select()` → `SharedSwitch`=현 `shared` 본문(first-attach+marker / clear_grid+switch-client-over-control-or-switch_plan / window select-window-unless-folded), `PerSessionReattach`=현 `!stable` 본문(showing 다르면 remove+clear, 아니면 attach; window=select-window plan). 죽은 `else` 드롭.
3. `sync_source_terminals`(558/560 + 561/567/594 3-way): `SharedSwitch`=warm-one-per-host+reap-when-empty; `PerSessionReattach`=sessions 비면 host-id remove. 죽은 stable 가지(594-615)+`addresses_to_reap`가 그 가지 전용이면 함께 드롭(다른 사용처 확인).
4. `handle_host_event` Sessions arm(1133-1136): 현재 `if stable { h.display_key(addr) } else { source }` → psmux는 always `source`(host-id). `select()==PerSessionReattach`에서 host-id 키로 단순화, 죽은 가지 드롭.
5. `reconnect.tick` 재웜(2441 `shares_one_attachment()`): `matches!(host.mux.select(), SelectOutcome::SharedSwitch)`로 치환(shared만 per-host PTY 재웜).

**TDD:** backend 테스트 — `tmux().select()==SharedSwitch`, `psmux().select()==PerSessionReattach`(backend/mod.rs 기존 server_model 테스트 옆). 그 다음 분류 메서드 제거→컴파일러가 5 사이트 가이드. select_attach/sync의 동작은 코드-이동(2 살아있는 가지 보존)이라 기존 테스트가 가드하나 **약함**(attach 경로 커버리지 부족, 메모리 `xmux-merge-review`).

**green-gate:** build/test(그린 유지)/clippy0/fmt. **라이브 시각 게이트(사람 — 무인 검증 불가):** tmux 호스트 세션 간 전환=switch-client in-place(잔상 없음), psmux 세션 간 전환=remove+reattach(새 그리드), window-row 전환=select-window 추적. `xmux-cockpit-local-attach-headless-untestable`로 local attach 헤드리스 위험 → 신선/대화형 세션에서 실행 권장.

## Phase 5 — Ideal architecture: 단방향 흐름 (Store + Action + Command + flat Components)

**목표 (사용자 확정: 수정 난이도보다 이상적 코드가 더 큰 가치):** 상태 변경 경로를 하나로 강제하는 단방향 데이터 흐름. 컴포넌트는 `&State`를 *읽기만* 하고 입력을 `Action`으로 방출, `State::apply(Action) -> Vec<Command>`가 유일한 변경 지점, `app.rs`가 Command를 Backend에 디스패치, `BackendEvent`는 `State::apply_event`로 흡수. 이로써 결함 A류 동기화 버그가 구조적으로 불가능.

> **spec §4/§6 모순 정정:** spec §6은 "Tree가 직접 `state.selection`을 바꾸고 `backend.select()` 호출"이라 했으나 이는 §4("변경은 `apply(action)`로")와 충돌하고 결함 A류의 재발 자리다. **이상적 해소: 컴포넌트는 State를 직접 mutate하지 않는다 — 자기 뷰상태(커서/스크롤)만 바꾸고 도메인 변경은 Action으로 요청.** 부수효과는 `Command`(intent와 분리)로 반환해 app.rs가 실행.

**이상적 Component 시그니처** (spec의 `&mut State`+`&dyn Backend`보다 한 단계 순수):
```rust
trait Component {
    fn handle_event(&mut self, ev: &Event, state: &State) -> Vec<Action>;  // &State 읽기만
    fn render(&self, frame: &mut Frame, area: Rect, state: &State);
}
```

각 Task는 cargo test·clippy·fmt 그린 + 커밋. 라이브 시각 게이트(psmux/tmux 세션·윈도우 전환)는 사람이 수행.

- [x] **Task 5.1 — State가 표시중 진실 단일 소유 (결함 A).** `last_attached_sel`→`displayed`(확정 주소), render는 `display_matches_selection`일 때만 그리드, attach 트리거 `should_attach`(in-flight 무발사). 순수 헬퍼 단위테스트, `attach_deadline` 보존. 커밋 `b2ec18e`.
- [x] **Task 5.2 — inventory+filter → State (분해 백본).** groups/panes/scanning/panes_loaded/filter를 State로, `from_scan`/`from_sources`가 시딩, Switcher 메서드가 `&State`/`&mut State` 받음. 커밋 `47ed323`.
- [x] **Task 5.3 — `ui/status.rs` 추출.** divider/footer/host-info + view-local 상태(flash/spinner/colors/…)를 `Status` 구조체로, `&State` 읽음. 커밋 `636a485`.
- [ ] **Task 5.4 — `Action`+`Command` + `State::apply` (단방향 키스톤).** 현 `Operation`(ctl)과 `PendingOp`(create/rename/kill 큐) 두 intent 경로를 **하나의 `Action` enum**으로 통일. `State::apply(Action) -> Vec<Command>`(유일 변경 지점) + `State::apply_event(BackendEvent)`. State가 `focus`·`popup`(모달)·`dirty` 흡수. 입력→Action, 부수효과→Command→Backend 디스패치로 재배선. **가장 큰 회귀 위험 — TDD + 어드버사리얼 리뷰 + 라이브 게이트 필수.**
- [ ] **Task 5.5 — `Component` trait + `ui/tree.rs`.** trait 정의(위 시그니처). switcher→Tree 컴포넌트: rows/커서/필터-뷰는 자기 소유, `handle_event`가 Action 방출(직접 mutate 0), `render`는 `&State`. 우클릭→Tree가 rows로 대상 해석→`Action::OpenMenu` (popup이 tree 내부 접근 0).
- [ ] **Task 5.6 — `Popup`·`Terminal` 컴포넌트 + `Status` 적합.** Popup은 `state.popup`/`state.selection`만 읽고 Action 방출(menu/input/kill의 tree 결합 소멸). Terminal은 그리드 렌더+마우스. Status를 trait에 맞춤.
- [ ] **Task 5.7 — `app.rs` 얇은 배선.** `run_cockpit`을 `src/app.rs`로: 루프+focus/popup 기반 입력 라우팅+Action 수집→`apply`→Command 디스패치+`apply_event`+dirty 게이트 draw만. 도메인 로직은 전부 State/Component로 이주 → cockpit god-object 소멸.

> 5.5~5.7의 exact 단계는 직전 Task가 코드를 재편한 *후* 상세화(없는 타입 추측 금지). 5.4~5.7은 단순 파일분할이 아니라 **데이터 흐름의 단방향 재배선** — 결합을 우회가 아니라 제거한다. 동작은 보존(5.1의 stale→"(attaching…)" 외 사용자 가시 변경 없음 목표).

## Self-Review (Phase 1)

- **Spec 커버리지:** §8.1(State 도입) = Task 2. State 필드는 selection 도메인만 도입(나머지 §4 필드는 §로드맵에서 해당 Phase로 명시 이연). `last_attached_sel` 제거(§8.1)는 결함 A를 현 아키텍처에서 안 고치는 사용자 결정에 따라 **이연** — Phase 5에서 State가 display를 소유하며 자연 obsolete.
- **플레이스홀더:** 없음. Task 1 Step 1의 호출처 개수는 실행 시 grep로 확정(TODO 아님).
- **타입 일관성:** `State` 필드명·타입이 Task 2 hoist 치환과 일치. `crate::prefs::` 경로가 Task 1 치환과 일치.
