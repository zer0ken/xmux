# P4 구현 계획 — UI(`switcher.rs`) 분해 (SRP · 응집도 · SoC · CQS · 테스트 가능성)

> **상태: 설계(미구현).** xmux 이상적 구조 리팩토링 · 2026-07-03 · 브랜치 `refactor/ideal-structure`
> (워크트리 `.claude/worktrees/refactor-ideal/`, 베이스 `cf5e574`, HEAD `5e51737` = cf5e574 + P0 + P1 + P2 + P3).
> 마스터 스펙: `docs/superpowers/specs/2026-07-02-ideal-structure-refactor-design.html`
> (§1 북극성 · §5 P4 표 S1-1..S1-9 · §6 불변식). 선행: P0·P1·P2·P3 완료.
> **베이스라인: 593 tests / clippy0 / fmt0.**
> 경로는 repo-상대. 모든 `file:line` 앵커는 **TRIPLE-stale**였다(0818679 감사 → osc11 병합이 switcher.rs를
> ~439줄 바꾸고 `status.rs`→`chrome.rs` 개명 → P0/P1/P2/P3 편집). 아래 앵커는 **이 워크트리의 실코드(HEAD `5e51737`)로
> 재확인·재배치한 값**이며, 구현 중 심볼/패턴으로 다시 확인한다(**LSP는 신뢰 말 것**).
> TDD 규율: 각 단계 = 실패 테스트(red, 이유 명시) → 최소 구현 → green + 회귀 0. 성공 경로는 바이트 동일.

---

## ⚠ 이 단계의 위험 프로필 (반드시 먼저 읽을 것)

- **P4는 사람 라이브 시각 게이트가 필요한 단계다**(스펙 §4 P4·§7). 자동 테스트(harness `TestBackend` 렌더 dump +
  트리 유닛 테스트)가 행 모델/네비게이션/모달 상태의 동등성은 잡지만, **실제 터미널에서 트리·모달·메뉴·팝업 드래그·필터·
  status/hint bar가 눈으로 동일하게 보이고 동작하는지**는 사람만 확인한다. 아래에서 각 단계를 **자동-테스트-충분** /
  **라이브-게이트-필요**로 표시.
- **순수 추출(배치 A)은 자동-테스트-충분**이다. 하지만 배치 A는 트리 행 모델을 재도출(`tree::flatten`)로 갈아끼우므로,
  배치 A 종료 시 **트리 시각 스냅 1회**(dump가 못 잡는 색/스타일 확인)를 권장한다. 모달/크롬을 건드리는 **배치 B·C·D는
  라이브 게이트 필수.**
- **P4는 P2/P3와 대체로 독립**(스펙 §4: "P0 후; P2/P3와 대체로 독립·병렬 가능")이다. 이 계획은 P3 tip(`5e51737`) 위에
  쌓지만, HOT 파일(`driver.rs`/두 `display.rs`)을 **전혀 건드리지 않는다**(스펙 §7 HOT은 P5–P6). 조율 의존 없음.
- **P4↔P5 경계는 명시적으로 지킨다**(스펙 §5 P5, 그래프의 `P4 -.-> P5 State 수렴 상호작용`). P4는 **UI-구조적 이동만** 한다.
  런타임 루프 재구성(`Runtime` 구조체, 913줄 god-function), `displayed`/`attach_deadline`/`focus`를 `Action`으로 미는
  **수렴(S2-2)**, `Selection`→`model` 이동(S2-5)은 **P5**다. 각 발견에서 "P5로 미루는 부분"을 명시한다.

---

## 이미 해소된 것 — P2가 한 일 (재작업 금지)

스펙의 P4 표 앵커는 P2 배치 C(inventory 단일 소유) **이전** 상태를 반영한다. 실코드는 이미 달라졌다:

- **S1-2(op-result inventory 변경이 UI에; State→UI→State 역호출) — 부분 해소됨.** 현 `state/mod.rs::apply_event`
  (286–366)가 **이벤트-구동 mutation의 단일 소유**다:
  - `HostEvent::Connected|Inventory`(295–301) → `EventEffect::ApplyInventory{host, sessions}`를 반환하고, 루프가
    파싱 세션을 `model::Host.inventory`(단일 소유)에 fold한다. **역호출 없음** — 이 경로는 P2-C가 완결.
  - `HostEvent::Sessions`(345)·`Panes`(361)·`Focus`(316) 팔은 `apply_event`(State) 안에서 각각
    `switcher.apply_source_result`/`switcher.apply_panes`/`switcher.set_active_window`를 **호출**한다. 즉 도메인 데이터
    (`state.groups`/`state.panes`/`state.scanning`/`state.panes_loaded`)의 실제 mutation이 **switcher 메서드 안에** 있고
    `apply_event`는 거기로 내려갔다 온다. 이는 "행 재도출(rebuild)"과 "도메인 mutation"이 한 switcher 메서드에 얽혀 있는
    잔여 형태다.
  - **가장 뚜렷한 잔여 역전은 OP-RESULT 경로**다: `switcher.apply_op_result`(1455–1499)가 `state.groups`를
    `tree::add_session`/`remove_session`/`rename_session`로 바꾸고 `state.panes`/`panes_loaded`를 insert/remove한다.
    이 메서드는 `State::apply`/`apply_event`를 **거치지 않고** 루프가 직접 호출한다(`app/runtime.rs:2505`
    `switcher.apply_op_result(result, &mut state)`). **이 부분이 S1-2에서 P4가 옮길 것**(아래 배치 C).
  - ⇒ **P4의 S1-2 범위 = op-result inventory mutation(+ 원하면 source/panes mutation)을 `State`로 이관.** P2가 이미 한
    Connected/Inventory→`model::Host.inventory` 경로는 **손대지 않는다.** 전면적 "모든 것이 `Action`/`Command`로 흐름"
    수렴은 **P5(S2-2)** 몫이다.

- **S1-4의 `Status` 타입은 이제 `ui/chrome.rs`의 `Chrome`이다**(osc11 개명, 스펙 §5 각주). `Chrome`은 별도 타입이지만
  여전히 **`Switcher`의 필드**(`switcher.rs:357 chrome: Chrome`)이고, app은 **7개 pass-through shim**으로만 접근한다:
  `set_spinner`(976)·`set_spinner_frame`(983)·`set_auto_hide`(988)·`set_view_border_hovered`(994)·
  `set_view_border_colors`(1001)·`set_ui_prefix`(1007)·`set_ssh_config_text`(2051). **S1-4는 미해소** — 이 shim들이
  그대로다. 유리한 사실: `Switcher::render`(1896)는 **이미 `&state`를 인자로 받는다**(호출부 runtime.rs:2211/2217,
  run.rs:64) → 크롬을 `state.chrome`로 옮겨도 render 시그니처는 안 바뀐다(배치 D 위험이 그만큼 작다).

---

## 목표 종료 형태 (스펙 §1 UI 계층)

- `tree.rs` = **순수 변환**(터미널 없이 테스트 가능): `Row`/`RowRef` + `flatten` + `visible_groups`/`first_visible_session`/
  `target_for`/`host_status` + 라벨 헬퍼.
- `ui/modal.rs` = **모달 표면 소유**: `Modal`/`Input`/`Menu`/`MenuItem`/`MenuOutcome`/`PendingKill`/`PopupDrag`/`InputMode`
  타입 + geometry/render 헬퍼 + 분류기(popup/menu/input) + self-contained 동작(help feed, 팝업 드래그, 메뉴 hover).
- `State` = **inventory mutation 단일 소유**(op-result fold 포함) + 크롬(뷰-로컬 표시 상태)의 소유(P4에서 State,
  P5에서 필요 시 `Runtime`로 재배치).
- 잔여 `Switcher` = **커서/선택 상태 기계 + 입력 디스패치 + rebuild 오케스트레이션(capture→flatten→restore) + render**.

---

## 서브배치 분할 (checkpoint 가능 단위)

| 배치 | 내용 (발견) | 위험 | 게이트 |
|---|---|---|---|
| **A** (순수 추출) | A1 = S1-7(`map_color` 이동), A2 = **S1-3+S1-5**(`Row`/`RowRef`+`tree::flatten`+갇힌 순수 함수), A3 = S1-6(필터-매칭/주소 문법 dedup) | 낮음 · 바이트 동일 | **자동-테스트-충분** (A 끝 트리 시각 스냅 1회 권장) |
| **B** (모달 모듈) | S1-9 → `ui/modal.rs`: B1 타입, B2 geometry/render 헬퍼, B3 분류기+self-contained 동작 | 중 · 새 파일 · 경로 ripple | **라이브 게이트** (B 끝: 모달·메뉴·드래그·입력·kill) |
| **C** (State op-result fold) | S1-2(잔여) = `apply_op_result` inventory mutation을 `State`로 | 중 · 도메인 mutation 이동 | **라이브 게이트(경량)** (create/kill/rename/new-window/split) |
| **D** (크롬 소유권) | S1-4 = app/State가 `Chrome` 소유 + `flash()` API + 7 shim 삭제, S1-8(CQS) 문서화 | **가장 높음** · render/호출부 재배선 | **라이브 게이트** (뷰 보더·hint bar·스피너·host-info·flash) |

**배치 간 독립성**: A/B/C는 서로 독립(순서 무관·병렬 가능). D는 render 본문을 `state.chrome`로 바꾸므로 **가장 침습적 →
마지막.** 스펙 순서 지침("순수 추출 먼저 → 모달 → Status/app 마지막")대로 **A → B → C → D**로 확정한다. C(State fold)는
A/B와 독립이나 도메인-소유 이동이라 D 앞에 둔다.

---

## 단계 순서 & 의존 그래프

```
A1 (map_color 이동)              ─┐
A2 (Row/RowRef + tree::flatten)  ─┼─ 배치 A: 순수·자동-테스트-충분 (A2 후 A3 재사용)
A3 (필터/주소 문법 dedup)        ─┘        │ A 끝: 트리 시각 스냅 1회
                                            ▼
B1 (모달 타입) → B2 (geometry/render 헬퍼) → B3 (분류기 + self-contained 동작)   ← 배치 B: 라이브 게이트
                                            ▼
C1 (op-result inventory → State)                                                ← 배치 C: 경량 라이브 게이트
                                            ▼
D1 (Chrome 소유권 + flash() + shim 삭제) → D2 (S1-8 CQS 문서화)                  ← 배치 D: 라이브 게이트
```

- **A2가 A3의 전제**: A2가 `tree::first_visible_session`을 만들면 A3의 "필터-매칭 규칙 dedup" 절반이 이미 완결 → A3는
  주소 문법(`session:window`, `source/session`) 통일만 남는다.
- **B는 A와 독립**(모달 타입은 트리 행 모델과 별개 파일)이나, `ui/mod.rs`에 `mod modal;` 추가가 B1의 첫 편집.
- **D를 마지막에** 두는 근거: render 본문이 `self.chrome`→`state.chrome`로 바뀌면 A/B/C가 건드린 render 경로와 겹치므로,
  그 이동들이 정착한 뒤 한 번만 지나간다.

---

## 배치 A — 순수 추출 (바이트 동일 · 자동-테스트-충분)

### 단계 A1 — S1-7: `map_color`를 `switcher.rs` 밖으로

**(a) 목표**
tmux 색 토큰 파싱(`map_color`, switcher.rs:43–85 — 16 named + `bright*` + `colourN`/`colorN` + `#RRGGBB` + `fg=` 접두 +
`default`)을 god-module에서 빼내 응집도/SoC를 높인다. 프로덕션 유일 호출부는 `app/runtime.rs:1878–1880`(config의
`view_*_border_style` 문자열 → `ViewBorderColors`)뿐이다.

**(b) 실패 테스트 먼저**
`map_color`의 세 유닛 테스트(`map_color_named_and_default` 2420, `map_color_indexed_and_hex` 2435,
`map_color_tolerates_fg_prefix_and_case` 2442)를 새 모듈의 `mod tests`로 이관:
- 단언(그대로): `assert_eq!(map_color("green"), Color::Green); assert_eq!(map_color("colour4"), Color::Indexed(4));
  assert_eq!(map_color("#268bd2"), Color::Rgb(0x26,0x8b,0xd2)); assert_eq!(map_color("fg=blue"), Color::Blue);
  assert_eq!(map_color(""), Color::Reset);`
- 오늘 red인 이유: 이관 후 `crate::ui::chrome::map_color`(권장 홈, 아래 설계 선택 §1) 경로가 없어 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색; 설계 선택 §1 = `ui/chrome.rs`)
- `pub fn map_color`(43–85)를 **`ui/chrome.rs`로 이동**(chrome은 이미 `ratatui::style::Color`와 `ViewBorderColors`를
  소유 — 유일 소비자와 동거, 새 크로스-레이어 의존 0). switcher.rs의 `pub use` 재-export는 두지 않고 호출부를 직접 갱신.
- `app/runtime.rs:1878–1880` `crate::ui::switcher::map_color` → `crate::ui::chrome::map_color`.
- `config.rs:46`의 독스트링 링크 `[crate::ui::switcher::map_color]` → `[crate::ui::chrome::map_color]`(AS-IS).
- switcher.rs 상단의 `map_color` 독스트링/함수 삭제.

**(d) 검증**
`cargo test -p xmux` red→green. 이관된 3 테스트 green. `grep -n "map_color" src/ui/switcher.rs` → 0. runtime/config 빌드.
`cargo clippy -- -D warnings`/`cargo fmt --check` 클린. **자동-테스트-충분**(런타임 동작 바이트 동일; 경로만 이동).

**(e) 파일**: `src/ui/switcher.rs`, `src/ui/chrome.rs`, `src/app/runtime.rs`, `src/config.rs`(독스트링)

---

### 단계 A2 — S1-3 + S1-5: `Row`/`RowRef` + `tree::flatten` + 갇힌 순수 함수 → `tree.rs`

> **P4 최대 순수-win이자 A의 위험 집결지.** 트리 전체 행 모델의 소유가 이동하고, 이후 렌더/네비게이션/모달이 모두
> 이 위에서 돈다. red-first로 신규 `tree` 유닛 테스트를 세운 뒤 기존 switcher 렌더/네비 테스트가 그린인지 반드시 확인.

**(a) 목표**
1. **S1-3**: `Switcher::rebuild`(484–618)가 하는 5가지 일 중 **순수 행 생성**(498–595)을 `tree::flatten`으로 추출.
   `rebuild`는 **capture→flatten→(rows에서 preselect 도출)→set_selected→restore** 오케스트레이션만 남긴다.
2. **S1-5**: `&self` 메서드에 갇힌 순수 계산 `visible_groups`(443–463)·`first_session_of`(792–809)·`target_for`(811–830)·
   `host_status`(623–633)와 라벨 헬퍼(`session_label` 635, `window_label` 2282, `pane_label` 2286, `plural` 2261)를
   `tree.rs`로 승격 — 터미널 없이 유닛 테스트 가능하게.
3. **핵심 설계**: `Row`/`RowRef`를 `tree.rs`로 옮기되 `Row`의 `ratatui::style::Color` 필드는 **제거**한다(설계 선택 §2).
   `tree.rs`는 현재 ratatui-무의존 순수 모델이며, 그 무의존이 "터미널 없이 테스트 가능"의 핵심이다. 색은 `RowRef`
   변종(=레벨)에서 순수하게 결정되므로 render에서 파생한다.

**(b) 실패 테스트 먼저** (`ui/tree.rs`의 `mod tests`)
- `flatten_builds_host_session_window_pane_rows`: 1 host + 1 session(windows 로드됨, 2 window, 각 1 pane)인 groups/panes로
  `tree::flatten(...)` 호출 → `reference` 시퀀스가 `[Host, Session, Window, Pane, Window, Pane]`, indent가 `[0,2,4,6,4,6]`.
- `flatten_marks_active_window_and_pane`: 활성 window/pane의 `Row.active == true`, 그 외 false.
- `flatten_shows_loading_placeholder_when_panes_unloaded`: `panes_loaded`에 주소 없음 → session 아래 `RowRef::Loading` 1행.
- `flatten_skips_session_rows_while_scanning`: `scanning`에 source 포함 → 그 host는 session 행을 만들지 않고 `host_status`가
  `Some("scanning…")`.
- `first_visible_session_respects_filter`: source-match면 첫 세션, session-match면 매칭 첫 세션, unreachable면 None.
- `host_status_reports_scanning_unreachable_empty`: scanning→`"scanning…"`, err→`"⚠"`, 빈 sessions→`"(empty)"`, else None.
- 오늘 red인 이유: `tree::flatten` / `tree::Row` / `tree::first_visible_session` / `tree::host_status`가 없어 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색)
- `tree.rs`로 이동: `RowRef`(switcher.rs:112–125, 순수 — 그대로), `Row`(249–261)를 **`Color` 필드 없이**
  `{ label: String, status: Option<String>, indent: usize, reference: RowRef, active: bool }`로. `Row::selectable`(264)도 이동.
- `pub fn flatten(groups: &[Group], panes: &HashMap<String, Vec<WindowPanes>>, panes_loaded: &HashSet<String>,
  scanning: &HashSet<String>, filter: &str) -> Vec<Row>`: 본문은 rebuild 498–595 이식 — 내부에서 `visible_groups(groups,
  filter)`로 가시 그룹을 얻고, `name_col_width`를 그 안에서 계산해 라벨 패딩에 반영, host/session/window/pane/loading 행 생성.
  *(설계 선택 §3: 발견의 4-인자 스케치 `flatten(groups,panes,panes_loaded,filter)`는 `scanning`을 빠뜨렸다 — 실코드는
  host_status와 "scanning이면 세션 미전개"에 `scanning`이 필수라 5-인자로 둔다.)*
- 순수 함수 이동: `visible_groups`→`pub fn visible_groups(groups, filter) -> Vec<Group>`(내부 XM-01 빈-필터 폴백 유지),
  `first_session_of`→`pub fn first_visible_session(group: &Group, filter) -> Option<Session>`(S1-6과 공유),
  `host_status`→`pub fn host_status(g: &Group, scanning: bool) -> Option<String>`, `session_label`(name_col_width 인자화)·
  `window_label`·`pane_label`·`plural` 이동. `target_for`→`pub fn target_for(reference: &RowRef, groups, filter) ->
  (String, String)`(source, target) 반환 — `TerminalViewTarget`(switcher, `pub` 유지)은 switcher에서 그 튜플로 조립하거나
  `TerminalViewTarget`을 `tree`로 함께 이동(설계 선택 §4).
- `rebuild`(484)를 오케스트레이션으로 축소: `let keep=…; let rows = tree::flatten(&state.groups, &state.panes,
  &state.panes_loaded, &state.scanning, &state.filter); self.rows = rows; self.name_col_width`는 flatten이 라벨에 흡수하므로
  **제거**; kill-target 무효화(603–605) 유지; preselect(`preferred_row`/`first_session_row`)는 flatten 결과 rows를 훑어
  도출(첫 `RowRef::Session` = first_session_row; `self.preferred`와 주소가 같은 session row = preferred_row); `set_selected`.
- `render_tree`(1971)가 `RowRef`→`Color` 매핑을 담당: Host→`COLOR_HOST`, Session→`COLOR_SESSION`, Window→`COLOR_WINDOW`,
  Pane→`COLOR_PANE`, Loading→`COLOR_HINT`(상수 29–35는 switcher/render 쪽 유지). `visible_groups`/`first_session_of`/
  `target_for`/`host_status`/`session_label`의 switcher 메서드는 `tree::…` 호출로 교체하거나 제거(호출부만 남김).

**(d) 검증**
`cargo test -p xmux` red→green. 신규 `tree` 유닛 테스트 green. **회귀 게이트(그린 유지)**: switcher 네비게이션·렌더 테스트 —
`right_descends_left_ascends_tree_levels`(2901)·`up_down_move_within_level_and_hjkl_match_arrows`(2935)·
`active_window_is_bold_italic`(2824)·`active_window_pane_have_no_text_marker`(2993)·
`removed_window_selection_falls_to_previous_sibling_then_parent`(5519)·`select_address_moves_cursor_to_named_session`(5637)·
`session_label_pads_by_display_width_not_char_count`(5682)·`render_tree_width_zero_gives_terminal_full_width`(5553)·
`from_sources_renders_scanning_skeletons` 및 run.rs dump 테스트(`dump_switcher_flattens_buffer`·`dump_screen_renders_the_live_grid`).
`grep -n "fn flatten\|struct Row\|enum RowRef" src/ui/switcher.rs` → 0(이동 완결). clippy/fmt 클린.
- **자동-테스트-충분**(행 시퀀스/indent/active/라벨/색 매핑을 dump·유닛이 커버). 단 색/스타일은 dump가 못 잡으므로
  **배치 A 종료 시 트리 시각 스냅 1회**(host=노랑·session=초록·window=자홍·pane=청록·active=bold+italic·scanning/⚠/(empty)
  dim)에 포함.

**(e) 파일**: `src/ui/tree.rs`, `src/ui/switcher.rs`

---

### 단계 A3 — S1-6: 필터-매칭 규칙 + `source/session` 주소 문법 중복 제거

**(a) 목표**
(1) 필터-매칭 중복 — `first_session_of`가 `filter_groups`의 규칙을 한 그룹에 대해 재구현하던 것(A2가 이미
`tree::first_visible_session` 하나로 통일). (2) 주소 문법 중복 — `session:window`가 3곳에서 손-포맷(`format!("{}:{}", …)`),
`source/session` join·split이 산발. **`session:window`는 `crate::mux::window_target`**(mux/vocab.rs:77, 기존·테스트됨),
**`source/session`는 하나의 주소 헬퍼**로 통일한다(DRY).

**(b) 실패 테스트 먼저**
- `mux::window_target`는 이미 테스트됨(vocab.rs:402). 새로 `session::source_of_returns_the_source_half`:
  `assert_eq!(session::source_of("jup/api"), "jup"); assert_eq!(session::source_of("local/a/b"), "local");`
  (첫 `/` 앞, `parse_target`과 같은 분리 규칙).
- 오늘 red인 이유: `session::source_of`(또는 재사용 헬퍼)가 없어 컴파일 실패.

**(c) 최소 구현**
- `src/session.rs`에 `pub fn source_of(addr: &str) -> &str { addr.split('/').next().unwrap_or(addr) }`(또는
  `parse_target(addr).ok().map(|s| s.source)` 재사용) 추가 — 첫 `/` 규칙은 `address()`(23)·`parse_target`(48)과 일치.
- switcher.rs `session:window` 손-포맷 3곳을 `crate::mux::window_target`로: `menu_title`(199)·`target_for`(→A2로 tree.rs
  이동됐으면 거기 826 상당)·`open_new` SplitWindow 팔(1238). *(A2에서 이미 tree로 옮긴 `target_for`도 동일 교체.)*
- `set_active_window`(953)의 `format!("{source}/{session}")`는 `Session{source,name}.address()` 또는 소형 join 헬퍼로,
  `restore_focus`(1679)의 `addr.split('/').next()`는 `session::source_of(&addr)`로.

**(d) 검증**
`cargo test -p xmux` red→green. `source_of` 테스트 green; `set_active_window_moves_the_marker`(2845)·
`select_window_follows_*`·menu 타이틀 테스트 그린. `grep -nE "\"\\{\\}:\\{\\}\"|split\('/'\)" src/ui/switcher.rs` →
의도된 0(또는 헬퍼 경유만). clippy/fmt 클린. **자동-테스트-충분.**

**(e) 파일**: `src/session.rs`, `src/ui/switcher.rs`(+ A2에서 옮긴 `tree.rs`의 `target_for`)

---

## 배치 B — `ui/modal.rs` (S1-9) — 라이브 게이트

> 현재 모달 관련 표면은 switcher.rs 전역에 흩어져 있고(타입·geometry·render·동작), 분류기는 `State`에 있다. 이 배치는
> **모달 표면의 소유를 새 `ui/modal.rs`로 모은다.** 단, `open_new`/`arm_kill`/`menu_release`/`queue_*`처럼 **switcher의
> 선택·네비게이션 상태에 결합된 동작**은 switcher에 얇게 남기고(모달 생성자만 modal.rs에서), self-contained 부분만 옮긴다
> (설계 선택 §5). 전면적 "모달 커밋이 `Action`으로 흐름"은 P5 수렴 몫.

### 단계 B1 — 모달 데이터 타입을 `ui/modal.rs`로

**(a) 목표**
`Modal`(313–318)·`Input`(293–304)·`InputMode`(285–291)·`Menu`(165–171)·`MenuItem`(131–149)·`MenuOutcome`(154–158)·
`PopupDrag`(176–179)·`PendingKill`(98–106)를 `ui/modal.rs`로 이동. `State.modal`(state/mod.rs:52)과 switcher/테스트의 경로를
`crate::ui::modal::…`로 통일.

**(b) 실패 테스트 먼저**
`ui/modal.rs`의 `mod tests`에 `modal_help_variant_constructs`:
`let _m = crate::ui::modal::Modal::Help;` + `assert!(matches!(crate::ui::modal::Modal::Help, Modal::Help));`
- 오늘 red인 이유: `crate::ui::modal` 모듈/타입이 없어 컴파일 실패.

**(c) 최소 구현**
- `src/ui/mod.rs`(13줄)에 `pub mod modal;` 추가.
- 위 8개 타입 + `MenuItem::label`(140)을 `ui/modal.rs`로 이동. `pub(crate)` 가시성 유지.
- `state/mod.rs`: `crate::ui::switcher::Modal`(52·62·72·77·87) → `crate::ui::modal::Modal`.
- switcher.rs: 타입 정의 삭제, `use crate::ui::modal::{Modal, Input, …}`. 필요한 곳(예: `apply_op_result`가 `OpResult`를
  쓰듯 외부가 참조하는 경우)만 `pub use crate::ui::modal::…` 재-export.
- 테스트 모듈(switcher.rs 2415+): `RowRef`(A2에서 tree로 이동)·`Modal` 경로 갱신 — 예: `modals_are_mutually_exclusive`(5117)·
  `closed_popup_cannot_be_grabbed_even_with_a_stale_rect`(5146) 등.

**(d) 검증**
`cargo test -p xmux` red→green. 모달/메뉴/팝업 관련 기존 테스트 전부 그린(경로만 이동, 로직 불변). clippy/fmt 클린.
**자동-테스트-충분(컴파일 단계)** — 시각 게이트는 B 끝에서 일괄.

**(e) 파일**: `src/ui/mod.rs`, `src/ui/modal.rs`, `src/ui/switcher.rs`, `src/state/mod.rs`

---

### 단계 B2 — geometry/render 헬퍼를 `modal.rs`로

**(a) 목표**
모달 표면의 **self-contained geometry/render 헬퍼**를 modal.rs로 이동: `menu_items`(183)·`menu_title`(195)·
`Menu::item_at`/`contains`(2323–2344)·`menu_rect`(2349)·`input_lines`(2206)·`confirm_lines`(2217)·`input_title`(2404)·
`centered_rect`(2375)·`offset_centered`(2389)·`render_popup`(2243)·`wrap_text`(208). `help_lines`(2057)도 모달 팝업
콘텐츠이므로 modal.rs로(설계 선택 §5). 이들은 switcher 상태를 안 읽는 순수/렌더 헬퍼다(`render_popup`은 `Frame`만).

**(b) 실패 테스트 먼저**
기존 geometry 테스트를 modal.rs `mod tests`로 이관: `menu_rect_clamps_into_screen`(4526)·
`menu_rect_fits_a_title_wider_than_the_items`(4543)·`menu_rect_measures_cjk_title_by_display_width`(4554)·
`wrap_text_wraps_on_words_and_hard_splits_long_words`(5284)·`popup_blanks_only_a_wide_glyph_bisected_by_the_left_border`(4969).
- 오늘 red인 이유: 이관 후 `crate::ui::modal::menu_rect`/`wrap_text`/`render_popup` 경로가 없어 컴파일 실패.

**(c) 최소 구현**
- 위 헬퍼를 modal.rs로 이동(가시성: `wrap_text`는 chrome.rs가 `use crate::ui::switcher::{fit, wrap_text}`로 쓰므로 —
  chrome.rs를 `crate::ui::modal::wrap_text`로 갱신; `fit`은 switcher에 남거나 함께 이동 검토). `render_popup`이 쓰는 `Line`/
  `Block`/`Paragraph` import를 modal.rs로.
- `render_modal_popup`(2158)·`render_menu`(2181)는 `&self`(popup_rect·screen_area)와 `state.modal`을 읽으므로 **switcher에
  남기되** 콘텐츠 조립(`help_lines`/`input_lines`/`confirm_lines`)과 박스 그리기(`render_popup`)를 modal.rs 헬퍼로 호출.

**(d) 검증**
`cargo test -p xmux` red→green. 이관 geometry/wrap 테스트 green. `popup_renders_without_panicking_on_a_narrow_screen`(5163)·
`popup_drag_clamps_within_screen`(5189)·`long_flash_wraps_in_narrow_hint_bar_instead_of_clipping`(4757, chrome의 wrap 경유)
그린. clippy/fmt 클린. **자동-테스트-충분(컴파일 단계).**

**(e) 파일**: `src/ui/modal.rs`, `src/ui/switcher.rs`, `src/ui/chrome.rs`(`wrap_text` 경로)

---

### 단계 B3 — 분류기 + self-contained 동작을 `modal.rs`로

**(a) 목표**
(1) **분류기**: `State`의 `is_modal_popup_open`(61)·`is_inputting`(71)·`menu_active`(76)·`modal_kind`(85)를
`modal.rs`의 자유 함수(`Option<&Modal>` 위)로 옮기고, `State`는 얇은 위임(`modal::is_popup_open(&self.modal)` 등)만 남긴다 —
모달 분류 규칙의 SSOT를 타입 옆으로. (2) **self-contained 동작**: `feed_help_key`(1100, help 모달 키 소비)·팝업 드래그
geometry(`popup_drag_active` 1041·`begin_popup_drag` 1047·`drag_popup` 1072·`end_popup_drag` 1084·`reset_popup_pos` 1089)·
`menu_hover`(1818)를 modal.rs로(드래그 상태 `popup_offset`/`popup_rect`/`popup_drag`를 담는 소형 `PopupGeometry`
구조로 묶어 이동 검토, 설계 선택 §6).

**(b) 실패 테스트 먼저**
- `modal::popup_kind_classifies_help_input_kill_as_popup_menu_as_menu`: `modal::modal_kind(&Some(Modal::Help)) ==
  Some(ModalKind::Popup)`, `…(&Some(Modal::Menu(_))) == Some(ModalKind::Menu)`, `…(&None) == None`.
- `modal::help_feed_consumes_and_closes_on_q_or_esc`: help 열린 상태에서 `q`/`Esc`가 닫고 true 반환, 다른 키는 삼키고 유지.
- 오늘 red인 이유: `modal::modal_kind`/`modal::feed_help` 자유 함수가 없어 컴파일 실패.

**(c) 최소 구현**
- `modal.rs`: `pub(crate) fn modal_kind(m: &Option<Modal>) -> Option<ModalKind>` 등 4 분류기. `State`의 4 메서드는
  `modal::…(&self.modal)` 위임으로 축소(호출부 `state.is_inputting()` 등은 유지 — API 표면 불변).
- `feed_help_key`/드래그 geometry/`menu_hover`를 modal.rs로 이동(switcher는 `state.modal`/드래그 geometry에 위임하는
  얇은 진입점만; 드래그 geometry 필드가 `PopupGeometry`로 묶이면 switcher는 `self.popup_geo` 하나만 보유).
- **switcher 잔류(P4 범위 밖 이동)**: `open_input`(1157)·`open_new`(1210)·`arm_kill`(1509)·`resolve_kill`(1530)·
  `menu_open`(1782)·`menu_release`(1831)·`queue_*`(1324–1450)는 `current_ref`/`set_selected`/`capture_focus`/`restore_focus`/
  `state.apply`에 결합 → switcher에 남기고 modal 생성자/`menu_items`만 modal.rs에서 가져다 쓴다.

**(d) 검증**
`cargo test -p xmux` red→green. `default_state_is_empty`(state, 분류기 위임 확인)·`feed_help_key_is_modal_and_closes_on_q_or_esc`
(5214)·`toggle_help_flips_visibility`(5203)·`popup_border_press_then_drag_moves_the_rect`(5096)·
`popup_interior_press_does_not_grab`(5175)·`modals_are_mutually_exclusive`(5117) 그린. clippy/fmt 클린.
- **⚠ 라이브 게이트(사람) 필수(B 종합)**: 실 터미널에서 (1) `?` help 모달 열림/`q`·Esc 닫힘, (2) `/` 필터·`n` new·`R` rename
  입력 모달, (3) `x` kill y/n confirm, (4) 우클릭 메뉴 press-hold-release + hover 하이라이트, (5) 팝업 보더 드래그 이동이
  P4 이전과 **시각·동작 동일**한지 확인.

**(e) 파일**: `src/ui/modal.rs`, `src/ui/switcher.rs`, `src/state/mod.rs`

---

## 배치 C — `State`가 op-result inventory fold 소유 (S1-2 잔여) — 경량 라이브 게이트

### 단계 C1 — `apply_op_result`의 inventory mutation을 `State`로

**(a) 목표**
mux op 결과(`OpResult` — Created/Renamed/Killed/PanesRefreshed)의 **도메인 inventory mutation**이 현재 `switcher.apply_op_result`
(1455–1499) 안에서 일어나고 루프가 이를 직접 호출한다(`app/runtime.rs:2505`) — `State::apply`/`apply_event`를 우회하는
잔여 State→UI 역전(위 "이미 해소된 것" 참조). 이 mutation(`tree::add_session`/`remove_session`/`rename_session`,
`state.panes`/`panes_loaded` insert/remove)을 **`State`로 이관**해 State가 inventory mutation의 단일 소유가 되게 하고,
switcher는 **행 재도출(rebuild) + 커서 복원/reselect**만 한다(SoC/결합도/DIP). `Failed`의 flash는 UI 표시이므로 switcher 잔류.

**(b) 실패 테스트 먼저** (`state/mod.rs`의 tests)
- `state::apply_op_result_created_adds_session_and_panes`: `state.fold_op_result(OpResult::Created{session, panes})` →
  `state.groups`에 세션 추가, `state.panes`/`panes_loaded`에 주소, 반환값 = reselect 주소 `Some(addr)`.
- `state::apply_op_result_killed_removes_session`·`…_renamed_moves_panes_and_renames`: 대응 mutation 검증.
- 오늘 red인 이유: `State::fold_op_result`(신규)가 없어 컴파일 실패.

**(c) 최소 구현** (설계 선택 §7 = State 메서드 fold, 커서 힌트 반환)
- `state/mod.rs`에 `pub fn fold_op_result(&mut self, result: OpResult) -> OpFollow` 추가 —
  Created/Renamed/Killed/PanesRefreshed의 `state.groups`/`panes`/`panes_loaded` mutation(현 apply_op_result 1457–1494 이식)을
  수행하고, switcher가 필요로 하는 후속(예: Created의 reselect 주소, Failed의 flash 메시지)을 `OpFollow`
  (`{ reselect: Option<String>, flash: Option<String> }` 같은 소형 반환)로 돌려준다.
- `switcher.apply_op_result`(1455)를 축소: `let follow = state.fold_op_result(result); self.rebuild(state);
  if let Some(addr)=follow.reselect { self.user_moved=true; if let Some(i)=self.row_of_session(&addr){ self.set_selected(i,state);}}
  if let Some(msg)=follow.flash { /* flash via state.chrome — D 이후엔 state.flash(msg) */ }`.
  `PanesRefreshed`는 현재도 `self.apply_panes`로 rebuild+restore하므로 그 경로 유지(또는 fold가 panes만 넣고 switcher가 rebuild).
- 루프 호출부 `runtime.rs:2505`는 **`switcher.apply_op_result(result, &mut state)` 그대로**(이제 내부가 얇아짐).
- *(선택)* `apply_source_result`(1597)/`apply_panes`(1642)의 도메인 mutation도 같은 방식으로 `State`로 밀어 switcher를
  rebuild+restore로 축소 가능 — 단 이는 `apply_event`(State)가 이미 이 메서드를 호출하는 구조라 **P5 수렴과 겹친다**;
  P4에서는 **op-result 경로만** 확정 이관하고 source/panes 이관은 P5(S2-2)로 미룬다(설계 선택 §7).

**(d) 검증**
`cargo test -p xmux` red→green. 신규 State fold 테스트 + 기존 op 테스트(switcher 테스트 모듈의 create/kill/rename/split 계열,
`only_run_op` 헬퍼 경유) 그린 — 행/선택 결과 불변. `apply_event_*`(state) 전부 그린(건드리지 않음). clippy/fmt 클린.
- **⚠ 라이브 게이트(경량)**: 트리에서 new session/kill(y)/rename/new window/split이 실제로 트리에 반영되고 커서가 올바른
  세션에 착지하는지 실기(jupiter06 throwaway). *(argv/상태는 자동이 잡지만 라이브 트리 반영 타이밍은 눈으로.)*

**(e) 파일**: `src/state/mod.rs`, `src/ui/switcher.rs`, `src/ui/ops.rs`(`OpFollow`를 두면 여기 또는 model)

---

## 배치 D — 크롬 소유권 (S1-4) + CQS 문서화 (S1-8) — 라이브 게이트 · 마지막

### 단계 D1 — app/State가 `Chrome` 소유 + `flash()` API + 7 shim 삭제

**(a) 목표**
`Chrome`이 `Switcher`의 필드(357)이고 app이 7 pass-through shim으로만 접근하는 것을 끝낸다. `Chrome`을 **단일 소유**로 옮기고
(설계 선택 §8 = P4는 `State.chrome`, `Modal` 선례와 대칭), `flash()` API를 제공하고, 7 shim(976–1009·2051)을 삭제한다
(응집도/결합도/데메테르). render는 이미 `&state`를 받으므로(1896) `state.chrome`을 읽게만 바꾼다.

**(b) 실패 테스트 먼저**
- `state::flash_sets_message_and_key_clears_it`: `state.flash("boom"); assert_eq!(state.chrome.flash, "boom");`
  (또는 접근자 `state.flash_text()`), 그리고 네비 키 후 비워짐(handle_key clear 경로).
- 컴파일 레벨 red: app이 `state.chrome`을 세팅하도록 바꾼 뒤 7 switcher shim이 없어 `switcher.set_spinner(...)` 호출이
  **컴파일 실패**(shim 삭제 유도).
- 오늘 red인 이유: `State::flash`/`state.chrome`가 없어 컴파일 실패.

**(c) 최소 구현** (설계 선택 §8)
- `chrome: Chrome` 필드를 `Switcher`(357)에서 `State`(`state/mod.rs`)로 이동(`pub(crate) chrome: Chrome`).
- `Switcher::render`(1896) 본문: `self.chrome.hint_bar_lines`/`render_hint_bar`/`render_view_border`/`render_host_info`
  (1937–1957)와 `render_tree`(1977·2008–2009)의 `self.chrome.spinner`/`spinner_frame` 읽기 → `state.chrome.…`.
  render 시그니처는 **불변**(이미 `state` 보유).
- flash 쓰기 사이트(switcher 1128·1158·1173·1211·1214·1412·1441·1496·1513) → `state.flash(msg)` 또는 `state.chrome.flash=`;
  이들 메서드는 이미 `&mut state`를 받는다(확인됨). C1의 `Failed` flash도 `state.flash(msg)`로.
- **7 shim 삭제**(switcher 976–1009·2051). app 호출부를 `state.chrome` 직접 세팅으로:
  `runtime.rs:1872 set_ssh_config_text`→`state.chrome.set_ssh_config_text(...)`, `1877–1883 set_view_border_colors/set_ui_prefix`,
  `1991–1992 set_spinner_frame/set_view_border_hovered`, `2166 set_auto_hide`, `2541 set_spinner`. (Chrome의 `pub(crate)`
  세터는 유지 — 이제 `state.chrome`가 소유하고 app이 직접 호출.)
- 테스트의 `sw.chrome.flash`/`h.sw.chrome.flash`(4074·4762·4785·5483·5495·5511 등) → `state.chrome.flash`/접근자로 갱신.

**(d) 검증**
`cargo test -p xmux` red→green. hint bar/뷰 보더/host-info/스피너/flash 관련 테스트 —
`hint_bar_text_reflects_configured_prefix`(3778)·`long_flash_wraps_in_narrow_hint_bar_instead_of_clipping`(4757)·
`render_terminal_view_none_grid_is_blank_not_attaching`(4666)·flash 단언 테스트들 그린. `grep -n "set_spinner\|set_auto_hide\|
set_view_border\|set_ui_prefix\|set_ssh_config_text" src/ui/switcher.rs` → 0(shim 소멸). clippy(미사용 경고 0)/fmt 클린.
- **⚠ 라이브 게이트(사람) 필수**: 실 터미널에서 (1) 포커스에 따른 뷰 보더 accent 위치(트리=위/터미널=아래) + auto-hide ║/│ +
  hover ┃, (2) hint bar의 help/scanning/filter/flash 라인, (3) 세션 스피너, (4) unreachable host의 info 패널(ssh 스탠자),
  (5) 에러 시 flash(예: "cannot kill a host")가 P4 이전과 **시각 동일**한지 확인.

**(e) 파일**: `src/state/mod.rs`, `src/ui/switcher.rs`, `src/ui/chrome.rs`, `src/app/runtime.rs`

---

### 단계 D2 — S1-8: mutate-and-return-bool(CQS) 관례 문서화 `[Low · 코드 무변경]`

**(a) 목표**
"뮤테이트하고 bool 반환" 스멜(`select_window` 868·`select_address` 904·`select_active_window` 925·`set_active_window` 946·
`menu_open` 1782·`begin_popup_drag` 1047 — 모두 "실제로 움직였/잡았는가"를 반환해 app이 후속(attach/이벤트 소비)을 결정)을
**수용된 관례로 문서화**한다(설계 선택 §9 = dirty 신호 분리는 Low 대비 churn 과다이므로 하지 않음).

**(b) 실패 테스트 먼저** — 없음(문서 전용). 회귀 방지로 기존 관련 테스트가 그린인지만 확인.

**(c) 최소 구현**
- `src/ui/AGENTS.md`(Invariants 절)에 한 줄: "선택/드래그 뮤테이터는 `bool`(‘실제로 이동/그랩했는가’)을 반환하는 것이
  수용된 관례다 — app이 이 신호로 후속(attach/이벤트 소비)을 게이트한다. 순수 CQS 분리는 하지 않는다." (AS-IS 서술.)

**(d) 검증** `cargo test`/clippy/fmt 불변(코드 무변경). **자동-테스트-충분(라이브 불요).**

**(e) 파일**: `src/ui/AGENTS.md`

---

## 설계 선택 (구현 전 확정 — 리뷰 포인트)

1. **`map_color` 홈 = `ui/chrome.rs`**(발견은 `config`/`mux::vocab` 제시). 근거: `config.rs`·`mux/vocab.rs`는 **현재
   ratatui-무의존**(grep 확인) — `ratatui::style::Color`를 반환하는 함수를 넣으면 새 크로스-레이어 의존이 생기고, 특히
   `mux::vocab`는 불변식 "이 계층은 UI에 분기하지 않음"을 깬다. `chrome.rs`는 이미 `Color`/`ViewBorderColors`를 소유한
   유일 소비자와 동거 → god-module에서 빼내되(발견 취지 충족) 레이어링을 지킨다. **리뷰에서 config로 강제하려면** config가
   ratatui 의존을 받아들일지 결정 필요.
2. **`Row`에서 ratatui `Color` 필드 제거**(색은 render에서 `RowRef`→`Color`로 파생). 근거: S1-3/S1-5의 목적이 "터미널 없이
   테스트 가능"인데 `Row`가 ratatui 타입을 품으면 그 목적이 훼손된다. 색은 레벨(RowRef 변종)의 순수 함수이므로 render 파생이
   자연스럽다. 대안(‘tree.rs가 ratatui 의존을 받음’)은 기각.
3. **`tree::flatten` 시그니처에 `scanning` 포함**(발견의 4-인자 스케치는 누락). 실코드는 `host_status`와 "scanning이면 세션
   미전개"에 `scanning` 집합이 필수.
4. **`TerminalViewTarget`/`target_for` 배치**: `target_for`를 `tree.rs`로 옮기며 `(source, target)` 튜플을 반환하고 switcher가
   `TerminalViewTarget`으로 조립(pub 타입은 switcher 유지) — 또는 `TerminalViewTarget`을 tree로 함께 이동. 둘 다 순수;
   **튜플 반환**을 권장(pub API 표면 이동 최소화). 리뷰에서 확정.
5. **S1-9 동작 분할선**: modal.rs = 타입 + geometry/render 헬퍼 + 분류기 + **self-contained 동작**(help feed, 팝업 드래그,
   메뉴 hover). switcher 잔류 = **선택/네비게이션 결합 동작**(`open_new`/`arm_kill`/`menu_release`/`queue_*` — `current_ref`/
   `set_selected`/`capture_focus`/`restore_focus`/`state.apply`에 의존). 전면 "모달 커밋→`Action`" 수렴은 **P5**.
6. **팝업 드래그 상태**: `popup_offset`/`popup_rect`/`popup_drag`(switcher 357–366)를 소형 `PopupGeometry`로 묶어 modal.rs로
   옮길지, 필드 3개를 switcher에 두고 로직만 modal 자유 함수로 옮길지 — **`PopupGeometry` 묶음**을 권장(응집). 리뷰 확정.
7. **S1-2 깊이 = op-result 경로만 P4**. `fold_op_result`를 `State` 메서드로 두어 inventory mutation을 State가 소유,
   switcher는 rebuild+reselect. `apply_source_result`/`apply_panes`의 mutation 이관과 "모든 것이 `Command`/`Action`으로 흐름"
   전면 수렴은 **P5(S2-2)** — 여기서 하면 런타임 루프/수렴과 겹쳐 P4 범위(UI-구조)를 넘는다.
8. **크롬 소유 = P4에서 `State.chrome`**(발견 문구는 "app이 소유"). 근거: `Modal`이 이미 `State.modal`로 옮겨진 선례와 대칭,
   그리고 **`Runtime` 구조체는 P5(S2-1)에서 도입**되므로 P4 시점에 "app이 소유"할 그릇이 아직 없다. `State.chrome`가 P4의
   현실적 단일 소유 그릇이고 render가 이미 `&state`를 threading하므로 침습이 작다. **P5가 per-frame 크롬 입력 조립을
   `Runtime`로 모으면** 그때 `Runtime`로 재배치 가능(발견 문구 충족) — P4↔P5 상호작용으로 명시.
9. **S1-8 = 관례 수용(문서화)**. dirty 신호 분리는 Low 대비 churn 과다 → 하지 않음.
10. **`ui/render.rs`는 만들지 않음**(스펙의 "optional"). `render`/`render_tree`/`render_terminal_view`/`render_modal_popup`/
    `render_menu`는 `self.rows`/`list_state`/`selected`/`tree_inner`/`popup_rect`에 본질적으로 결합 — 별 파일로 빼면 대량
    `&Switcher` 통과만 늘어 깨끗이 분리되지 않는다. `render_popup`/geometry는 B2로 modal.rs, `help_lines`도 modal.rs로 가므로
    render 표면은 이미 얇아진다. render는 switcher 자기 상태 위의 것으로 남긴다.

---

## P4 완료 기준

- `cargo test -p xmux` · `cargo clippy -- -D warnings` · `cargo fmt --check` 클린 — **593 tests 유지 또는 상회**(A2/A3/B/C/D의
  신규 유닛 테스트만큼 증가; 회귀 0). 이관된 테스트(map_color×3, menu_rect/wrap_text/popup 계열, apply_op_result 계열)는
  등가 이동이며 커버리지 순감 아님.
- 각 단계 red-first 후 green. **성공/happy 경로 바이트 동일**(런타임 출력 불변 — 순수 이동·소유 이동뿐).
- **구조 불변식**: `tree.rs`가 ratatui-무의존 유지(순수·터미널 없이 테스트 가능); `Switcher`가 god-object가 아니라 커서/선택
  상태 기계 + 입력 디스패치 + rebuild 오케스트레이션 + render; 모달 표면은 `ui/modal.rs` 소유; inventory mutation은 `State`
  단일 소유(op-result 포함); 크롬은 `State.chrome` 단일 소유(7 shim 소멸).
- **불변식 게이트(스펙 §6)**: 이 계층은 mux-특정 분기 0(트리는 도메인 intent만 emit); AS-IS 문서(옮긴 표면의 AGENTS/독스트링
  갱신 — 넓은 CONTEXT 스윕은 P6).
- **라이브 게이트(사람)**: **B·C·D 필수** — 실 터미널에서 (1) 트리(색/스타일/스피너/scanning·⚠·(empty)), (2) 모달(help/입력/
  kill)·메뉴(hover·release)·팝업 드래그, (3) new/kill/rename/split의 트리 반영, (4) 뷰 보더·hint bar·host-info·flash가 P4
  이전과 시각·동작 동일. **A는 자동-테스트-충분**(A 끝 트리 시각 스냅 1회 권장).

---

## P4↔P5 경계 (명시)

P4가 **하지 않고 P5로 미루는** 것(스펙 §5 P5):

- `apply_source_result`/`apply_panes`의 mutation 전면 이관 및 "모든 것이 `Action`/`Command`로 흐름" **수렴**(S2-2) — P4는
  op-result 경로만.
- `Runtime` 구조체 도입·913줄 god-function 분해·`app::input` 추출(S2-1·S2-6) — P4는 UI 파일만.
- `Selection`을 `model`로 이동(S2-5, HOT) — P4는 UI 구조만.
- 크롬 소유를 `State`→`Runtime`로 최종 재배치(발견 S1-4 "app이 소유") — P4는 `State.chrome`까지.

## 북극성 가산성 재확인 (P4 후)

P4는 UI 구조만 바꾸고 축(machine/mux) 확장 경로를 건드리지 않는다(불변식 "supervisor 무지"). 트리는 여전히 mux에 분기하지
않고 도메인 intent만 emit하므로, 새 mux/machine 패밀리 추가 시 `ui/` 수정은 **0**을 유지한다(스펙 §1 북극성). `tree::flatten`이
순수해지고 모달/크롬 소유가 단일화되어 오히려 UI의 가산성·테스트 가능성이 강화된다.
