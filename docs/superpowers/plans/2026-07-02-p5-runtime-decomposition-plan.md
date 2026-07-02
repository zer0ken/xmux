# P5 구현 계획 — `app/runtime.rs` 분해 + 도메인 수렴 (SRP · 응집도 · DIP · 테스트 가능성 · 명시성)

> **상태: 설계(미구현).** xmux 이상적 구조 리팩토링 · 2026-07-02 · 브랜치 `refactor/ideal-structure`
> (워크트리 `.claude/worktrees/refactor-ideal/`, 베이스 `cf5e574`, HEAD `32151de` = cf5e574 + P0 + P1 + P2 + P3 + P4).
> 마스터 스펙: `docs/superpowers/specs/2026-07-02-ideal-structure-refactor-design.html`
> (§1 북극성/목표 지도 · §5 P5 표 S2-1..S2-9 · §6 불변식 · §7 HOT 조율). 선행: P0·P1·P2·P3·P4 완료.
> **베이스라인: 610 tests / clippy0 / fmt0.**
> 경로는 repo-상대. 모든 `file:line` 앵커는 스펙 기준 **QUADRUPLE-stale**였다(0818679 감사 → cf5e574(osc11) 병합 →
> P0/P1 → P2 → P4가 `runtime.rs`를 대폭 재배선). 아래 앵커는 **이 워크트리의 실코드(HEAD `32151de`)로 재확인·재배치한
> 값**이며, 구현 중 심볼/패턴으로 다시 확인한다(**LSP는 신뢰 말 것**).
> TDD 규율: 각 단계 = 실패 테스트(red, 이유 명시) → 최소 구현 → green + 회귀 0. 성공/happy 경로는 **바이트 동일**.

---

## ⚠ 이 단계의 위험 프로필 (반드시 먼저 읽을 것)

- **P5는 로드맵에서 가장 크고, `runtime.rs`의 이벤트 루프 전체를 재구성한다.** 그 루프는 xmux의 심장이다: attach/switch,
  입력 라우팅(키/마우스/제스처), draw 핫 패스, 호스트 오케스트레이션, 디바운스 attach가 모두 여기서 돈다. 자동 테스트
  (harness `TestBackend` dump + 순수 유닛 + `handle_stdin_bytes`/`handle_mouse_event`/`dispatch_action`/`apply`/`apply_event`
  라운드트립)가 **구조 등가성**은 잡지만, **실제 터미널에서 attach가 안 비고, switch가 올바른 세션으로 in-place 전환되고,
  키/마우스가 올바른 뷰로 라우팅되고, 화면이 30fps로 coalesce되며 안 깜빡이는지**는 사람만 확인한다(스펙 §4 P5, §7).
  아래에서 각 단계를 **자동-테스트-충분** / **라이브-게이트-필요**로 표시한다.
- **가장 위험한 발견 = S2-2(도메인 수렴).** `displayed`/`attach_deadline`/`focus`가 `State::apply` 밖에서 명령형으로
  변경되는 경로를 `Action`으로 라우팅한다. 이는 프리즈-감도 attach 경로와 focus 중간-읽기(mid-read) 시맨틱을 건드리므로
  **P5에서 유일하게 행동을 바꿀 위험이 실재하는 단계**다. 그래서 **맨 마지막 자체 배치(D)로 격리**하고, **3개 독립 하위
  단계로 쪼개**며, **가장 무거운 검증(라이브 게이트 + 프리즈 재현)**을 붙인다. D3(focus 수렴)는 필요 시 **부분 축소/이관
  가능**(아래 설계 선택 §7).
- **HOT 조율.** S2-5(`Selection`→`model`)는 스펙 §7 HOT 파일 `driver.rs`·`mux/tmux/display.rs`·`mux/psmux/display.rs`를
  건드린다(이들이 `crate::app::runtime::Selection`을 import). **boxed-migration stash는 폐기 결정이므로 rebase/조율
  의존은 없다**(P2 계획의 결론과 동일); 단 세 파일의 import 라인을 함께 갱신하므로 **한 배치 안에서 원자적으로** 처리한다.
- **P4가 이미 바꾼 것과 겹치지 않게 한다**(아래 "이미 해소된 것" 참조). P4는 op-result fold를 `State`로, 크롬을
  `State.chrome`로 옮겼다. P5의 S2-2 수렴은 그 위에서 **runtime.rs의 명령형 도메인 쓰기**만 잡는다. switcher가 여전히
  `apply_source_result`/`apply_panes`/`set_active_window`로 `state.groups`/`panes`를 쓰는 잔여 수렴은 **P5 범위 밖**으로
  명시한다(설계 선택 §7).

---

## 이미 해소된 것 — P1–P4가 한 일 (재작업 금지)

스펙의 P5 표 앵커(`runtime.rs:1698-2611`, `560-566`, `1814-1836` 등)는 P1–P4 **이전** 상태다. 실코드는 크게 달라졌다.
발견별로 현재 형태와 P1–P4가 줄인 범위를 정리한다.

- **S2-1(`run_app` god-function 913줄 + 15–24-param 헬퍼).** 여전히 최대 발견이나 **파라미터 수/헬퍼 구성이 바뀌었다.**
  - `run_app`은 현재 **1739–2653(약 914줄)**. `#[allow(clippy::too_many_arguments)]`는 스펙의 "9×"에서 **현재 10×**:
    `dispatch_action`(116)·`dispatch_commands`(148)·`request_attach`(393)·`select_attach`(431)·`sync_source_terminals`(523)·
    `handle_tree_bytes`(843)·`handle_host_event`(946)·`run_event_effect`(999)·`handle_mouse_event`(1232)·`handle_stdin_bytes`(1444).
  - **P2가 host 오케스트레이션을 `Hosts`로 옮기면서** `ensure_current_host`(570)·`dispatch_detected_host`(609)·
    `scan_or_dispatch_host`(622)·`kick_rescan`(667)·`connect_all_sources`(701)는 이제 **`&Hosts`/`&mut HostManager` 기반**이며
    `env.srcs`/`by_alias` threading이 사라졌다 → 스펙이 말한 "HostOrchestrator 추출"의 **파라미터 압력이 줄었다**. 그래도
    이들은 여전히 `(mgr, hosts, detecting, cols, rows, tree_width)` 다발을 손으로 나른다 → **`Runtime` 메서드로 흡수 대상.**
  - **P4가 크롬을 `State.chrome`로 옮겨서** 루프-탑의 per-frame 크롬 입력 조립이 이제 `state.chrome.set_*`(1993–1998·2172·2547)
    직접 호출이다(옛 7개 switcher shim은 소멸). 스펙 S2-1이 말한 "per-frame 크롬 입력을 `Runtime`이 모은다"의 소재가
    `state.chrome`로 이미 단일화됐다 → `Runtime`은 이 세팅을 **한 메서드로 모으기만** 하면 된다(설계 선택 §5).
- **S2-2(`displayed`/`attach_deadline`/`focus`가 apply 밖 변경).** **여전히 미해소**(P4가 명시적으로 P5로 미룸). 실코드 재확인:
  - `state.displayed = …`: **3곳** — 2047(reattach-kick → default 클리어), 2109(in-place attach 확정), 2365(DisplayReady 확정).
  - `state.attach_deadline = …`: **4곳** — 2048(reattach-kick), 2249·2257(host-event 복구 rearm), 2327(pty detach 복구 rearm).
  - `state.focus.*`: **apply 밖 5곳** — 1285(menu release `set_view_focus`), 1415(mouse `focus.toggle()`),
    1680(focus_terminal)·1688(focus_tree)·1714(replay focus_terminal). (2003 `sync_modal`은 루프-탑 모달 차원 reconcile —
    도메인 쓰기가 아니라 P4가 넣은 정당한 조정이므로 건드리지 않는다.) 참고: `Action::Focus(FocusTarget)` 경로가 **이미
    존재**(apply, state/mod.rs:146–155)하므로 focus 수렴은 "새 개념 도입"이 아니라 "직접 쓰기 → 기존 Action 라우팅"이다.
- **S2-3(`HostDisplay` 필드 `pub` + Ready-arm 인라인).** `HostDisplay`(model/host.rs:43–57)는 이미 시맨틱 메서드
  일부(`shows`/`set_shows`/`mark_in_flight`/`clear`)를 갖지만 4개 필드(`current`/`in_flight`/`reaped_ids`/`pending`)가
  **여전히 `pub`**. Ready/Failed 결정은 여전히 루프 인라인(2332–2391)이고, pty-`Exited` reap도 인라인(2281–2321)이다.
  P2/P4가 줄이지 않았다 → 그대로 대상.
- **S2-4(grid 획득 중복).** draw 경로(2150–2169)와 dump 경로(2437–2456)가 `driver_for`+`DriverCtx`+`driver.grid`를 **바이트
  동일하게 2번** 짓는다. 미해소.
- **S2-5(`Selection`이 `app::runtime`).** `Selection`은 여전히 runtime.rs:244–281. 상향 import: `state/mod.rs:4`,
  `model/action.rs:19`, `driver.rs:120`(테스트), `model/host.rs`(테스트 315·322·329), `state/mod.rs`(테스트 453),
  `mux/tmux/display.rs:245`(테스트), `mux/psmux/display.rs:299`(테스트). **HOT 표면 재확인**: 드라이버(display.rs)는
  `crate::app::runtime::{run_switch_plan, display_key}`도 상향 호출(tmux/display.rs:87·176·181, psmux/display.rs:104·216·221) —
  단 **S2-5는 `Selection`만** 아래로 내린다(나머지 상향 호출은 P5 범위 밖, 설계 선택 §4).
- **S2-6(입력 라우팅 ~700줄).** 전부 runtime.rs에 있음: 순수(`resolve_tree_key` 800·`resolve_mouse_chain` 762·`ChainAction`
  741·`leading_ctrl_arrow` 208·`to_grid_local` 330·`wheel_targets_tree` 726·`tree_menu_may_open` 734·`is_focus_in` 717·
  `view_border_drag_width` 198) + 상태 다발(`MouseState` 1196·`StdinOutcome` 1213) + 스테이트풀 핸들러(`handle_mouse_event`
  1232·`handle_stdin_bytes` 1444·`handle_tree_bytes` 843). 미해소.
- **S2-7(fingerprint/slow-step 관측).** `grid_fingerprints`(1958)·fingerprint match 블록(2191–2214)·`log_slow_step`(233)이
  draw 핫 패스에 얽힘. 미해소.
- **S2-9(`psmux_session_live` → `session_is_live`).** 여전히 `psmux_session_live`(model/host.rs:217; 호출 runtime.rs:1095;
  테스트 host.rs:767·769·779). 동작은 이미 mux-중립(`death_signal` match). 순수 개명.

**요약:** P2는 S2-1의 host-오케스트레이션 파라미터 압력을 줄였고, P4는 S2-1의 크롬 소재를 `State.chrome`로 단일화했다.
그 외 S2-2..S2-9는 P1–P4가 **거의 건드리지 않았다** — P5의 범위는 실질적으로 스펙 그대로다.

---

## 목표 종료 형태 (스펙 §1 app 계층)

- `src/app/runtime.rs` = **`Runtime` 구조체 + 그 impl(루프 locals를 필드로 소유) + `run_app`(구조체를 만들고 `select!`을
  도는 얇은 진입점)**. 913줄 god-function 없음, `too_many_arguments` allow **0개**. `select!` arm 하나당 `Runtime` 메서드 하나.
- `src/app/input.rs` (**신규**) = **순수/무상태 입력 라우팅 코어**: `ChainAction`·`resolve_tree_key`·`resolve_mouse_chain`·
  `leading_ctrl_arrow`·`to_grid_local`·`view_border_drag_width` + 술어 + `MouseState`/`StdinOutcome` 타입 + 그 유닛 테스트.
  스테이트풀 핸들러(`handle_stdin_bytes`/`handle_mouse_event`/`handle_tree_bytes`)는 **`Runtime` 메서드**가 되어 이 순수 코어를
  호출한다(설계 선택 §3).
- `src/model` = `Selection`(순수 도메인 값)의 새 집. `model::Selection { source, session, window }` + `address`/`is_empty`.
  (`Selection::from_target`는 `TerminalViewTarget`(ui)에 의존하므로 **app에 잔류** — 설계 선택 §4.)
- `Runtime`의 협력자: **attach 코디네이터**(`HostDisplay`의 `resolve_ready` 시맨틱 메서드 + `current_grid` 헬퍼, S2-3/S2-4)와
  **`DrawObserver`**(fingerprint/slow-step, S2-7). host 오케스트레이션은 `Runtime` 메서드로 흡수(P2가 이미 `Hosts` 기반).
- **도메인 쓰기는 `State::apply`가 유일 소유**: `displayed`/`attach_deadline`/`focus`가 루프에서 명령형으로 바뀌지 않고
  `Action`을 통해 흐른다(S2-2). 단, 런타임 사실(registry/host)이 필요한 **결정**은 루프가 하고 그 결과를 데이터로 실어
  apply를 **동기 호출**한다(apply는 동기이므로 중간-읽기 시맨틱 보존 — 설계 선택 §6).

---

## 서브배치 분할 (checkpoint 가능 단위)

| 배치 | 내용 (발견) | 위험 | 게이트 |
|---|---|---|---|
| **A** (안전·기계적) | A1=S2-9(개명), A2=S2-4(`current_grid` dedup), A3=S2-7(`DrawObserver`), A4=S2-5(`Selection`→`model`, HOT) | 낮음 · 바이트 동일 | **자동-테스트-충분** (A4 후 트리+터미널 렌더 스냅 1회 권장) |
| **B** (app::input 순수 코어) | S2-6 순수 부분: 타입 + 순수 fn + 술어 + 그 테스트를 `app/input.rs`로 | 낮음 · 순수 이동 | **자동-테스트-충분** |
| **C** (Runtime 구조체) | C1=S2-1(`Runtime` 도입·헬퍼→메서드·arm당 메서드·입력 핸들러 메서드화·`too_many_arguments` 0), C2=S2-3(`HostDisplay::resolve_ready`+필드 비공개) | **높음** · 루프 재구성 | **라이브 게이트 필수** (전체 루프) |
| **D** (도메인 수렴 S2-2) | D1=`displayed`→Action, D2=`attach_deadline`→Action, D3=`focus`→Action | **가장 높음** · 도메인 쓰기 이동 | **라이브 게이트 필수 + 프리즈 재현** (각 하위단계 개별) |

**배치 간 독립성/순서**: A와 B는 서로 독립(병렬 가능). **C는 A·B 후**(A가 `current_grid`/`DrawObserver`/`Selection`을 정착시키고,
B가 순수 입력 코어를 분리해 두어야 C가 이를 깨끗이 메서드로 흡수). **D는 C 후**(도메인 쓰기가 `Runtime` 메서드로 모인 뒤라야
"apply 단일 소유"로의 라우팅이 국소적이고, D의 위험을 안정된 구조 위에서 격리할 수 있음). D는 P5의 마지막이자 가장 위험한 배치.

---

## 단계 순서 & 의존 그래프

```
A1 (psmux_session_live→session_is_live)  ─┐
A2 (current_grid dedup)                   ─┼─ 배치 A: 안전·기계적·바이트 동일 (독립·순서 무관)
A3 (DrawObserver)                         ─┤        │ A4 후: 트리+터미널 렌더 스냅 1회
A4 (Selection→model, HOT)                 ─┘        │
                                                    │
B  (app/input.rs 순수 코어)  ───────────────────────┤  배치 B: 순수 이동·자동-테스트-충분 (A와 독립)
                                                    ▼
C1 (Runtime 구조체 + arm당 메서드 + too_many_arguments 0) → C2 (HostDisplay::resolve_ready + 필드 비공개)   ← 배치 C: 라이브 게이트
                                                    ▼
D1 (displayed→Action) → D2 (attach_deadline→Action) → D3 (focus→Action)   ← 배치 D: 라이브 게이트 + 프리즈 재현, 각 개별
```

- **A2/A3는 C의 전제**: `current_grid`와 `DrawObserver`를 배치 A에서 자유 함수/독립 구조로 먼저 빼두면, C1에서 이들을
  `Runtime` 메서드/필드로 흡수하는 것이 기계적이 된다(중복 제거를 두 번 하지 않음).
- **B는 C의 전제**: 순수 입력 코어가 별 모듈에 있어야 C1에서 스테이트풀 핸들러를 `Runtime` 메서드로 만들며 순수 코어를
  **호출**하는 형태가 깔끔하다.
- **D는 C 후**: `displayed`/`attach_deadline`/`focus` 쓰기 사이트가 `Runtime` 메서드로 국소화된 뒤라야 apply 라우팅이 안전.

---

## 배치 A — 안전·기계적 (바이트 동일 · 자동-테스트-충분)

### 단계 A1 — S2-9: `psmux_session_live` → `session_is_live` 개명 `[Low]`

**(a) 목표**
`model::Host::psmux_session_live`(host.rs:217)는 동작이 이미 mux-중립이다(`death_signal` match — psmux만 `.port` stat,
그 외 항상 live). 이름이 구체 mux(psmux)를 노출해 "supervisor 무지" 불변식(스펙 §6)을 흐린다. `session_is_live`로 개명.

**(b) 실패 테스트 먼저**
기존 테스트 3개(`psmux_host_session_liveness_uses_the_port_stat` 757, `tmux_host_session_is_always_live_by_port_stat` 772)의
호출을 새 이름으로 바꾼다: `assert!(h.session_is_live(&name));` / `assert!(!h.session_is_live(&name));` / `assert!(h.session_is_live("anything"));`
- 오늘 red인 이유: `session_is_live` 메서드가 없어 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색)
- `Host::psmux_session_live`(host.rs:217) → `pub fn session_is_live`. 본문/독스트링 불변(mux-중립 서술로 독스트링 1줄 정리:
  "psmux는 `.port` stat, 그 외는 항상 live" — AS-IS).
- 유일 프로덕션 호출부 `runtime.rs:1095` `h.psmux_session_live(&s.name)` → `h.session_is_live(&s.name)`.
- 테스트 3곳 호출 갱신(위).

**(d) 검증**
`cargo test -p xmux` red→green. `grep -rn "psmux_session_live" src/` → 0. clippy/fmt 클린. **자동-테스트-충분**(순수 개명).

**(e) 파일**: `src/model/host.rs`, `src/app/runtime.rs`

---

### 단계 A2 — S2-4: grid 획득 중복 제거 → `current_grid` 헬퍼 `[Med · DRY]`

**(a) 목표**
draw 경로(runtime.rs:2150–2169)와 dump 경로(2437–2456)가 **동일한** `hosts.get(&displayed.source).map(driver_for)` +
`DriverCtx{…}` 조립 + `driver.grid(&displayed, &ctx)`를 반복한다. 하나의 헬퍼 `current_grid`로 통합(DRY). 배치 A에서는
아직 `Runtime`이 없으므로 **자유 함수**로 추출(C1에서 `Runtime` 메서드로 흡수 — 그때 다인자가 `&mut self`로 접힌다).

**(b) 실패 테스트 먼저**
`current_grid`는 실 PTY/registry 상태 없이 의미 있는 값을 내기 어렵다(빈 selection → `None`). 순수-테스트 가능한 최소 계약:
`current_grid_returns_none_for_empty_displayed`: 빈 `displayed`(source 없음)에서 `hosts.get` 미스 → `None`.
- 오늘 red인 이유: `current_grid` 함수가 없어 **컴파일 실패**.
- *(주: draw/dump의 실제 grid 반환은 기존 통합 테스트 `dump_screen_renders_the_live_grid`(run.rs) + 라이브가 커버.)*

**(c) 최소 구현** (심볼로 재탐색)
- 자유 함수 추가(다인자 → 임시 `#[allow(clippy::too_many_arguments)]` — C1이 제거):
  ```
  fn current_grid(
      displayed: &Selection, registry: &mut AttachRegistry, hosts: &mut Hosts,
      worker: &DisplayWorker, mgr: &HostManager,
      pty_tx: &UnboundedSender<PtyEvent>, attach_seq: &mut u64,
      cols: u16, body_rows: u16, tree_width: u16,
  ) -> Option<Arc<Mutex<Grid>>>
  ```
  본문 = draw 경로 2150–2169 이식(`driver_for` + `DriverCtx` 조립 + `driver.grid`).
- draw 블록(2150–2169)과 dump 블록(2437–2456)을 `let grid_arc = current_grid(&state.displayed, &mut registry, &mut hosts, &worker, &mgr, &driver_pty_tx, &mut attach_seq, cols, body_rows, tree_width);`로 교체.

**(d) 검증**
`cargo test -p xmux` red→green. `current_grid` 테스트 green; `dump_screen_renders_the_live_grid`·`dump_switcher_flattens_buffer`
(run.rs) green(동작 불변). `grep -c "DriverCtx {" src/app/runtime.rs` = 2 → **1**(draw/dump 중복 소멸; select_attach·sync_source_terminals·RawBytes 팔은 별개). clippy/fmt 클린. **자동-테스트-충분**(happy 경로 바이트 동일).

**(e) 파일**: `src/app/runtime.rs`

---

### 단계 A3 — S2-7: fingerprint/slow-step 관측을 `DrawObserver`로 `[Med · SoC/SRP]`

**(a) 목표**
draw 핫 패스가 관측(fingerprint 비교 → `display_grid_changed` 로깅, INFO/TRACE 분기 2191–2214)과 slow-step 프로브
(`log_slow_step` 233; grid_lock/render/draw/select_attach/host_drain 등에서 호출)에 얽혀 있다. 이들을 `DrawObserver`로 분리해
draw 블록은 **lock → render**만 하게 한다(SoC). `grid_fingerprints: HashMap<String,(u64,String)>`(1958)는 `DrawObserver`가 소유.

**(b) 실패 테스트 먼저** (`DrawObserver`의 `mod tests`, 순수)
- `draw_observer_reports_change_only_on_new_fingerprint`: 같은 (addr, fp)로 두 번 `observe(addr, session, fp)` →
  첫 호출만 "changed"(예: `Some(Transition::First)` 또는 bool), 둘째는 "unchanged". 같은 addr·같은 session·다른 fp →
  "steady"(TRACE 등급), 다른 session → "switched"(INFO 등급). 반환을 로그-등급 결정에 쓰도록 순수 분류.
- 오늘 red인 이유: `DrawObserver`/`observe`가 없어 **컴파일 실패**.

**(c) 최소 구현** (설계 선택 §2 = runtime.rs 내 소형 구조; 로깅은 호출부가 등급 결정)
- `struct DrawObserver { fingerprints: HashMap<String,(u64,String)> }` + `fn observe(&mut self, addr: &str, session: &str, fp: u64) -> FpOutcome`
  (`FpOutcome::{Unchanged, Steady, Switched}` — 2196/2199/2206 세 분기의 순수 버전). draw 블록의 fingerprint match(2191–2214)를
  `match observer.observe(&addr, session, grid.fingerprint()) { Steady => trace!…, Switched => info!…, Unchanged => {} }`로.
- `log_slow_step`(233)은 `DrawObserver::slow_step(&self, label, start)` 연관 함수로 이동(상태 불필요 → `fn`이어도 무방;
  응집 위해 같은 모듈/impl에 둠). 호출부(draw 내부 grid_lock/render/draw + select_attach 2112 + host_drain 2265)는 경로만 갱신.
- draw 블록에서 `grid_fingerprints` 로컬을 `DrawObserver` 필드로 이동; 루프 셋업(1958)은 `let mut draw_observer = DrawObserver::default();`.

**(d) 검증**
`cargo test -p xmux` red→green. `DrawObserver` 순수 테스트 green. 로그 문구/등급은 관측용이라 자동 테스트가 문자열을 단언하지
않음 — **행동 불변**(그리드/렌더 경로 바이트 동일). clippy/fmt 클린. **자동-테스트-충분.**

**(e) 파일**: `src/app/runtime.rs`

---

### 단계 A4 — S2-5: `Selection`을 `app::runtime` → `model`로 (HOT · DIP/레이어링) `[Med]`

> **HOT 원자 단계.** `driver.rs` + `mux/tmux/display.rs` + `mux/psmux/display.rs`의 import를 함께 갱신한다. stash 폐기로
> rebase 조율은 없지만, 한 커밋 안에서 세 파일을 모두 고쳐 중간 상태가 안 깨지게 한다.

**(a) 목표**
도메인 값 `Selection`(runtime.rs:244–281)이 오케스트레이션 계층(`app`)에 살아, 하위 계층 `model`/`state`/`driver`가
**상향 의존**한다(불변식 §6 "도메인은 오케스트레이션에서 import하지 않는다"를 위반). `Selection`(+ `address`/`is_empty`)을
`model`로 내린다. `from_target`은 `ui::switcher::TerminalViewTarget`에 의존하므로 **app 잔류**(설계 선택 §4).

**(b) 실패 테스트 먼저**
- `model::selection_addresses_source_slash_session`(model 내): `assert_eq!(model::Selection{source:"jup".into(),session:"api".into(),window:None}.address(), "jup/api");` + `assert!(model::Selection::default().is_empty());`
- 오늘 red인 이유: `model::Selection`이 없어 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색)
- `Selection` 구조체 + `impl { address, is_empty }` + derive를 `src/model/`(신규 `model/selection.rs` 또는 `model/host.rs`
  인접; 설계 선택 §4는 `model/selection.rs` 권장)으로 이동. `pub use selection::Selection;`를 `model/mod.rs`에 추가.
- **`from_target`는 이동 금지** — runtime.rs에 자유 함수 `fn selection_from_target(t: &TerminalViewTarget) -> Selection`로
  잔류(현 `Selection::from_target` 본문 이식; 호출부 `sync_selection_from_switcher` 299를 이 자유 함수로).
- import 갱신(상향 → `crate::model::Selection`): `state/mod.rs:4`, `model/action.rs:19`, `driver.rs:17`(및 테스트 120),
  `model/host.rs`(테스트 315·322·329·453 상당), `mux/tmux/display.rs:245`(테스트), `mux/psmux/display.rs:299`(테스트),
  `state/mod.rs`(테스트 453), runtime.rs 내부 참조. **`run_switch_plan`/`display_key`/`run_lowered`는 app 잔류**(S2-5 범위 밖;
  드라이버가 `crate::app::runtime::…`로 상향 호출하는 것은 별개 발견, 설계 선택 §4).
- `driver.rs:17` `use crate::app::runtime::{run_lowered, Selection};` → `use crate::app::runtime::run_lowered; use crate::model::Selection;`.

**(d) 검증**
`cargo test -p xmux` red→green. `model::Selection` 테스트 green; `selection_from_*_target`(runtime 2957–2989)·
`target_round_trips_through_selection`(driver 132)·`seam_show_replaces_the_psmux_display_attachment`(driver 179) green(경로만 이동).
`grep -rn "app::runtime::Selection" src/` → 0. clippy/fmt 클린.
- **자동-테스트-충분**이나, HOT 세 파일(display 드라이버)을 건드렸으므로 **A 종료 시 트리+터미널 렌더 스냅 1회**(attach/switch가
  살아있는지)에 포함 — 코드 경로는 불변이라 시각 회귀 위험은 낮지만 HOT 접촉을 눈으로 확인.

**(e) 파일**: `src/model/selection.rs`(신규), `src/model/mod.rs`, `src/app/runtime.rs`, `src/state/mod.rs`, `src/model/action.rs`,
`src/model/host.rs`, `src/driver.rs`, `src/mux/tmux/display.rs`, `src/mux/psmux/display.rs`

---

## 배치 B — `app/input.rs` 순수 코어 (S2-6 순수 부분 · 자동-테스트-충분)

### 단계 B1 — 순수 입력 라우팅 코어를 `app/input.rs`로

**(a) 목표**
입력 라우팅 ~700줄 중 **순수/무상태** 부분을 새 `app/input.rs`로 모아 응집도/테스트 가능성을 높인다. 스테이트풀 핸들러
(`handle_stdin_bytes`/`handle_mouse_event`/`handle_tree_bytes`)는 `Runtime`의 세계 상태를 통째로 변경하므로 **C1에서 `Runtime`
메서드**가 되며, 이 순수 코어를 호출한다(설계 선택 §3 — 이동을 두 번 하지 않기 위한 분할선).

**(b) 실패 테스트 먼저** (`app/input.rs`의 `mod tests` — 기존 순수 테스트 이관)
runtime.rs의 순수 입력 테스트를 새 모듈로 이관하고 경로를 `crate::app::input::…`로:
`resolve_tree_prefix_commands`(2709)·`resolve_tree_enter_focuses_mux_and_nav_is_a_tree_key`(2757)·
`resolve_tree_while_inputting_passes_prefix_and_enter_to_the_tree`(2775)·`resolve_tree_arming_persists_across_reads`(2903)·
`wheel_targets_tree_only_when_tree_focused_and_over_tree`(2799)·`resolve_mouse_chain_routes_by_focus_and_position`(2819)·
`tree_menu_opens_only_in_tree_focus_over_the_tree`(2883)·`leading_ctrl_arrow_peels_one_and_ignores_others`(3154)·
`view_border_drag_width_clamps_to_range`(3207)·`to_grid_local_*`(3265–3310).
- 오늘 red인 이유: 이관 후 `crate::app::input::{resolve_tree_key, resolve_mouse_chain, …}` 경로가 없어 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색)
- `src/app/mod.rs`에 `pub mod input;` 추가.
- `app/input.rs`로 이동: `ChainAction`(741)·`resolve_mouse_chain`(762)·`resolve_tree_key`(800)·`leading_ctrl_arrow`(208)·
  `to_grid_local`(330)·`wheel_targets_tree`(726)·`tree_menu_may_open`(734)·`is_focus_in`(717)·`view_border_drag_width`(198) +
  타입 `MouseState`(1196)·`StdinOutcome`(1213). 가시성 `pub(crate)`. `Action`(display::dispatch) import를 input.rs로.
- **잔류(C1에서 `Runtime` 메서드화)**: `handle_stdin_bytes`·`handle_mouse_event`·`handle_tree_bytes`는 runtime.rs에 두되,
  위 순수 심볼을 `crate::app::input::…`로 호출하게 갱신. **너비 헬퍼**(`adjust_tree_width` 73·`apply_width_delta` 81·
  `reconciled_tree_width` 223·`terminal_view_size` 315·`toggle_auto_hide` 96)는 입력과 draw 양쪽이 쓰므로 **runtime.rs 잔류**
  (설계 선택 §3; 옮기면 상호 참조만 늘어남). 이들의 테스트(`apply_width_delta_is_write_free…` 3188·`reconciled_tree_width_…`
  3143·`terminal_view_size_*` 3133·3242·3256·`tree_width_adjust_clamps` 3234)는 runtime.rs 잔류.

**(d) 검증**
`cargo test -p xmux` red→green. 이관된 순수 입력 테스트 전부 green(로직 불변, 경로만 이동). `grep -nE "fn resolve_tree_key|fn resolve_mouse_chain|enum ChainAction|struct MouseState" src/app/runtime.rs` → 0(이동 완결). clippy/fmt 클린. **자동-테스트-충분**(순수 이동).

**(e) 파일**: `src/app/mod.rs`, `src/app/input.rs`(신규), `src/app/runtime.rs`

---

## 배치 C — `Runtime` 구조체 (S2-1 + S2-3) — 라이브 게이트 필수

> **P5의 키스톤.** `run_app`의 루프 locals를 `Runtime` 구조체 필드로 소유시키고, 느슨한-파라미터 헬퍼를 메서드로,
> `select!` arm 본문을 arm당 메서드로 바꾼다. **성공 경로 바이트 동일**이 목표 — 순수 로직 추가 없음, 소유/구조 이동뿐.
> 구조 이동이라 red-first는 대체로 컴파일-게이트(메서드 존재) + 회귀 스위트 그린 + 라이브로 검증한다.

### 단계 C1 — `Runtime` 구조체 도입 + arm당 메서드 + `too_many_arguments` 0

**(a) 목표**
913줄 god-function과 15–24-param 헬퍼를 제거한다. `Runtime`이 루프 locals(세계 상태)를 필드로 소유하고, 헬퍼는 `&mut self`
메서드가 되어 파라미터가 접힌다. `select!` arm 하나당 메서드 하나. `#[allow(clippy::too_many_arguments)]` **10개 전부 제거.**

**(b) `Runtime` 구조체 형태** (설계 선택 §1 = "세계 상태는 구조체, `select!` 소스는 루프-로컬")
루프의 tokio `select!`이 채널 수신기를 future로 빌리는 동안 arm 본문이 `&mut self`를 다시 빌리는 **차용 충돌**이 P5 최대
기술 리스크다(설계 선택 §6). 확정 형태:

- **`Runtime`이 소유(필드)**: `registry: AttachRegistry`, `hosts: Hosts`, `mgr: HostManager`, `worker: DisplayWorker`,
  `switcher: Switcher`, `state: State`, `attach_seq: u64`, `cols`/`body_rows`/`tree_width`/`tree_width_natural`/`auto_hide_tree`,
  `mouse_state: MouseState`, `term_input: TermInput`, `tree_decoder: KeyDecoder`, `prefix: u8`, `connected`/`panes_requested`/
  `detecting: HashSet<String>`, `draw_observer: DrawObserver`(A3), `term: Terminal<…>`, `driver_pty_tx: UnboundedSender<PtyEvent>`,
  `op_tx: UnboundedSender<OpResult>`, `ops: Arc<dyn Ops>`, `env: Arc<Env>`, `dirty: bool`, `last_draw`/`width_dirty`/`width_flush_at`.
- **`run_app`(진입점)이 소유(루프-로컬, `Runtime` 밖)**: `select!`이 폴링하는 **수신기/타이머** — `host_rx`, `pty_rx`,
  `worker`의 수신 반쪽, `stdin_rx`, `cmd_rx`, `op_rx`, `tick`/`reconnect`/`frame`. 이유: `self.host_rx.recv()`를 폴링하며
  arm 본문에서 `self.handle_host_event(...)`(=`&mut self`)를 부르면 충돌. **패턴**: `select!`은 각 arm에서 이벤트 값만 뽑아
  `Ev` enum으로 넘기고, `select!` 블록이 끝나 recv-future 차용이 풀린 뒤 `rt.dispatch(ev)`(=`&mut self`) 한 번을 부른다.
  버스트 드레인(현 `host_rx.try_recv`/`pty_rx.try_recv`)은 **수신기를 dispatch 메서드에 `&mut` 파라미터로 전달**해 처리(예:
  `fn on_host_event(&mut self, ev: HostEvent, host_rx: &mut UnboundedReceiver<HostEvent>)`) — 이때는 outstanding recv-future가
  없으므로 `&mut self` + `&mut host_rx` 동시 차용이 성립. `worker`는 필드지만 그 수신 반쪽만 루프-로컬로 분리(아래).
  - **`DisplayWorker` 수신 분리**: 현재 `worker.recv()`(runtime.rs:2332 arm)가 `&mut self.worker`를 요구 → arm 본문
    `&mut self` 충돌. `DisplayWorker`의 수신 반쪽(내부 `UnboundedReceiver<DisplayEvent>`)을 `worker.take_events()` 같은
    메서드로 **한 번 꺼내 루프-로컬**로 두고, 송신 능력(`ensure`)만 `Runtime.worker`에 남긴다(설계 선택 §6-a; 이 분리가
    불가하면 `Runtime`이 `worker`를 갖지 않고 `run_app`이 `worker`+수신기를 통째 루프-로컬로 두고 draw/attach 메서드에
    `&DisplayWorker`를 파라미터로 넘긴다 — 대안 §6-b).
- **arm당 메서드**(10개): `on_host_event`(host_rx arm 2246–2266, 드레인 포함)·`on_pty_event`(pty_rx arm 2267–2331)·
  `on_display_event`(worker arm 2332–2391)·`on_stdin`(stdin_rx arm 2392–2412 → `handle_stdin_bytes` 흡수)·
  `on_ctl_command`(cmd_rx arm 2413–2509)·`on_op_result`(op_rx arm 2510–2512)·`on_tick`(tick arm 2513–2548)·
  `on_reconnect`(reconnect arm 2549–2630)·`on_frame`(frame arm 2631–2636). 루프-탑(1990–2235: 크롬 조립·width reconcile·
  reattach-kick·selection sync·Tick·draw)은 `prepare_and_draw`(또는 `tick_top`) 메서드로.
- **헬퍼 → 메서드**: `ensure_current_host`·`connect_all_sources`·`kick_rescan`·`scan_or_dispatch_host`·`dispatch_detected_host`·
  `request_session_panes`·`refetch_host`·`select_attach`·`sync_source_terminals`·`selection_attach_facts`·`handle_host_event`·
  `run_event_effect`·`handle_mouse_event`·`handle_tree_bytes`·`dispatch_action`·`dispatch_commands`·`spawn_op`·`current_grid`(A2)를
  `impl Runtime`으로. `cols`/`body_rows`/`tree_width`/`mgr`/`hosts`/`registry`/`worker`/… 파라미터가 `self.*`로 접힌다.
  **순수 자유 함수는 잔류**(`app::input`의 순수 코어, `terminal_view_size`, `spinner_frame_at`, `status_line`, `host_of_key`,
  `attach_reply_is_current`, `run_lowered`/`run_switch_plan` 등 — 세계 상태 불필요).

**(c) 실패 테스트 먼저**
구조 이동이라 순수 red-first가 제한적. 컴파일-게이트 + 소형 구성 테스트:
- `runtime_struct_constructs_from_env`(runtime tests): fake `Env`(기존 `fake_env_builder_constructs` 3646 패턴)로
  `Runtime::new(env)` 호출 → 필드 기본값 확인(예: `rt.dirty == true`, `rt.state.selection.is_empty()`). 오늘 red: `Runtime`/
  `Runtime::new`가 없어 컴파일 실패. *(`select!` 루프 자체는 터미널 소유·async라 유닛 테스트 불가 → 라이브 게이트.)*
- 기존 스테이트풀 테스트(`handle_stdin_bytes_quit_on_prefix_q_in_tree_focus` 4507·`kill_confirm_owns_keys…` 4557·
  `menu_keyboard_input_is_consumed…` 4683·`handle_mouse_event_view_border_grab_sets_dragging` 4809·`dispatch_action_switch_…`
  4304·`ctl_switch_syncs_canonical_selection_immediately` 4442)는 **메서드 시그니처로 재배선**되며 그린 유지 = 등가성 게이트.

**(d) 최소 구현** (심볼로 재탐색; 큰 이동이므로 하위 커밋 권장)
1. `Runtime` 구조체 + `Runtime::new(env) -> Runtime`(현 `run_app` 셋업 1755–1943 이식: term_guard/panic hook은 `run_app` 잔류,
   구조체 필드 초기화만 `new`로). `run_app`은 `let mut rt = Runtime::new(env); … rt.run().await`로 축소.
2. 헬퍼를 하나씩 `impl Runtime` 메서드로(파라미터 → `self.*`), 호출부 갱신. `too_many_arguments` allow를 메서드화 완료마다 제거.
3. `select!` arm 본문을 arm당 메서드로. `run_app`(또는 `Runtime::run`)의 `select!`은 위 (b) 패턴으로 재구성.
4. 스테이트풀 입력 핸들러(`handle_stdin_bytes`/`handle_mouse_event`/`handle_tree_bytes`)를 `Runtime` 메서드로(B의 순수 코어 호출).
5. `dispatch_action`/`dispatch_commands`도 메서드로(그러나 `spawn_op`는 `ops`/`op_tx`만 쓰므로 자유 함수 유지 가능).

**(e) 검증**
`cargo test -p xmux` red→green. **회귀 게이트(그린 유지)**: 위 스테이트풀 테스트 전부 + `apply`/`apply_event`/`fold_op_result`
스위트 + `dump_*`(run.rs). `grep -c "too_many_arguments" src/app/runtime.rs` → **0**. `cargo clippy -- -D warnings`가
`too_many_arguments` 경고 0으로 통과. fmt 클린.
- **⚠ 라이브 게이트(사람) 필수**: 실 터미널에서 전체 루프 — (1) attach가 안 비고 세션 뜸, (2) tree↔terminal 포커스 토글,
  (3) 세션 switch가 in-place 전환, (4) 키/마우스/휠/우클릭 메뉴/팝업 드래그/뷰 보더 드래그, (5) 콘솔 resize, (6) `r` 재스캔,
  (7) 30fps coalesce(빠른 네비 시 안 프리즈)가 P5 이전과 **동일**. jupiter06(throwaway) 먼저.

**(f) 파일**: `src/app/runtime.rs`(+ `src/display/worker.rs`가 수신 분리를 위해 `take_events`류를 노출하면 여기도)

---

### 단계 C2 — S2-3: `HostDisplay::resolve_ready` + 필드 비공개 `[Med · 데메테르/캡슐화]`

**(a) 목표**
`HostDisplay`의 4개 필드(`current`/`in_flight`/`reaped_ids`/`pending`)가 `pub`이고, worker `Ready`/`Failed` arm(2332–2391)과
pty `Exited` reap(2281–2321)이 이 필드를 루프에서 직접 조작한다(reap/insert/stale-seq 결정 인라인). `HostDisplay`(+ 필요 시
`AttachRegistry`)에 **시맨틱 메서드**를 주고 필드를 비공개화한다(데메테르/캡슐화/테스트 가능성).

**(b) 실패 테스트 먼저** (`model/host.rs`의 tests — 순수, registry 무관 부분)
- `host_display_resolve_ready_reaped_race_tears_down`: `reaped_ids`에 id 존재 → `resolve_ready(key, seq, id)`가
  `ReadyOutcome::TearDownReaped`(in_flight/pending 정리) 반환.
- `host_display_resolve_ready_current_seq_installs`: in_flight[key]==seq → `ReadyOutcome::Install{prior_session}`(in_flight/pending
  제거, `shows(key)` 반환) 반환.
- `host_display_resolve_ready_stale_seq_tears_down`: in_flight[key]!=seq → `ReadyOutcome::TearDownStale`(pending만 제거) 반환.
- `host_display_resolve_failed_clears_when_current`: `resolve_failed(key, seq)`가 current면 in_flight/pending 정리 후 true.
- 오늘 red인 이유: `HostDisplay::resolve_ready`/`resolve_failed`/`ReadyOutcome`이 없어 **컴파일 실패**.

**(c) 최소 구현** (설계 선택 §8 = `HostDisplay`가 결정, registry 조작은 호출부)
- `HostDisplay`에 `resolve_ready(&mut self, key, seq, id) -> ReadyOutcome`(현 2339–2375 결정 로직 이식: reaped_ids race →
  stale seq → current seq 3분기; registry.remove/insert·attachment.teardown은 **호출부가** 반환 outcome을 보고 수행) +
  `resolve_failed(&mut self, key, seq) -> bool`(2383–2386) + pre-Ready Exited용 `mark_reaped_if_pending(&mut self, id) -> bool`
  (2290·2308의 `pending.contains_key` → `reaped_ids.insert` 로직). `ReadyOutcome` enum 반환.
- `Runtime::on_display_event`(C1)은 `match h.display.resolve_ready(&key, seq, id) { Install{prior} => { registry.remove; registry.insert; state.displayed = …(D1 전까지 직접) } TearDownReaped|TearDownStale => attachment.teardown() }`.
- `HostDisplay` 4개 필드를 `pub` → `pub(crate)` 또는 `private`(접근을 시맨틱 메서드로만). `driver.rs`/display 드라이버가
  `h.display.in_flight` 등을 직접 읽는지 `grep`으로 확인 후 접근자 추가(예: `in_flight_contains(key)` — 이미 runtime.rs:384·
  2107·2541이 `h.display.in_flight.contains_key`를 읽으므로 `fn in_flight_contains(&self, key) -> bool` 추가).

**(d) 검증**
`cargo test -p xmux` red→green. 신규 `HostDisplay` 시맨틱 테스트 green; 기존 `host_display_*`(host.rs 368–426)·
`attach_reply_is_current_only_for_latest_seq`(runtime 3855)·`seam_show_replaces_the_psmux_display_attachment`(driver 179) green.
`grep -n "\.display\.\(in_flight\|reaped_ids\|pending\|current\)\b" src/` → 시맨틱 메서드 경유만(직접 필드 접근 0). clippy/fmt 클린.
- **⚠ 라이브 게이트(사람)**: attach의 미묘한 경합 — (1) 빠른 세션 전환 중 reattach가 죽은 pane 안 남김(reaped-race), (2)
  detach 후 복구 재-attach, (3) stale Ready가 화면 안 흔듦. jupiter06 + 로컬 psmux.

**(e) 파일**: `src/model/host.rs`, `src/app/runtime.rs`

---

## 배치 D — 도메인 수렴 (S2-2) — 라이브 게이트 필수 + 프리즈 재현 · **가장 위험 · 맨 마지막**

> **P5에서 유일하게 행동을 바꿀 위험이 실재하는 배치.** `displayed`/`attach_deadline`/`focus`의 apply-밖 명령형 쓰기를
> `Action`으로 라우팅한다. **핵심 원칙(설계 선택 §6)**: `State::apply`는 **동기**다. "Action으로 라우팅"은 *지연*이 아니라
> *같은 사이트에서 `state.apply(Action::…)`를 동기 호출*하는 것 — 그래서 중간-읽기(mid-read) 시맨틱이 보존된다. 런타임
> 사실(registry.contains, in_flight)이 필요한 **결정**은 루프가 계속 하고, 그 결과를 `Action`의 **데이터**로 실어 apply에
> 넘긴다(Tick가 `key_live`/`in_flight`를 데이터로 받는 기존 패턴과 동형). **3개 독립 하위 단계**로 쪼개 각각 개별
> 커밋·검증하고, D3(focus)는 필요 시 축소한다(설계 선택 §7).

### 단계 D1 — `displayed`를 `Action`으로 (`State::apply` 단일 소유)

**(a) 목표**
`state.displayed`의 apply-밖 쓰기 3곳(2047 클리어, 2109 in-place 확정, 2365 DisplayReady 확정)을 `Action`을 통하게 한다.
결정(registry.contains && !reattach_pending 등)은 루프가 유지, mutation만 apply가 소유.

**(b) 실패 테스트 먼저** (`state/mod.rs` tests)
- `apply_confirm_display_sets_displayed`: `s.apply(Action::ConfirmDisplay(sel("api")))` → `s.displayed == sel("api")`, 반환 커맨드 없음.
- `apply_clear_display_empties_displayed`: 비어있지 않은 displayed에서 `s.apply(Action::ClearDisplay)` → `s.displayed.is_empty()`.
- 오늘 red인 이유: `Action::ConfirmDisplay`/`ClearDisplay`가 없어 **컴파일 실패**.

**(c) 최소 구현** (설계 선택 §6)
- `model/action.rs`: `Action::ConfirmDisplay(Selection)` + `Action::ClearDisplay` 추가. `state/mod.rs::apply`에 두 arm
  (`self.displayed = sel; Vec::new()` / `self.displayed = Selection::default(); Vec::new()`).
- runtime.rs 사이트 교체(모두 결정은 루프, 적용은 apply 동기 호출):
  - 2047 `state.displayed = Selection::default()` → `state.apply(Action::ClearDisplay);`
  - 2109 `state.displayed = sel.clone()` → `state.apply(Action::ConfirmDisplay(sel.clone()));`(가드 `registry.contains(&k) && !reattach_pending`는 루프에 유지)
  - 2365 DisplayReady `state.displayed = Selection{…}` → 같은 값 `Action::ConfirmDisplay(…)`.

**(d) 검증**
`cargo test -p xmux` red→green. 신규 apply 테스트 green; **stale-while-revalidate 회귀 게이트**: `should_attach_fires_on_change_and_recovery_never_storms_in_flight`(4083)·`apply_tick_does_not_fire_when_already_displayed_and_live`(state 579)·
`ctl_switch_syncs_canonical_selection_immediately`(4442) green. clippy/fmt.
- **⚠ 라이브 게이트**: switch 시 이전 세션이 화면에 유지되다 새 세션 준비되면 in-place 교체(stale-while-revalidate), `r`
  재-attach 시 블랭크→재표시. jupiter06 + 로컬.

**(e) 파일**: `src/model/action.rs`, `src/state/mod.rs`, `src/app/runtime.rs`

---

### 단계 D2 — `attach_deadline` 복구 rearm을 `Action`으로

**(a) 목표**
`state.attach_deadline`의 apply-밖 복구 rearm 4곳(2048 reattach-kick, 2249·2257 host-event, 2327 pty-detach)을 `Action`으로.
`apply`는 이미 `Tick`에서 deadline을 arm하므로, 복구 경로도 같은 소유자를 거치게 한다(프리즈 디바운스 불변식 보존).

**(b) 실패 테스트 먼저** (`state/mod.rs` tests)
- `apply_rearm_attach_arms_deadline`: `s.apply(Action::RearmAttach { now: t0 })` → `s.attach_deadline == Some(t0 + DEBOUNCE)`.
- 오늘 red인 이유: `Action::RearmAttach`가 없어 **컴파일 실패**.
- *(회귀: 기존 디바운스 테스트 5개가 그린 유지 = arm/fire 분리 보존.)*

**(c) 최소 구현** (설계 선택 §6 — clock은 데이터로 주입, Tick 패턴 동형)
- `model/action.rs`: `Action::RearmAttach { now: Instant }`(clock 주입, apply는 `Instant::now()`를 읽지 않는 불변식 유지).
  `apply` arm: `self.attach_deadline = Some(now + Duration::from_millis(ATTACH_DEBOUNCE_MS)); Vec::new()`.
- runtime.rs 4곳 `state.attach_deadline = Some(Instant::now() + …ATTACH_DEBOUNCE…)` → `state.apply(Action::RearmAttach { now: Instant::now() });`.
  2048(reattach-kick)은 `Some(Instant::now())`(즉시) — 별 arm 필요: `Action::RearmAttach`에 `now` 그대로 넘기되 즉시성이
  필요하면 `RearmAttachAt(Instant)` 하나로 통합하고 2048은 `now`를 그대로(=즉시 만료) 넘긴다(설계 선택 §7-b에서 확정).

**(d) 검증**
`cargo test -p xmux` red→green. `apply_tick_*`/`apply_select_between_ticks_rearms…`(state 494–722) 전부 green(디바운스 불변).
clippy/fmt.
- **⚠ 라이브 게이트 + 프리즈 재현**: 빠른 상하/좌우 네비 시 프리즈 없음(디바운스 유효), host detach/pty detach 후 복구 재-attach,
  `r` 재-attach. **프리즈는 자동 재현이 불안정하므로 사람이 실 터미널에서 빠른 네비를 실행**해 확인.

**(e) 파일**: `src/model/action.rs`, `src/state/mod.rs`, `src/app/runtime.rs`

---

### 단계 D3 — `focus` 토글을 `Action`으로 `[가장 민감 · 필요 시 축소]`

**(a) 목표**
키/마우스 focus 변경 5곳(1285 menu-release·1415 mouse-toggle·1680/1688/1714 stdin 핸들러)을 `state.focus.*` 직접 쓰기 대신
**기존 `Action::Focus(FocusTarget)`**(+ 필요 시 `Action::FocusToggle`)로 라우팅해, switcher/루프가 "두 번째 focus writer"가
되지 않게 한다. **apply는 동기이므로 같은 사이트에서 동기 호출**하면 중간-읽기 시맨틱이 보존된다(설계 선택 §6).

> **⚠ 이 하위단계가 P5 전체에서 가장 행동-민감하다.** `handle_mouse_event`(1229–1231 주석)는 focus를 **이벤트마다 즉시**
> 변경한다("routing re-reads focus per event, so deferring would change behavior"), `handle_tree_bytes`는 키마다
> `is_inputting`/focus를 재질의한다. `state.apply(Action::Focus(…))`는 동기라 값은 즉시 반영되지만, **apply arm이 부작용을
> 추가하면(현재는 focus만 세팅) 이 사이트들의 순서가 바뀔 수 있다** → red-first 회귀 테스트로 강제하고, 조금이라도 순서가
> 바뀌면 **이 하위단계를 축소(1680/1688/1714의 핸들러-말미 토글만 라우팅)하거나 P6로 이관**한다(설계 선택 §7-a).

**(b) 실패 테스트 먼저**
- `apply_focus_toggle_flips_view`(state): `Action::FocusToggle`이 Tree↔Terminal을 뒤집고(모달 중이면 prior 뒤집기 — `focus.toggle()`
  위임) 커맨드 없음. 오늘 red: `Action::FocusToggle` 없음 → 컴파일 실패.
- **회귀 강제**(그린 유지 필수): `enter_focuses_terminal_tab_does_not`(runtime 2996)·`kill_confirm_owns_keys_so_prefix_q_and_enter_do_not_quit_or_focus_mux`(4557)·`menu_keyboard_input_is_consumed_without_changing_restore_pane_or_writing_pty`(4683)·
  `handle_mouse_event_view_border_grab_sets_dragging`(4809). 이 중 하나라도 red면 D3 축소.

**(c) 최소 구현** (설계 선택 §7-a)
- `model/action.rs`: `Action::FocusToggle` 추가(`apply` arm: `self.focus.toggle(); Vec::new()`). `Action::Focus(FocusTarget)`는
  이미 존재(set_view_focus 위임).
- runtime.rs 사이트 교체(동기 호출):
  - 1415 `state.focus.toggle()` → `state.apply(Action::FocusToggle);`
  - 1285 `state.focus.set_view_focus(Terminal)` → `state.apply(Action::Focus(FocusTarget::Terminal));`
  - 1680/1688/1714 `set_view_focus(Terminal|Tree)` → 대응 `Action::Focus(…)`.
- **범위 결정(설계 선택 §7-a)**: 회귀가 그린이면 5곳 모두. 순서 리스크가 확인되면 **핸들러-말미 토글(1680/1688/1714)만** 라우팅
  하고 mid-loop(1285/1415)는 동기 `apply` 호출로 **값만** 라우팅(부작용 0 arm이라 안전) — 그래도 "apply 단일 writer" 달성.

**(d) 검증**
`cargo test -p xmux` red→green. 위 회귀 4개 + `focus.rs` 유닛(app_starts_tree_focused_and_toggles 등)·`apply_focus_moves_focus_with_no_command`(state 724) green. clippy/fmt.
- **⚠ 라이브 게이트(사람) 필수**: (1) prefix Tab / Enter / prefix Esc 포커스 토글, (2) 마우스 좌클릭 unfocused 뷰 → 포커스 전환,
  (3) 우클릭 메뉴 "focus terminal", (4) 모달 중 focus toggle이 prior만 뒤집고 모달 유지, (5) terminal 포커스에서 타이핑이 PTY로
  가고 tree로 안 샘. 조금이라도 다르면 D3 축소.

**(e) 파일**: `src/model/action.rs`, `src/state/mod.rs`, `src/app/runtime.rs`

---

## 설계 선택 (구현 전 확정 — 리뷰 포인트)

1. **`Runtime` 형태 = "세계 상태는 구조체, `select!` 소스는 루프-로컬".** `Runtime`이 registry/hosts/mgr/worker/switcher/state/
   widths/sets/term/senders를 소유하고, `run_app`(또는 `Runtime::run`)이 수신기/타이머를 소유하며 `select!`을 돈다. **근거**:
   tokio `select!`이 `self.<rx>.recv()`를 폴링하면서 arm 본문이 `&mut self`를 부르면 차용 충돌 → 수신기는 self 밖. 대안(모든
   것을 `Runtime`에 넣고 arm마다 이벤트 enum을 뽑아 `select!` 종료 후 `dispatch(ev)` 호출)도 가능하나 버스트 드레인이 수신기를
   요구하므로 **수신기를 dispatch 메서드에 `&mut` 파라미터로 전달**하는 형태를 권장. 리뷰에서 "수신기도 `Runtime` 필드 +
   `select!`을 이벤트-뽑기로" 통일할지 확정.
2. **`DrawObserver`/`current_grid` 위치 = runtime.rs 내부.** 별 모듈로 빼면 대량 `&mut` 통과만 늘어난다(P4의 `ui/render.rs`
   기각과 동형 논리). `DrawObserver`는 소형 구조(fingerprints 소유), `current_grid`는 A2에서 자유 함수 → C1에서 `Runtime` 메서드.
3. **S2-6 분할선 = 순수 코어만 `app/input.rs`, 스테이트풀 핸들러는 `Runtime` 메서드.** `handle_stdin_bytes`/`handle_mouse_event`/
   `handle_tree_bytes`는 세계 상태를 통째로 변경하므로 `Runtime` 메서드가 자연스럽다(이동을 두 번 하지 않음). 대안(`InputRouter`
   구조체가 MouseState/term_input/tree_decoder/prefix 소유 + 핸들러 메서드)은 **기각** — `Runtime`이 이미 그 상태를 소유하고,
   `InputRouter`를 별로 두면 핸들러가 다시 `&mut world`를 다인자로 받아 S2-1 목적과 상충. 너비 헬퍼(`terminal_view_size` 등)는
   입력·draw 공용이라 runtime.rs 잔류.
4. **S2-5 깊이 = `Selection`만 아래로.** `from_target`은 `ui::switcher::TerminalViewTarget` 의존 → app 잔류(자유 함수
   `selection_from_target`). `run_switch_plan`/`display_key`/`run_lowered`가 드라이버(display.rs)에서 상향 호출되는 것은
   **별개 발견(P5 범위 밖)** — S2-5는 도메인 값 하나만 내린다. `Selection`의 집은 `model/selection.rs`(신규) 권장(호스트/액션과
   대칭); 리뷰에서 `model/mod.rs` 인라인 vs 별 파일 확정.
5. **크롬 위치 = `State.chrome` 유지(P5에서 `Runtime`로 이동하지 않음).** P4가 이미 `State.chrome`로 단일화했고 render가
   `&state`를 threading한다(state/mod.rs:53–59 주석이 "P5에서 `Runtime`로 재배치 가능"이라 했으나). **근거**: `Runtime`으로
   옮기면 render 시그니처(`&state` 읽기)를 다시 건드려 C1 위험을 키운다 — per-frame 크롬 입력 조립을 `Runtime::prepare_and_draw`
   **메서드가 `self.state.chrome.set_*`로 모으는 것**으로 충분(발견 취지 "Runtime이 조립을 모음" 충족, 소유는 State 유지). 리뷰
   확정 포인트.
6. **S2-2 원칙 = "Action 라우팅 ≠ 지연".** `State::apply`는 동기 → 같은 사이트에서 동기 호출해 중간-읽기 시맨틱 보존. 런타임
   사실이 필요한 결정은 루프 유지, apply엔 데이터로 주입(Tick의 `key_live`/`in_flight` 패턴). (6-a) `DisplayWorker` 수신 반쪽을
   `take_events`로 분리해 루프-로컬화(차용 충돌 회피). 불가 시 (6-b) `worker` 전체를 루프-로컬로 두고 메서드에 `&DisplayWorker`
   파라미터 전달.
7. **S2-2를 얼마나 밀 것인가.** (7-a) **D3(focus)가 가장 민감** — 회귀가 그린이면 5곳 전부, 아니면 핸들러-말미만 라우팅하고
   mid-loop는 값만(부작용 0 arm) 또는 P6 이관. (7-b) D2의 `RearmAttach`는 `now`를 데이터로 받아 즉시(2048)·디바운스(2249 등)를
   한 arm으로 통합. (7-c) **switcher가 여전히 `apply_source_result`/`apply_panes`/`set_active_window`로 `state.groups`/`panes`를
   쓰는 잔여 수렴은 P5 범위 밖으로 명시** — 이는 P4가 op-result만 이관하며 미룬 것과 같은 큰 수렴이고, 태스크의 S2-2 명시 목록
   (`displayed`/`attach_deadline`/`focus`)에 없다. P6 또는 별도 트랙.
8. **S2-3 결정 소유 = `HostDisplay`, registry 조작 = 호출부.** `resolve_ready`가 3분기 결정을 `ReadyOutcome`으로 반환하고,
   `registry.remove/insert`·`attachment.teardown`은 `Runtime::on_display_event`가 outcome을 보고 수행(registry는 `HostDisplay`가
   못 가짐). 필드 비공개 후 필요한 읽기는 접근자(`in_flight_contains` 등)로.

---

## P5 완료 기준

- `cargo test -p xmux` · `cargo clippy -- -D warnings` · `cargo fmt --check` 클린 — **610 tests 유지 또는 상회**(A2/A3/A4/B/
  C1/C2/D1/D2/D3의 신규 유닛만큼 증가; 회귀 0). 이관 테스트(순수 입력 계열, Selection, HostDisplay 계열)는 등가 이동.
- **`#[allow(clippy::too_many_arguments)]` = 0** (스펙 §5 P5 검증: "모든 `too_many_arguments` allow 제거"). `grep -rn "too_many_arguments" src/app/` → 0.
- 각 단계 red-first 후 green. **성공/happy 경로 바이트 동일**(구조·소유 이동 + 동기 Action 라우팅; 런타임 출력 불변).
- **구조 불변식**(스펙 §1·§6): `app`은 mux/machine-특정 분기 0(`driver_for`/`host.mux`/`host.transport` 경유); `run_app`에
  913줄 god-function 없음(`Runtime` + arm당 메서드); `Selection`은 `model`(도메인은 app에서 import 안 함); 도메인 쓰기는
  `State::apply` 단일 소유(`displayed`/`attach_deadline`/`focus`가 Action 경유); `HostDisplay` 필드 비공개.
- **라이브 게이트(사람)**: **C1·C2·D1·D2·D3 필수** — 실 터미널에서 전체 루프(attach/switch/입력 라우팅/draw/resize/재스캔/
  프리즈 없음)가 P5 이전과 시각·동작 동일. **A·B는 자동-테스트-충분**(A4 후 렌더 스냅 1회 권장). D2/D3는 **프리즈 재현**(빠른
  네비) + focus 토글을 사람이 직접.
- **AS-IS 문서**(넓은 CONTEXT 스윕은 P6): `src/app/AGENTS.md`의 Invariants — 현재 "`run_app`은 the whole runtime; 분해는 out
  of scope"(46–47), "`Selection` … lives here"(35·48–49)를 **P5 종료 형태로 갱신**(`Runtime` 구조체 + arm당 메서드 소유;
  `Selection`은 `model`; 도메인 쓰기 apply 단일 소유; `app/input.rs` 순수 코어). `state/mod.rs:53–59` 크롬 "P5에서 relocate
  가능" 주석은 설계 선택 §5 결정(State 유지)으로 정리.

---

## 라이브 게이트 매트릭스 (자동-테스트-충분 vs 라이브-게이트-필요)

| 단계 | 발견 | 위험 | 게이트 |
|---|---|---|---|
| A1 | S2-9 개명 | Low | 자동-충분 |
| A2 | S2-4 `current_grid` | Med | 자동-충분 |
| A3 | S2-7 `DrawObserver` | Med | 자동-충분 |
| A4 | S2-5 `Selection`→model (HOT) | Med | 자동-충분 (A 끝 렌더 스냅 1회) |
| B | S2-6 순수 입력 코어 | Low | 자동-충분 |
| C1 | S2-1 `Runtime` 구조체 | **High** | **라이브 필수(전체 루프)** |
| C2 | S2-3 `HostDisplay::resolve_ready` | Med | **라이브 필수(attach 경합)** |
| D1 | S2-2 `displayed`→Action | High | **라이브 필수(stale-while-revalidate)** |
| D2 | S2-2 `attach_deadline`→Action | High | **라이브 필수 + 프리즈 재현** |
| D3 | S2-2 `focus`→Action | **Highest** | **라이브 필수(focus 토글) · 필요 시 축소** |

---

## P5↔P6 경계 (명시)

P5가 **하지 않고 P6/별도 트랙으로 미루는** 것:

- switcher가 `apply_source_result`/`apply_panes`/`set_active_window`로 `state.groups`/`panes`를 쓰는 잔여 State-writer 수렴
  (설계 선택 §7-c) — 큰 수렴, P4가 op-result만 이관하며 미룬 것과 동류.
- `run_switch_plan`/`display_key`/`run_lowered`를 드라이버(display.rs)가 `crate::app::runtime`에서 상향 호출하는 레이어링
  (설계 선택 §4) — S2-5는 `Selection`만 내림.
- 4중복 `display_inventory` 로깅 dedup(S4-M3, HOT), `driver.rs` 시임 정리, 넓은 CONTEXT.md/신규 AGENTS.md 스윕 = **P6**(스펙 §5 P6).

## 북극성 가산성 재확인 (P5 후)

P5는 오케스트레이션 구조만 바꾸고 축(machine/mux) 확장 경로를 건드리지 않는다(불변식 "supervisor 무지"). `Runtime`은
`driver_for(host)`/`host.mux`/`host.transport`로만 동작을 흘리므로, 새 mux/machine 패밀리 추가 시 `app/` 수정은 **0**을 유지한다
(스펙 §1 북극성). `Selection`이 `model`로 내려가고 도메인 쓰기가 apply로 수렴해 오히려 오케스트레이션의 테스트 가능성·
레이어링이 강화된다.
