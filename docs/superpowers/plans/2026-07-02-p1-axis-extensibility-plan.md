# P1 구현 계획 — 축 확장성 하드닝 (OCP · ISP · LSP)

> **상태: 설계(미구현).** xmux 이상적 구조 리팩토링 · 2026-07-02 · 브랜치 `refactor/ideal-structure`
> (워크트리 `.claude/worktrees/refactor-ideal/`, 베이스 `cf5e574`).
> 마스터 스펙: `docs/superpowers/specs/2026-07-02-ideal-structure-refactor-design.html` (§1 북극성 · §2 확정 결정 · §5 P1 표 · §6 불변식).
> 선행: P0 완료(`cf5e574` 위 7커밋). **베이스라인: 585 tests / clippy0 / fmt0.**
> 경로는 repo-상대. 앵커(`file:line`)는 감사 시점 `0818679` 기준이라 `cf5e574` rebase로 이동했다 —
> 아래 앵커는 이 워크트리의 실코드로 **재확인**한 값이며, 구현 중 심볼/패턴으로 다시 확인한다(LSP는 신뢰 말 것).
> TDD 규율: 각 단계 = 실패 테스트(red) → 최소 구현 → green + 회귀 0. success 경로는 바이트 동일.

---

## 확정 결정 (재론 금지)

- **MACHINE 팩토리 = Option A.** 함수형 팩토리 `machine::local`/`ssh`(/미래 `wsl`) 유지, 선택은 단 한 곳
  (`MachineKind::transport()` — `Hosts::build`가 소비하는 단일 match)에서. `Source.remote: bool` → `MachineKind` enum.
  `known_machines()` 레지스트리 **없음**.
- **MUX = 슬림 단일 trait.** 바이트 동일한 9개 command-plan 메서드 → `self.bin()` 위 **trait 기본 구현**.
  옵션 in-place-switch 3메서드 → 불투명 `switch_in_place(host_key, session, display_tty) -> Option<SwitchPlan>`
  하나(드라이버가 blind 실행). `attach_plan`의 죽은 `window` 파라미터 제거. 역할 sub-trait 분할 **안 함**.

---

## 단계 순서 & 조율

의존/충돌을 줄이려 **비-HOT(가산·순수) 먼저 → HOT(타 세션 파일) 마지막**으로 배치한다.

1. **S4-H1** — 9 command-plan → trait 기본 구현 *(trait 슬림화가 토대)*
2. **S4-L3** — tmux 폴백 3중복 → 공유 헬퍼 하나
3. **S4-L2** — `Mux::clone_box` + `run_poll`이 `for_kind` 재구성 대신 mux를 받음
4. **S4-H2** — tmux `list-clients` 와이어 포맷 → `ControlProtocol` 뒤로
5. **S3-H2a** — `MachineKind` enum + 단일 transport 팩토리 *(P1 최대 churn)*
--- 이하 **HOT**: `driver.rs` / `mux/tmux/display.rs` / `mux/psmux/display.rs`를 편집 중인 타 세션의
    테스트 전용 `Local::boxed`/`Ssh::boxed` 마이그레이션이 병합된 뒤 **rebase하고** 착수 ---
6. **S4-L1** — `attach_plan`의 죽은 `window` 파라미터 제거 `[HOT: display.rs]`
7. **S4-M1 + S4-M4** — `SwitchPlan` + `switch_in_place` 통합 `[HOT: display.rs + 실행 헬퍼]` · **라이브 게이트**
8. **S3-H4** — `is_remote` 과부하 → 능력 술어 `[HOT: display.rs]`

> **HOT 근거(스펙 §7)**: 타 세션이 편집 중인 파일은 `driver.rs` + 두 `display.rs`뿐이며, 그쪽은 테스트 영역
> (`*::boxed` 생성자), 이쪽 편집은 프로덕션 영역(`show`/`sync`/`with_display_tty_record`) + 일부 tmux 테스트
> 재작성이라 충돌은 작지만 실재한다. 6·7·8은 display.rs를 만지므로 세 번 지나가지 않게 인접 배치했다.

---

## 단계 1 — S4-H1: 9개 command-plan 메서드 → trait 기본 구현

**(a) 목표**
tmux/psmux가 바이트 동일하게 반복하는 9개 command-plan 메서드(`list_panes_plan`/`new_window_plan`/
`split_window_plan`/`select_window_plan`/`kill_window_plan`/`rename_window_plan`/`new_session_plan`/
`kill_session_plan`/`rename_session_plan`)를 `Mux` trait의 `self.bin()` 기반 **기본 구현**으로 올려, tmux
호환 mux가 이 9개 동사를 공짜로 얻게 한다(북극성: "새 mux = identity + 몇 메서드, 동사는 공짜").

**(b) 실패 테스트 먼저**
`src/mux/mod.rs`의 `mod tests`에 최소 mock mux + 가산성 테스트 `bare_tmux_compatible_mux_gets_command_plans_for_free` 추가:
- 테스트 전용 `struct BareMux { bin: String }`가 `Mux`의 **필수** 메서드만 구현(`kind`/`bin`/`server_model`/
  `driver`(→ `Box::new(crate::mux::tmux::TmuxDriver)`)/`enumerate`(→ `enumerate_via_list_sessions`)/`attach_plan`/
  `control_argv`(→ `None`)/`death_signal`/`event_source`) — 9개 command-plan 메서드는 **구현하지 않음**.
- 단언: `BareMux{bin:"tmux".into()}.list_panes_plan("work") == mux::list_panes("tmux","work")` 및
  `new_session_plan("dev") == mux::new_session("tmux","dev")` (대표 2개로 window/session 양쪽 커버).
- 오늘 red인 이유: 9개 메서드에 기본 구현이 없어 `BareMux`가 "not all trait items implemented"로 **컴파일 실패**
  — 가산성이 아직 성립하지 않음을 그대로 드러낸다.

**(c) 최소 구현** (심볼로 재탐색; LSP-lies-here)
- `src/mux/mod.rs`의 `Mux` trait에서 9개 메서드 시그니처(현재 `mux/mod.rs:221-235` 부근의 abstract 선언)를
  각각 `self.bin()`에 위임하는 **기본 구현**으로 교체: 예 `fn list_panes_plan(&self, session: &str) -> Vec<String> { mux::list_panes(self.bin(), session) }` … 9개 모두 동형(`mux::<verb>(self.bin(), …)`).
- `src/mux/tmux/mod.rs`(현 `115-141`)와 `src/mux/psmux/mod.rs`(현 `118-144`)에서 이 9개 메서드의 **명시적
  override를 삭제**(두 impl 모두 `mux::<verb>(&self.bin, …)`로 기본 구현과 바이트 동일 → 기본 구현 상속).
- `poll_once` 기본 구현이 `self.list_panes_plan(...)`을 호출(mux/mod.rs:212)하므로 기본 구현화 후에도 그대로 동작.

**(d) 검증**
`cargo test -p xmux mux` red→green. 기존 `tmux_window_plans_match_mux_builders`(mux/mod.rs:380)·
`tmux_session_plans_match_mux_builders`·`psmux_window_plans_use_the_psmux_binary`·
`psmux_session_plans_use_the_psmux_binary`·`psmux_behavior_is_decoupled_from_invoked_binary` 그대로 green
(동일 argv). `poll_once_emits_sessions_then_panes_on_success` green. `cargo clippy -- -D warnings` /
`cargo fmt --check` 클린. 자동 테스트 충분(라이브 불요).

**(e) 파일**: `src/mux/mod.rs`, `src/mux/tmux/mod.rs`, `src/mux/psmux/mod.rs`

---

## 단계 2 — S4-L3: tmux 폴백 3중복 → 공유 엔트리 하나

**(a) 목표**
`for_binary`·`for_kind`·`detect_backend` 세 팩토리가 각기 하드코딩한 `Box::new(Tmux { bin: … })` 폴백을
공유 헬퍼 하나로 모아, tmux가 암묵적 폴백이라는 사실이 한 곳에만 있게 한다(OCP/DRY, 무동작 변경).

**(b) 실패 테스트 먼저**
> 순수 리팩토링(바이트 동일)이라 행동 red가 없다. 기존 green 테스트가 안전망이며, 폴백-바이너리 보존을 못박는
> 얇은 회귀 테스트를 추가한다. `src/mux/mod.rs` tests에 `fallback_preserves_the_invoked_binary`:
> `assert_eq!(for_binary("some-fork").kind(), "tmux"); assert_eq!(for_binary("some-fork").bin(), "some-fork");`
> `assert_eq!(for_kind("nope","tcustom").bin(), "tcustom");` — 이 단언은 리팩토링 전에도 통과하므로 red-first가
> 아니다(이 단계는 명시적으로 무동작 정리). 안전망 = 이 테스트 + 기존 `for_binary_picks_psmux_else_tmux`·
> `for_kind_preserves_identity_and_invoked_binary`·`detect_backend_*` 전부 green 유지.

**(c) 최소 구현**
- `src/mux/mod.rs`에 private 헬퍼 하나 추가: `fn tmux_fallback(bin: &str) -> Box<dyn Mux> { Box::new(Tmux { bin: bin.to_string() }) }` (tmux는 positive help 시그널이 없어 `known_muxes()` 엔트리가 될 수 없으므로 레지스트리가 아니라 명시적 폴백 헬퍼가 맞다).
- `for_binary`(현 `254-263`)·`for_kind`(현 `267-276`)·`detect_backend`(현 `299-323`)의 각 `Box::new(Tmux { … })`을
  `tmux_fallback(bin)` 호출로 교체.

**(d) 검증**
`cargo test -p xmux mux` 전부 green(신규 + 기존). `cargo clippy`/`fmt` 클린. 자동 테스트 충분.

**(e) 파일**: `src/mux/mod.rs`

---

## 단계 3 — S4-L2: `run_poll`이 `(kind,bin)` 재구성 대신 mux를 그대로 받음

**(a) 목표**
`run_poll`이 `(mux_kind, mux_bin)` 문자열로 `for_kind`를 다시 태워 mux를 재구성하는 결합을 제거하고,
호스트의 실제 mux를 소유 복제로 넘겨받게 한다(DRY/결합도; 미래 mux의 poll 동작이 (kind,bin) 외에 의존해도 유실 없음).

**(b) 실패 테스트 먼저**
`src/mux/mod.rs` tests에 `mux_clone_box_preserves_identity_and_binary` 추가:
`let m: Box<dyn Mux> = psmux().clone_box(); assert_eq!(m.kind(),"psmux"); assert_eq!(m.bin(),"psmux");`
및 tmux 동형. 오늘 red인 이유: `Mux`에 `clone_box`가 없어 컴파일 실패(기존 `Transport::clone_box` 관용구와 동형).

**(c) 최소 구현** (결정: `Arc<dyn Mux>` 대신 `clone_box` — 기존 `Transport::clone_box`와 대칭, ripple 최소)
- `src/mux/mod.rs`의 `Mux` trait에 `fn clone_box(&self) -> Box<dyn Mux>;` 추가(기본 없음 — 각 mux가 자신을 복제).
  (선택) `impl Clone for Box<dyn Mux>`도 추가해 `.clone()` 사용 가능하게 — 필수는 아니며 호출부가 `clone_box()`면 생략.
- `src/mux/tmux/mod.rs`·`src/mux/psmux/mod.rs`: `fn clone_box(&self) -> Box<dyn Mux> { Box::new(Self { bin: self.bin.clone() }) }` 각 1줄.
- `src/host/mod.rs` `run_poll`(현 `647-655`): 시그니처에서 `mux_kind: String, mux_bin: String`을 `mux: Box<dyn crate::mux::Mux>` 하나로 교체하고 본문 첫 줄 `let mux = crate::mux::for_kind(&mux_kind, &mux_bin);`(현 `655`) 삭제. `mux.poll_once(...)` 그대로.
- `src/host/mod.rs` spawn 사이트(현 `786-793`): `host.mux.kind().to_string(), host.mux.bin().to_string()` 두 인자를 `host.mux.clone_box()` 하나로 교체.

**(d) 검증**
`cargo test -p xmux` red→green. `run_poll`을 태우는 기존 테스트(poll 경로: `poll_once_*`는 mux 직접 호출이라
무관; manager ensure 경로 테스트)가 green 유지. `tokio::spawn`의 `Send + 'static` 요구는 `Box<dyn Mux>`
(`Mux: Send + Sync`)가 충족 — 컴파일이 즉시 증명. `cargo clippy`/`fmt` 클린. 자동 테스트 충분.

**(e) 파일**: `src/mux/mod.rs`, `src/mux/tmux/mod.rs`, `src/mux/psmux/mod.rs`, `src/host/mod.rs`

---

## 단계 4 — S4-H2: tmux `list-clients` 와이어 포맷 → `ControlProtocol` 뒤로

**(a) 목표**
`host/`에 하드코딩된 tmux `list-clients -F '#{client_tty} #{client_flags}'` 와이어 포맷과 그 블록-바디
파서(control-mode 플래그로 display 클라이언트 판별)를 `ControlProtocol` 뒤로 옮겨, `host/`가 어떤 tmux 와이어
세부도 이름 붙이지 않게 한다(OCP/SoC/명시성 — 다른 control-mode mux가 자신의 포맷을 제공 가능).

**(b) 실패 테스트 먼저**
`src/mux/tmux/control_proto.rs`의 `mod tests`(파일 하단)에 두 테스트 추가:
- `display_clients_line_pins_the_tmux_wire_format`: `assert_eq!(TmuxControl.display_clients_line(), "list-clients -F '#{client_tty} #{client_flags}'\n");`
- `parse_display_client_tty_picks_the_non_control_client`: body `["/dev/pts/7 control-mode", "/dev/pts/3 active-pane,focused"]` → `Some("/dev/pts/3")`; body `["/dev/pts/7 control-mode"]` → `None`.
- 오늘 red인 이유: `ControlProtocol::display_clients_line`/`parse_display_client_tty`와 그 tmux 구현이 없어 컴파일 실패.

**(c) 최소 구현**
- `src/mux/control.rs`의 `ControlProtocol` trait에 두 메서드 추가:
  `fn display_clients_line(&self) -> String;` 와 `fn parse_display_client_tty(&self, body: &[String]) -> Option<String>;`
  (독스트링: "list-clients 질의 라인" / "블록 바디에서 display 클라이언트 tty 선택 — control-mode 플래그 없는 첫 클라이언트").
- `src/mux/tmux/control_proto.rs`에 순수 파서 `pub(crate) fn parse_display_client_tty(body: &[String]) -> Option<String>` 추가 — `host/mod.rs`의 현 `display_client_tty`(`258-272`) 로직을 **그대로 이식**(첫 non-`control-mode` 라인의 tty, splitn(2,' ')).
- `src/mux/tmux/mod.rs`의 `impl ControlProtocol for TmuxControl`에 구현: `display_clients_line`은 포맷 문자열 반환; `parse_display_client_tty`는 `control_proto::parse_display_client_tty(body)` 위임(`classify`가 `control_proto::classify`에 위임하는 패턴과 동형, tmux/mod.rs:154).
- `src/host/mod.rs`:
  - 자유 함수 `display_tty_query_line()`(`248-250`)와 `display_client_tty(body)`(`258-272`) **삭제**.
  - `capture_display_tty`(현 `570-575`)의 `line: display_tty_query_line()`(`572`) → `line: self.proto.display_clients_line()`.
  - `resolve_block`(현 `274`)에 `proto: &dyn ControlProtocol` 파라미터 추가; `PendingReply::DisplayClientTty` 팔(현 `319-326`)의 `tty: display_client_tty(body)`(`324`) → `tty: proto.parse_display_client_tty(body)`. 호출부(현 `199` `resolve_block(host, kind, &body, state, &mut emit)`)에 `proto` 전달(`run_reader`는 이미 `proto: &dyn ControlProtocol` 보유, host/mod.rs:158).
- 테스트 이동: `host/mod.rs`의 `display_tty_query_line_lists_client_tty_and_flags`(`1472`)·
  `display_client_tty_picks_the_non_control_client_with_many_clients`(`1480`)·`display_client_tty_is_none_when_only_a_control_client_is_attached`(`1493`)는 tmux 와이어 세부라 (b)의 control_proto.rs 테스트가 흡수 → host 쪽에서 삭제. 통합 테스트 `reader_resolves_display_tty_block_into_event`(`1502`)는 `test_control_proto()`(=tmux proto, host/mod.rs:857)를 쓰므로 **그대로 green 유지**(resolve_block가 proto 경유로 파싱).

**(d) 검증**
`cargo test -p xmux` red→green. `reader_resolves_display_tty_block_into_event`·`writer_query_list_panes_correlates`
등 리더/라이터 테스트 green. `host/`에 `#{client_tty}`/`#{client_flags}`/`control-mode` 리터럴이 하나도 남지
않음을 grep로 확인(SoC 완결). `cargo clippy`/`fmt` 클린. 자동 테스트 충분(원격 -CC tty 캡처는 라이브지만
파서/질의는 유닛이 커버; 단계 7 라이브 스모크에 함께 실릴 수 있음).

**(e) 파일**: `src/mux/control.rs`, `src/mux/tmux/mod.rs`, `src/mux/tmux/control_proto.rs`, `src/host/mod.rs`

---

## 단계 5 — S3-H2a: `MachineKind` enum + 단일 transport 팩토리

**(a) 목표**
transport 종류 선택이 세 곳(`Source::transport` / `transport_for_source` / `Hosts::build` 인라인)에 흩어진 것을
단일 팩토리(`MachineKind::transport()` — Option A의 유일 match 사이트)로 수렴하고, 세 번째 패밀리를 표현 못 하는
`Source.remote: bool`을 `MachineKind` enum으로 교체한다. **Source 완전 축소·`transport_for_source`/`Source::transport`
완전 삭제는 P2**(레지스트리 통합 후) — 여기서는 생성/종류 표현만 통일한다(전이적 중복은 P2가 정리).

**(b) 실패 테스트 먼저**
`src/machine/mod.rs` tests에 `machine_kind_selects_the_family_at_one_site` 추가:
- `MachineKind::Local { socket: Some("/tmp/s".into()) }.transport()` → `host_id()=="local"`, `!is_remote()`, `exec_argv(false,&["tmux".into(),"ls".into()])`가 `-S /tmp/s`를 주입.
- `MachineKind::Ssh { alias:"prod".into(), control_path:String::new(), os:"linux".into() }.transport()` → `host_id()=="prod"`, `is_remote()`.
- 오늘 red인 이유: `MachineKind`/`MachineKind::transport`가 없어 컴파일 실패.

**(c) 최소 구현** (결정: 자기완결 param-carrying enum — 아래 "설계 선택" 참조)
- `src/machine/mod.rs`에 추가:
  ```
  #[derive(Clone, Debug)]
  pub enum MachineKind {
      Local { socket: Option<String> },
      Ssh { alias: String, control_path: String, os: String },
      // 미래: Wsl { distro: String } — 변이체 + 아래 match arm 한 줄이면 끝.
  }
  impl MachineKind {
      /// 머신 선택의 유일 지점(Decision A). 새 패밀리 = 변이체 + arm 한 줄, 다른 match/if 수정 0.
      pub fn transport(self) -> Box<dyn Transport> {
          match self {
              MachineKind::Local { socket } => local(socket),
              MachineKind::Ssh { alias, control_path, os } => ssh(alias, control_path, os),
          }
      }
  }
  ```
- `src/source.rs`: `Source`(현 `92-109`)에서 `remote: bool`(`98`)·`control_path`(`100`)·`os`(`102`)·`socket`(`106`)
  필드를 제거하고 `kind: MachineKind` 하나로 대체(유지: `alias`(호스트 id)·`binary`(mux)·`runner`). `Source::transport()`
  (현 `148-158`) 본문을 `self.kind.clone().transport()`로 교체. `os`/`socket`을 외부에서 읽는 곳을 위해 얇은
  접근자 추가: `pub(crate) fn os(&self) -> &str`(Local이면 `std::env::consts::OS` 아님 — kind에 os가 없는 Local은
  런타임 os를 별도 보관해야 하므로 아래 주의)·`pub(crate) fn local_socket(&self) -> Option<String>`.
  - **주의(Local os)**: 현재 `Source.os`는 로컬 소스도 보유하고 runtime.rs:1826이 "첫 src의 os"로 host os를 읽는다.
    `MachineKind::Local`은 os를 담지 않으므로, host os는 `Ssh.os` 또는 config/`build`의 `os` 인자에서 얻어야 한다.
    최소안: `source::build`가 이미 `os` 인자를 받으므로 로컬 소스의 os 의존을 없애고, runtime.rs:1823-1827의
    "첫 src.os" 읽기를 `env`가 이미 보유한 os(있으면) 또는 `std::env::consts::OS`로 단순화(로컬은 항상 이 머신).
    이 정리는 S3-H2a 범위 안(중복 os 표현 제거)이며 P2의 Source 축소와 상충하지 않는다.
- `src/source.rs` `build`(현 `182-214`): 로컬 → `kind: MachineKind::Local { socket: local_socket }`, ssh →
  `kind: MachineKind::Ssh { alias: spec.alias.clone(), control_path, os: os.to_string() }`(+ `alias: spec.alias`).
- `src/app/runtime.rs`:
  - `transport_for_source`(현 `560-566`) 본문을 `src.transport()` 위임으로 축약(래퍼는 P2가 삭제; 여기서는 match 중복만 제거).
  - `local_socket_opt`(현 `1828-1832`)의 `s.socket.clone()` → `s.local_socket()`. `host_os`(현 `1823-1827`)는 위 주의대로 처리.
- `src/model/hosts.rs` `build`(현 `43-72`): 로컬/ssh 생성(`54`,`67`)을 `MachineKind::Local { socket: local_socket }.transport()`·
  `MachineKind::Ssh { alias: spec.alias, control_path, os: os.to_string() }.transport()`로 교체 → **선택 match가 이제 유일 지점**.

**(d) 검증**
`cargo test -p xmux` red→green. transport 결과가 바이트 동일하므로 기존 `machine::tests`(local/ssh 팩토리),
`model::hosts::tests`(`build_puts_local_first_then_ssh_hosts_in_order`·`build_local_socket_threads_into_the_transport`),
`source::tests`(`build_puts_local_first`)가 green 유지 — 단, `remote` 필드를 읽는 단언(source.rs:414/416
`!srcs[0].remote`/`srcs[1].remote` 및 test 헬퍼 `src(...,remote,...)` 시그니처, `265-`)은 `kind` match로 재작성.
`env.rs`가 `srcs`를 clone하므로 `MachineKind: Clone` 확인. `cargo clippy`/`fmt` 클린. 자동 테스트 충분(생성 결과가
바이트 동일 → 라이브 불요).

**(e) 파일**: `src/machine/mod.rs`, `src/source.rs`, `src/app/runtime.rs`, `src/model/hosts.rs`
*(문서: `src/machine/AGENTS.md`의 "is_remote()가 유일 질의" 서술은 단계 8에서 갱신)*

---

## 단계 6 — S4-L1: `attach_plan`의 죽은 `window` 파라미터 제거 `[HOT: display.rs]`

**(a) 목표**
두 impl 모두 `_window`로 무시하는 `attach_plan(session, window: Option<i64>)`의 죽은 파라미터를 제거한다
(YAGNI/명시성 — 윈도우 선택은 `select_window_plan`/transport의 pre-select가 담당).

**(b) 실패 테스트 먼저**
`src/mux/mod.rs`의 `tmux_attach_plan_is_plain_attach`(현 `358-369`)를 새 시그니처로 수정:
`assert_eq!(m.attach_plan("api"), argv(&["tmux","attach","-t","api"]));` (죽은 `Some(2)` 케이스 삭제).
오늘 red인 이유: 인자 2개를 넘기던 호출을 1개로 바꾸면 현 시그니처와 불일치로 **컴파일 실패**(시그니처 변경 유도 red).

**(c) 최소 구현**
- `src/mux/mod.rs` trait: `fn attach_plan(&self, session: &str) -> Vec<String>;`(현 `119`에서 `window` 제거).
- `src/mux/tmux/mod.rs`(현 `73`)·`src/mux/psmux/mod.rs`(현 `70`): `_window` 파라미터 제거(본문 이미 미사용).
- 호출부 인자 제거: `src/source.rs:126`(`attach_plan(name, window)` → `attach_plan(name)`; `window`는 `127`의 pre_select에 계속 사용),
  `src/mux/tmux/display.rs:57`·`117`·`206`, `src/mux/psmux/display.rs:155`.
- 나머지 테스트의 `attach_plan(_, None)`/`(_, Some(_))` 호출(mux/mod.rs:468,507,519,566)에서 두 번째 인자 제거.

**(d) 검증**
`cargo test -p xmux` red→green. `psmux_attach_plan_routes_to_the_per_session_server`·
`*_behavior_is_decoupled_from_invoked_binary`·`detect_backend_classifies_psmux_by_help_marker` 등 green.
`cargo clippy`(미사용 파라미터 경고 소멸)/`fmt` 클린. 자동 테스트 충분.

**(e) 파일**: `src/mux/mod.rs`, `src/mux/tmux/mod.rs`, `src/mux/psmux/mod.rs`, `src/source.rs`, `src/mux/tmux/display.rs`, `src/mux/psmux/display.rs`

---

## 단계 7 — S4-M1 + S4-M4: `SwitchPlan` + `switch_in_place` 통합 `[HOT] · 라이브 게이트`

**(a) 목표**
옵션 in-place-switch 3메서드(`switch_client_argv` / `display_tty_record_prefix` / `switch_via_recorded_tty_cmd`)를
불투명 `switch_in_place(host_key, session, display_tty) -> Option<SwitchPlan>` 하나로 통합하고, 드라이버가 반환
plan을 **blind 실행**한다. tmux의 `tty >file` 메커니즘이 `Mux` trait 경계로 새는 것을 차단(ISP/응집도/LSP).
psmux의 동일-기본구현 `switch_client_argv` override도 함께 소멸(**S4-M4 흡수**).

**(b) 실패 테스트 먼저** — 세 층
- `src/mux/tmux/mod.rs`의 재작성된 `display_identity_tests`에
  `tmux_switch_in_place_returns_a_remote_shell_plan_reading_its_recorded_tty`:
  `let SwitchPlan::Shell(cmd) = Tmux{bin:"tmux".into()}.switch_in_place("jup","test2", None).unwrap() else { panic!() };`
  단언 `cmd.contains("cat ") && cmd.contains("jup") && cmd.contains("switch-client -c") && cmd.contains("test2") && cmd.contains("[ -n")`.
- `src/mux/psmux/mod.rs` tests에 `psmux_switch_in_place_is_exec_plan_with_tty_and_none_without`:
  `Some("/dev/pts/3")` → `SwitchPlan::Exec(v)` 이고 `v[0]==["psmux","switch-client","-c","/dev/pts/3","-t","target"]`,
  `v[1]==["psmux","refresh-client","-t","/dev/pts/3"]`; `None`/`Some("")` → `switch_in_place(...) == None`.
- 오늘 red인 이유: `SwitchPlan` 타입과 `Mux::switch_in_place`가 없어 컴파일 실패.

**(c) 최소 구현**
- **SwitchPlan 정의** (`src/mux/mod.rs`, `pub`; mux-저작 의도 값):
  ```
  /// mux가 저작한 불투명 in-place 스위치 계획. 드라이버는 transport로 통째로 실행하며 변이체 의미를 검사하지 않는다.
  pub enum SwitchPlan {
      /// 비대화 exec 경로로 순서대로 실행할 mux argv들(psmux: switch-client, 이어서 refresh-client).
      Exec(Vec<Vec<String>>),
      /// 호스트 셸에서 실행할 raw 셸 명령(tmux: 기록해 둔 tty 파일을 읽어 switch+refresh를 한 셸에서).
      /// 원격 셸이 없는 머신에서는 스위치가 성립하지 않아 드라이버가 reattach로 폴백한다.
      Shell(String),
  }
  ```
- **trait 메서드 통합** (`src/mux/mod.rs`):
  - 기본 `switch_client_argv`(현 `126-135`)·`display_tty_record_prefix`(현 `147-149`)·`switch_via_recorded_tty_cmd`(현 `160-162`) **삭제**.
  - 추가: `fn switch_in_place(&self, _host_key: &str, _session: &str, _display_tty: Option<&str>) -> Option<SwitchPlan> { None }` (기본 None — in-place 스위치를 지원 않는 mux).
- **tmux** (`src/mux/tmux/mod.rs`):
  - override `display_tty_record_prefix`(현 `77-84`)·`switch_via_recorded_tty_cmd`(현 `86-97`) 삭제.
  - `switch_in_place` 구현: `Some(SwitchPlan::Shell(<현 switch_via_recorded_tty_cmd 본문 문자열>))` (`display_tty_path`+`self.bin` 사용; `display_tty` 인자는 무시 — tmux는 파일에서 읽음). `display_tty` 미사용은 `_display_tty`.
  - 기록 프리픽스 로직은 **패밀리-private 자유 함수**로 강등: `pub(super) fn record_prefix(host_key: &str) -> String { format!("tty >{} 2>/dev/null; ", display_tty_path(host_key)) }` (더 이상 trait 메서드 아님 → `tty >file`이 Mux 경계 밖으로 안 샘).
- **psmux** (`src/mux/psmux/mod.rs`): override `switch_client_argv`(현 `88-100`) **삭제**(S4-M4). `switch_in_place` 구현:
  `display_tty.filter(|t| !t.is_empty()).map(|tty| SwitchPlan::Exec(vec![ vec![self.bin.clone(),"switch-client".into(),"-c".into(),tty.into(),"-t".into(),mux::quote_target(session)], vec![self.bin.clone(),"refresh-client".into(),"-t".into(),tty.into()] ]))`.
- **공유 실행 헬퍼** (`src/app/runtime.rs`, `run_lowered` 옆 — `driver.rs`(HOT) 추가 churn 회피, 두 display.rs가 이미 `crate::app::runtime` import):
  ```
  pub(crate) fn run_switch_plan(host: &crate::model::Host, plan: crate::mux::SwitchPlan) -> bool {
      match plan {
          SwitchPlan::Exec(argvs) => { for a in &argvs { let (c,ar)=host.transport.exec_argv(false,a); let mut v=vec![c]; v.extend(ar); run_lowered(LoweredSwitch::Local(v)); } true }
          SwitchPlan::Shell(cmd) => match host.transport.raw_ssh_argv(&cmd) { Some(argv)=>{ run_lowered(LoweredSwitch::RawSsh(argv)); true } None=>false },
      }
  }
  ```
  (변이체→lowering 매핑은 transport측 `LoweredSwitch::{Local,RawSsh}`와 1:1 — 드라이버는 mux 타입을 이름 붙이지 않음.)
- **tmux 드라이버 호출부** (`src/mux/tmux/display.rs`):
  - in-place 스위치 블록(현 `86-101`)을 `let switched = host.mux.switch_in_place(&key, &sel.session, None).map(|p| crate::app::runtime::run_switch_plan(host, p)).unwrap_or(false);`로 교체(불변 borrow로 plan 획득 → 실행 → 이후 `set_shows`는 별도 `&mut`; 로컬 tmux는 `raw_ssh_argv==None`→false→기존대로 reattach 폴백).
  - `with_display_tty_record`(현 `237-246`): `host.mux.display_tty_record_prefix(host_key)`(현 `239`) → `super::record_prefix(host_key)`(String, 무조건). is_remote 게이트는 **단계 8**이 `runs_through_shell()`로 교체(같은 함수 → 8과 인접 실행).
- **psmux 드라이버 호출부** (`src/mux/psmux/display.rs`): in-place 스위치 블록(현 `100-108`)의 `switch_client_argv`+exec+`refresh_client_lowered`를 `if let Some(p)=host.mux.switch_in_place(&key,&sel.session,Some(&tty)){ crate::app::runtime::run_switch_plan(host,p); }`로 교체(이미 `(live, Some(tty))` 가드 안이라 항상 Some). 자유 함수 `refresh_client_lowered`(현 `245-256`)는 plan에 흡수되어 **삭제**.
- **문서 동기(AS-IS)**: `model/host.rs:82,146`·`model/plan.rs:32`의 `mux.switch_client_argv` 서술과
  `mux/psmux/AGENTS.md:42`·`mux/tmux/AGENTS.md:34-35`·`mux/AGENTS.md:27`의 3메서드 언급을 `switch_in_place`/`SwitchPlan`으로 갱신.

**(d) 검증**
`cargo test -p xmux` red→green. 드라이버 상태 테스트 green 유지: `psmux_driver_show_switches_in_place_when_tty_known`
(no teardown/no reattach/shows 갱신), `psmux_driver_show_reattaches_when_tty_unknown`(tty 없으면 reattach),
`seam_show_replaces_the_psmux_display_attachment`, `tmux_driver_show_warms_the_shared_host_pty_on_first_attach`,
`remote_shared_attach_records_its_display_tty`(이제 `record_prefix` 경유), `local_shared_attach_is_not_prefixed`.
`run_switch_plan`의 argv 조립은 위 유닛으로 커버(테스트에선 `run_lowered`가 detached spawn이라 실제 서브프로세스는
실패해도 상태 단언에 무영향). `cargo clippy`/`fmt` 클린.
- **⚠ 라이브 게이트(사람)**: in-place 스위치 실행 경로를 재배선하므로 argv/상태 동일성만으로는 화면 착지를 보장 못 함.
  실기 스모크 권장 — 원격 tmux 세션 스위치(jupiter06 throwaway) + 로컬 psmux 세션 스위치가 **화면이 안 비고**
  올바른 세션으로 in-place 전환되는지 사람 눈으로 확인(메모리의 "switch = 사람 시각 게이트" 규율).

**(e) 파일**: `src/mux/mod.rs`, `src/mux/tmux/mod.rs`, `src/mux/psmux/mod.rs`, `src/app/runtime.rs`, `src/mux/tmux/display.rs`, `src/mux/psmux/display.rs` (+ 문서: `model/host.rs`·`model/plan.rs` 주석, 3개 AGENTS.md)

---

## 단계 8 — S3-H4: `is_remote` 과부하 → 능력 술어 `[HOT: display.rs]`

**(a) 목표**
`is_remote()`가 "다른 머신"과 "셸 경유 실행" 두 뜻을 혼동해, 로컬-이지만-셸-경유인 미래 WSL 패밀리가 3개 mux
사이트를 깨뜨리는 문제를, `Transport`의 **능력 술어** 두 개로 분리한다. `is_remote`는 (필요 시) ssh 옵션 형성용
질의로만 남긴다(LSP/ISP/명시성).

**(b) 실패 테스트 먼저**
`src/machine/mod.rs` tests에 `capability_predicates_split_shell_from_registry_scope`:
- `local(None)`: `local_registry_scope() == true`, `runs_through_shell() == false`.
- `ssh("prod",...)`: `runs_through_shell() == true`, `local_registry_scope() == false`.
- 오늘 red인 이유: `Transport::runs_through_shell`/`local_registry_scope`가 없어 컴파일 실패.

**(c) 최소 구현** (술어 기본값은 `is_remote`에서 파생하지 않음 — 혼동을 trait에 재부호화하지 않으려고 보수적 기본 + 패밀리별 override)
- `src/machine/mod.rs` `Transport` trait에 추가(문서화 포함):
  - `fn runs_through_shell(&self) -> bool { false }` — mux 바이너리를 직접 spawn하는 로컬 머신 기본 false.
  - `fn local_registry_scope(&self) -> bool { false }` — 이 박스의 로컬 mux 레지스트리(`~/.psmux`)를 이 호스트의 세션
    권위로 삼지 않음이 기본.
  - `impl Transport for Box<dyn Transport>`(현 `66-92`)에 두 메서드 위임 추가.
- `src/machine/local.rs` `impl Transport for Local`: `fn local_registry_scope(&self) -> bool { true }` override(로컬은
  이 박스의 레지스트리가 권위·list-clients 프로브 가능). `runs_through_shell`은 기본 false 사용.
- `src/machine/ssh.rs` `impl Transport for Ssh`: `fn runs_through_shell(&self) -> bool { true }` override(원격은 셸 경유).
  `local_registry_scope`은 기본 false 사용.
  - **결과**(현 local/ssh 동작과 바이트 동일): Local=`{shell:false, registry:true}`, Ssh=`{shell:true, registry:false}` —
    현 `is_remote`(local=false/ssh=true)로 얻던 분기를 정확히 재현. 미래 WSL은 `{shell:true, registry:false}`로 새 조합을 override.
- **3개 mux 사이트 교체** (`is_remote()` → 명명 술어):
  - `src/mux/psmux/mod.rs:51` `if transport.is_remote()`(원격→generic enumerate) → `if !transport.local_registry_scope()` (로컬 레지스트리 스코프일 때만 registry-merge, 아니면 generic list-sessions).
  - `src/mux/tmux/display.rs:238` `if host.transport.is_remote()`(원격→tty 기록) → `if host.transport.runs_through_shell()` (셸 경유 어태치만 `tty >file` 프리픽스). *(단계 7이 같은 `with_display_tty_record`에서 프리픽스 소스를 `record_prefix`로 이미 바꿨으므로 여기선 게이트만 교체.)*
  - `src/mux/psmux/display.rs:156` `let remote = host.transport.is_remote(); … if !remote { spawn_local_psmux_tty_capture(...) }` → `if host.transport.local_registry_scope() { spawn_local_psmux_tty_capture(...) }` (로컬 스코프에서만 default-socket list-clients 프로브).
- **문서 동기(AS-IS)**: `src/machine/AGENTS.md:38`("is_remote()가 transport 종류의 유일 질의")와 `src/mux/tmux/AGENTS.md:63`("host.transport.is_remote()") 서술을 능력 술어로 갱신.

**(d) 검증**
`cargo test -p xmux` red→green. **행동 동일성 회귀 게이트**: 3개 사이트의 기존 테스트가 그대로 green이어야 함 —
`remote_psmux_enumerates_via_list_sessions_no_local_registry`·`remote_psmux_ssh_failure_is_error_not_empty`·
`local_psmux_swallows_error_into_registry_merge`(psmux/mod.rs), `remote_shared_attach_records_its_display_tty`·
`local_shared_attach_is_not_prefixed`(tmux/display.rs), `parse_psmux_client_tty_correlates_the_client_by_session`.
`cargo clippy`/`fmt` 클린. 자동 테스트 충분(기본값이 현 동작 재현 → 로컬/원격 바이트 동일). 단계 7 라이브 스모크에
enumerate/기록/프로브 경로가 함께 실려 실기 확인 가능.

**(e) 파일**: `src/machine/mod.rs`, `src/machine/local.rs`, `src/machine/ssh.rs`, `src/mux/psmux/mod.rs`, `src/mux/tmux/display.rs`, `src/mux/psmux/display.rs` (+ 문서: `machine/AGENTS.md`, `mux/tmux/AGENTS.md`)

---

## P1 완료 기준

- `cargo test -p xmux`·`cargo clippy -- -D warnings`·`cargo fmt --check` 클린 — **585 tests 유지 또는 상회**(각 단계
  신규 테스트만큼 증가; 회귀 0).
- 8개 단계의 신규 테스트가 각기 red-first로 확인된 뒤 전부 green. success/happy 경로는 바이트 동일 — 오직 trait
  표면 슬림화·와이어 캡슐화·능력 술어·단일 팩토리로만 구조가 바뀌고 실행 결과는 불변.
- **HOT 규율**: 단계 6·7·8은 타 세션의 `*::boxed` 마이그레이션이 병합된 뒤 rebase하고 착수. 실행 헬퍼는
  `driver.rs`(HOT) 대신 `runtime.rs`에 두어 HOT 표면을 넓히지 않음.
- **라이브 게이트(사람)**: 단계 7(그리고 함께 실을 수 있는 8·4의 enumerate/tty 경로) — 원격 tmux 스위치 +
  로컬 psmux 스위치가 화면 공백 없이 in-place 착지하는지 실기 확인. 나머지 단계는 자동 테스트로 충분.
- **AS-IS 문서**: 능력 술어·`switch_in_place`·`ControlProtocol` 이관으로 stale해진 주석/AGENTS.md를 각 단계 안에서
  갱신(변경 이력 서술 금지, 현재 사실만). `CONTEXT.md`의 넓은 문서 스윕은 P6 소유.
- **불변식 게이트(스펙 §6)**: (1) 모든 `Mux`/`Transport` impl의 LSP 계약 보존(로컬 psmux `enumerate`는 `Err` 없음
  = 문서화된 per-transport 보장). (2) trait 분할 없음(단일 `Mux`, 기본 구현 포함). (3) supervisor 무지: `app`/`ui`/`state`에
  mux/machine 분기 0(동작은 `driver_for`/`host.mux`/`host.transport` 경유). (4) 경계 Fail-Fast 유지.

---

## 북극성 가산성 체크리스트 (모든 단계의 수용 테스트)

P1 완료 후 아래 두 시나리오가 **새 파일 + 단일 사이트 arm/엔트리**만으로 성립해야 한다. 각 단계가 이 표를 향해 수렴한다.

### A. tmux 호환 mux 패밀리 추가 (예: `foomux`)
- [ ] 새 `src/mux/foomux/{mod.rs,display.rs}` 디렉토리.
- [ ] `known_muxes()`(mux/mod.rs)에 엔트리 **1줄** 추가(`MuxKind { name:"foomux", make:… }`).
- [ ] **구현 필수**: `kind`/`bin`/`server_model`/`driver`/`enumerate`/`attach_plan`/`control_argv`/`death_signal`/
      `event_source` + `clone_box`(자기 복제 1줄, `Transport::clone_box`와 대칭).
- [ ] **공짜(기본 구현)**: 9개 command-plan 동사(S4-H1), `switch_in_place`(기본 None → in-place 미지원이면 그대로),
      `control_protocol`(기본 None), `poll_once`(기본 스윕).
- [ ] **선택 구현**: `switch_in_place`(→ `SwitchPlan` 반환, 드라이버가 blind 실행), `control_protocol`(→ `ControlProtocol`
      구현으로 자신의 `list-clients`/list-sessions 와이어 제공, S4-H2), `poll_once` override.
- [ ] **수정 0**: `host/`·`app/`·`state/`·`ui/`·`driver.rs`. tmux 폴백/`is_recognized`는 자동 반영(S4-L3, `known_muxes` SSOT).
- [ ] `run_poll`은 `for_kind` 재구성이 아니라 실제 mux를 `clone_box`로 받으므로 새 패밀리의 poll 동작이 유실 없이 전달(S4-L2).

### B. WSL 머신 패밀리 추가
- [ ] 새 `src/machine/wsl.rs`가 `Transport` 구현 — 특히 능력 술어를 새 조합으로 override:
      `runs_through_shell() == true`(WSL 어태치는 셸 경유), `local_registry_scope() == false`(WSL측 자체 레지스트리),
      `is_remote()`는 형편에 맞게(대개 false).
- [ ] `machine::wsl(distro)` 팩토리 함수.
- [ ] `MachineKind::Wsl { distro }` 변이체 + `MachineKind::transport()`의 **arm 1줄**(유일 선택 사이트).
- [ ] **수정 0**: machine-kind에 대한 다른 `match`/`if`(3개 mux 사이트는 능력 술어를 읽으므로 `wsl.rs` 구현만으로 올바르게 분기, S3-H4). `mux/`·`host/`·`app/`·`state/`·`ui/` 수정 0.
- [ ] config→`MachineKind` 매핑(어느 호스트가 WSL인지)은 **config 레이어 몫**(예상됨) — P1의 단일 팩토리는
      kind→transport **선택**만 단일화하며, kind **생성**은 `Hosts::build`/config가 담당(전이적 Source 정리는 P2).

---

## 설계 선택 (구현 전 확정 사항 — 리뷰 포인트)

1. **`SwitchPlan` 모양** = 2-변이체 enum: `Exec(Vec<Vec<String>>)`(비대화 exec 경로로 순차 실행할 mux argv들 —
   psmux switch-client + refresh-client) / `Shell(String)`(호스트 셸 raw 명령 — tmux 기록-tty read-back). 변이체는
   transport측 `LoweredSwitch::{Local,RawSsh}`와 1:1로 내려가고, 드라이버 실행 헬퍼는 mux 타입을 이름 붙이지 않는다.
   `switch_in_place` 시그니처의 `display_tty: Option<&str>` — psmux는 캡처한 tty를 쓰고(없으면 `None` 반환→reattach),
   tmux는 무시(파일에서 읽음). tmux 로컬은 `raw_ssh_argv==None`으로 자연히 reattach 폴백.
2. **능력 술어 이름** = `runs_through_shell()`(셸 경유 어태치 = tty 기록 여부) + `local_registry_scope()`(이 박스의 로컬
   mux 레지스트리가 이 호스트의 권위 = registry-merge/local list-clients 프로브 여부). 기본값은 보수적(둘 다 false)로
   두고 Local이 `local_registry_scope=true`, Ssh가 `runs_through_shell=true`만 override — `is_remote` 파생 기본을 쓰지
   않아 혼동을 trait에 재부호화하지 않는다. `is_remote`는 잔존(테스트/향후 ssh-옵션 형성).
3. **`MachineKind` 모양** = 자기완결 param-carrying enum(`Local{socket}` / `Ssh{alias,control_path,os}` / 미래 `Wsl{distro}`),
   `transport(self)`가 유일 match. `Source.remote`+`control_path`/`os`/`socket`를 `kind`로 접고 `alias`/`binary`/`runner`만
   남김. **전이적 중복**(`Source.alias` ↔ `Ssh.alias`, 로컬 os 출처)은 인지된 P1 wart로, Source 축소·`transport_for_source`/
   `Source::transport` 완전 삭제가 P2(레지스트리 통합)에서 해소한다 — P1은 kind→transport 선택 match를 한 곳으로 모으는 데 그친다.
4. **`Mux::clone_box` vs `Arc<dyn Mux>`** = `clone_box` 채택(기존 `Transport::clone_box`와 대칭, `Host.mux: Box<dyn Mux>`
   타입/`for_binary` 반환 타입 변경 없이 ripple 최소). `Arc<dyn Mux>`는 반환 타입 파급이 커 기각.
5. **실행 헬퍼 위치** = `run_switch_plan`을 `driver.rs`(HOT)가 아니라 `runtime.rs`의 `run_lowered` 옆에 둠 —
   두 display.rs가 이미 `crate::app::runtime`를 import하고, HOT 파일 표면을 넓히지 않기 위함(단, 개념상 `lower_select_window`
   와 함께 `driver.rs`에 두는 대안도 성립).
