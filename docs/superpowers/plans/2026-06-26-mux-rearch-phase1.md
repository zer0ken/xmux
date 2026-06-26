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
- **Phase 3 — `events()` 통일.** `EventSource::Poll{interval_ms}`의 주기 타이머를 psmux backend `events()` 안에서 실제 구동(`PSMUX_POLL_MS`) + enumerate diff로 `InventoryChanged`/`ActiveWindowChanged` 생성 → **결함 B(psmux 변화감지 미구현) 해소**. tmux control `%`-notice 파싱을 `events()` 뒤로. cockpit의 `matches!(event_source, Control)` 분기 제거. 라이브 게이트: 터미널에서 psmux 윈도우 전환 → 트리 추적.
- **Phase 4 — `select()` 통일.** `select_attach`의 3-way 분기를 `backend.select() -> SelectOutcome` 뒤로. `shares_one_attachment`/`stable_per_session_attachments` 제거. host_selection_key 분류 분기도 backend로 흡수.
- **Phase 5 — Component 분해 + State 완성.** `cockpit.rs`/`switcher.rs`를 `src/app.rs`(얇은 배선) + `src/ui/{tree,terminal,popup,status}.rs`(flat·대등 Component)로 분해. 각 Component가 `&State`/`&dyn Backend`를 직접 받음. **`State`가 display(표시중 attachment + 그 address)를 단일 소유** → "표시중 == 선택" 불변식이 구조적으로 성립, 결함 A 해소. draw 게이팅·마우스 라우팅 보존.

## Self-Review (Phase 1)

- **Spec 커버리지:** §8.1(State 도입) = Task 2. State 필드는 selection 도메인만 도입(나머지 §4 필드는 §로드맵에서 해당 Phase로 명시 이연). `last_attached_sel` 제거(§8.1)는 결함 A를 현 아키텍처에서 안 고치는 사용자 결정에 따라 **이연** — Phase 5에서 State가 display를 소유하며 자연 obsolete.
- **플레이스홀더:** 없음. Task 1 Step 1의 호출처 개수는 실행 시 grep로 확정(TODO 아님).
- **타입 일관성:** `State` 필드명·타입이 Task 2 hoist 치환과 일치. `crate::prefs::` 경로가 Task 1 치환과 일치.
