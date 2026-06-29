# MuxDriver 추상화 설계 — 의도 입력 / 화면 출력

날짜: 2026-06-29
대상 브랜치: feat/rust-rewrite
선행: `2026-06-26-mux-backend-tui-rearchitecture-design.md` (Phase 5 종착점의 후반부)

## 1. 배경과 문제

`2026-06-26` 재아키텍처는 mux 종류별 어휘·분류를 `Backend` trait + `backend/{tmux,psmux}/` 디렉토리로 옮겼다(Phase Y, "전반부"). 하지만 `Backend`는 여전히 **부품 카탈로그**다: `select() -> SelectOutcome`, `switch_plan() -> SwitchPlan`, `switch_client_argv()`, `attach_plan()` 같은 메서드가 mux별 *결정의 재료*만 반환하고, **결정 자체와 그 결정에 딸린 상태는 supervisor(`cockpit.rs`)가 들고 있다.**

직접 관찰된 증상:

- `cockpit.rs`가 `match host.mux.select() { SharedSwitch => …, PerSessionReattach => … }`를 **6곳**에서 분기한다(`host_selection_key` L374, `select_attach` L475, 윈도우 폴드 L563, `sync_source_terminals` L624, 재연결 re-warm L2595, 키 계산 L1217). mux를 하나 추가하면 이 6곳을 모두 손봐야 한다 — 행동이 캡슐화되지 못했다.
- `select_attach`(L450~579, 약 130줄)가 `SharedSwitch`/`PerSessionReattach`의 attach·switch-client·select-window·grid wipe·in-flight 가드를 한 함수에서 모두 처리한다. tmux와 psmux가 화면을 갱신하는 *방법*이 supervisor에 노출돼 있다.
- 표시 tty 포착(`display_tty`), `switch-client -c <tty>` 메커니즘, 그 "1a 갭"(로컬 psmux는 tty를 못 잡음)이 supervisor 곳곳에 흩어져 있다.

사용자가 명시한 원칙(축자):

> 상위(cockpit)는 의도만 전달한다 — "호스트 H에서 세션 S의 윈도우 W를 표시하라". 어떻게(switch-client / reattach / select-window argv)는 전적으로 그 mux(Backend) 안에만 존재하고, 상위는 몰라야 한다. 그래야 디버깅이 용이해.

`Vec<DisplayAction>`처럼 mux가 "할 일 목록"을 반환하고 supervisor가 해석하는 방식은 **불충분**하다(사용자 반려): `KeepPty`/`RunOnce` 같은 op 집합은 tmux+psmux가 필요로 하는 것의 합집합이라 여전히 누수다. driver가 **결정과 상태를 함께 소유**해야 하고, supervisor는 *일반적 능력*(PTY를 띄우는 능력)만 제공한다.

## 2. 목표 경계 — MuxDriver (의도 in / 화면 out)

호스트당 하나의 driver(`Box<dyn MuxDriver>`). supervisor 루프는 mux별로 **아무것도 분기하지 않는다.**

```rust
trait MuxDriver {                                  // 호스트당 하나
    fn show(&mut self, target: &Target, ctx: &mut DriverCtx);  // 의도: 이 세션+윈도우를 표시
    fn grid(&self) -> Option<Arc<Mutex<Grid>>>;    // supervisor가 오른쪽 패널에 렌더
    fn input(&mut self, bytes: Vec<u8>);           // 표시중 세션으로 전달
    fn pump(&mut self, ev: PtyEvent, ctx: &mut DriverCtx);     // 루프가 raw bytes/EOF를 넘김
    fn sync(&mut self, sessions: &[Session], ctx: &mut DriverCtx); // 인벤토리 갱신 → warm/reap
    fn shown(&self) -> Option<&Selection>;         // 화면에 확정된 표시 진실(결함 A 게이트)
    fn reattach(&mut self, ctx: &mut DriverCtx);   // `r` 재스캔의 명시적 재attach
}
```

`Target`은 supervisor가 아는 일반 의도다: `{ session: String, window: Option<i64> }`. `DriverCtx`는 supervisor가 driver에게 **주입하는 일반 능력**이다(아래 §4).

supervisor 루프:

- 선택 정착 → `driver.show(target, ctx)`
- 렌더 → `driver.grid()`
- 입력 → `driver.input(bytes)`
- `PtyEvent` → `driver.pump(ev, ctx)`
- 인벤토리 변동 → `driver.sync(sessions, ctx)`

**supervisor는 mux 종류를 읽지 않는다.** `SelectOutcome`와 그 6개 match가 사라진다.

## 3. driver가 흡수하는 것 (mux별 private 구현)

| 현재 위치 (supervisor) | 이동 후 (driver private) |
|---|---|
| `SelectOutcome` + 6개 match | driver별 `show()`/`sync()` 내부 분기 |
| `select_attach`의 SharedSwitch 가지 | tmux driver: 첫 attach(warm) + 이후 `switch-client` |
| `select_attach`의 PerSessionReattach 가지 | psmux driver: 세션 바뀌면 `new-session -A -s` 재attach |
| `display_tty` 포착 + `switch-client -c <tty>` + "1a 갭" | tmux driver private (psmux driver는 tty 불필요) |
| `attach_plan`/`switch_plan`/`switch_client_argv`/`*_window_plan` (≈12 getter) | 각 backend의 private 메서드 — public trait 표면 아님 |
| warm / reap / death 오케스트레이션 (`sync_source_terminals`) | driver별 `sync()` |
| `host.display`(current/in_flight/pending/reaped_ids 북키핑) | driver가 소유 |

## 4. 단 하나의 실제 제약과 그 해소

PTY 레지스트리 / `DisplayWorker`(ConPTY 스포너, OS 스레드) / 이벤트 루프는 **supervisor의 것**이다. driver가 스포너를 문자 그대로 소유하지는 않는다. 대신 **스폰 능력을 주입**한다.

```rust
struct DriverCtx<'a> {
    worker: &'a DisplayWorker,        // spawn(argv, cols, rows, id) -> 비동기 Ready
    registry: &'a mut AttachRegistry, // 완성된 attachment의 그릇 (grid/input/reap)
    transport: &'a Transport,         // argv 로워링 (-S socket / ssh 래핑)
    control: Option<&'a HostClient>,  // 열린 -CC 채널 (switch-client/select-window가 탐)
    cols: u16, rows: u16,
    attach_seq: &'a mut u64,
}
```

- driver는 **무엇을 스폰할지 / 언제 switch할지**의 결정과, 결과 attachment(들)의 **소유(북키핑)**를 가진다.
- grid 바이트는 여전히 `PtyEvent`로 루프에 흘러온다. 루프는 id로 소유 driver를 찾아 `pump`로 라우팅한다.
- 따라서 PTY 인프라는 보존된다. **결정과 per-host 표시 상태만** driver로 옮긴다.

이 분리는 재아키텍처 계획의 종착점이다: 5.5(선택 권한 역전) → 5.6(컴포넌트 분해) → 5.7(얇은 app.rs). Phase Y가 전반부(어휘+분류), 이 작업이 후반부(오케스트레이션+상태+PTY 생명주기를 driver로).

## 5. 점진적·무회귀 경로 (big-bang 금지)

`4a5f053`의 확정된 정합성 수정(로컬 psmux가 자기 per-session 서버에 attach)은 **절대 회귀해선 안 된다.** 각 단계 실툴체인 그린 게이트 + 커밋.

### 단계 2 — 동작 보존 seam (이 작업의 핵심 산출)

`MuxDriver` trait + **단 하나의 impl**(`SeamDriver` 같은 단일 구조체)을 도입한다. 이 impl은 **현재의 `select_attach`/grid/input/생명주기 로직을 그대로 호출**한다(로직 이동·재작성 없음). supervisor 루프를 `driver.show/grid/input/pump/sync`로 라우팅한다.

- 동작은 **완전히 동일**. 542 테스트 그린, clippy 0, fmt 0.
- 이 단계는 결정을 옮기기 전에 **경계를 먼저 그어 위험을 제거**한다.
- `SelectOutcome`는 아직 살아있다(seam impl 내부에서 기존 코드를 호출하므로). 6개 match는 아직 supervisor에 있어도 되지만, **그 중 `select_attach`로 흘러드는 진입점은 driver를 거친다.**

핵심 설계 판단(seam impl):

- driver는 호스트당 하나가 아니라, **현재의 free 함수들을 호출하는 어댑터**로 시작한다. 즉 `SeamDriver`는 `&mut Hosts`·`&HostManager` 등을 빌려 기존 `select_attach`를 그대로 호출한다. 이렇게 하면 PTY 인프라·`host.display` 북키핑이 **그 자리에 그대로 있고**, 동작이 비트 단위로 동일함을 보장한다.
- trait 표면은 §2의 최종형을 향하되, 단계 2에서는 supervisor가 실제로 부르는 메서드(`show`/`grid`/`input`/`pump`)만 구현한다. 나머지(`sync`/`shown`/`reattach`)는 단계 3에서 라우팅을 추가하며 흡수한다.

### 단계 3 — per-mux 결정 driver로 이동 (라이브 게이트 직전까지)

tmux driver가 switch-client를, psmux driver가 "라이브 클라이언트+tty ⇒ switch-client; 아니면 ⇒ `new-session -A -s` 재attach"를 소유한다. `cockpit`에서 `SelectOutcome` match를 삭제한다.

**STOP 규칙**: 정합성이 라이브 터미널을 필요로 하는 첫 지점에서 멈추고, 그린인 것을 커밋하고, 라이브 게이트 절차를 보고한다.

### 단계 4 (이 작업 범위 밖, 후속) — 사용자 want 전달

psmux `show()`가 라이브 클라이언트를 in-place로 switch(teardown 없음 ⇒ switch에서 "(attaching…)" 없음). **로컬 display_tty 포착(1a 갭)** 필요 — tty 못 잡으면 재attach로 폴백(동작 상태 무회귀).

## 6. 검증된 사실 (재론하지 말 것)

- **psmux는 세션 전환을 지원한다.** `list-commands`에 `switch-client`/`choose-tree`가 있고, cross-server `switch-client`가 클라이언트를 다른 per-session 서버로 옮기는 것이 확인됐다.
- **psmux는 one-server-per-session**: 세션마다 자기 TCP 포트의 서버(`~/.psmux/<name>.port`). 기본 소켓이 `list-sessions`/`display-message -p -t <name>`를 서버 간 라우팅한다.
- **올바른 attach argv**: `psmux new-session -A -s <name>`(있으면 attach, `-d` 없음)가 그 세션의 서버로 라우팅한다(`4a5f053`).
- **1a 갭**: 로컬 `display_tty`는 포착되지 않는다. 원격 경로는 셸 마커(`printf '\033]XMUX-DISPLAY-TTY:%s\007' "$(tty)"`)를 prepend하고 PTY pump가 파싱하지만, 로컬 attach는 셸 없이 mux 바이너리를 직접 실행해 tty가 없다. 단계 4에서 해소(셸 래핑 또는 list-clients diff, 실패 시 재attach 폴백).
- **헤드리스 attach는 검증 불가 + 라이브 psmux 상태를 오염**시킨다. `psmux attach`를 winpty/portable-pty로 띄우지 말 것. 라이브 게이트는 사람의 몫. read-only 프로브(`list-sessions`, `display-message -p`, `~/.psmux/*.port`, `list-commands`)만 안전.

## 7. 테스트 전략

- **단위**: `MuxDriver`의 trait 경계가 object-safe함, seam driver의 `show`/`grid`/`input`/`pump`가 기존 free 함수와 **동일 인자로 위임**함(위임 증명 테스트), `Target` ↔ `Selection` 매핑.
- **회귀(필수)**: `4a5f053`의 정합성 테스트(`psmux_select_attach_*`, `psmux_attach_plan_routes_to_the_per_session_server`)가 seam 경유 후에도 그린.
- **결함 A 불변**: `display_matches_selection`이 표시 진실을 게이트(stale attach는 "(attaching…)").
- **라이브 게이트**: 실제 psmux test/test2 전환이 올바른 세션을 표시, in-place switch에서 "(attaching…)" 없음, 깜빡임 없음 — 사람의 눈으로 최종 확인.

## 8. 비목표 (Non-goals)

- 세션 lifecycle(create/kill/rename) 신규 기능 — 형태만 driver로 흡수, 동작 불변.
- 새 mux(zellij 등) 실제 구현.
- ctl 와이어 프로토콜 변경.
- Transport 계층 재설계(`Local`/`Ssh` 분리 유지).
- 단계 4(in-place switch + 1a 갭)는 이 작업 범위 밖(라이브 게이트 후속).
