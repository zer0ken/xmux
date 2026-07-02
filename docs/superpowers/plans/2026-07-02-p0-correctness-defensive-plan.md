# P0 구현 계획 — 정확성 & 방어적 하드닝

> xmux 이상적 구조 리팩토링 · 2026-07-02 · 브랜치 `refactor/ideal-structure`
> 마스터 스펙: `docs/superpowers/specs/2026-07-02-ideal-structure-refactor-design.html` (§5 P0)
> 도출: ultracode 워크플로(finding별 초안 → 적대적 검증 → 종합), 15 에이전트, **7건 전원 verdict SOUND**.
> 경로는 repo-상대다. 구현은 이 브랜치의 워크트리 `.claude/worktrees/refactor-ideal/`에서 수행한다.
> TDD 규율: 각 단계는 실패 테스트(red) → 최소 구현 → green + 회귀 0.

---

7건 모두 verdict가 SOUND이며 REJECT/NEEDS_REVISION은 없다. 드롭할 항목은 없고, 검증자가 잡은 과확장도 없다(유일하게 S2-8에서 "선택적 가독성 개선" 한 건이 언급되었으나 필수 아님 — 최소 형태 유지, 아래에 병기). 실제 버그(S5-1 마커 누수, S5-5 키 삼킴)를 앞에, 저위험 방어 하드닝(S5-4/S5-2 → S2-8)을 중간에, Low-severity 진단·자문 항목(S3-L1/S3-L2)을 뒤에 배치한다. 같은 파일을 만지는 쌍(`src/ui/run.rs`: S5-4·S5-2 / `src/env.rs`: S3-L1·S3-L2)은 충돌을 줄이려 인접 배치했다.

---

## 단계 1 — S5-1: display-tty 마커 버퍼 무한 증가 (실제 버그)

**(a) 목표**
유효한 마커가 끝내 도착하지 않는(또는 빈 `$(tty)`) 어태치에서 `marker_acc`가 무한 증가하고 매 read마다 O(n²) 재스캔하는 누수를 rolling-tail 상한으로 막는다.

**(b) 실패 테스트 먼저**
`src/display/attachment.rs`의 기존 `#[cfg(test)] mod tests`에 순수 단위 테스트 `scan_marker_once_bounds_acc_when_marker_never_completes` 추가.
- 본문: 완전하지만 빈 마커 `b"\x1b]XMUX-DISPLAY-TTY:\x07"`(빈 `$(tty)` 케이스)를 한 번 먹인 뒤, `&[b'x'; 512]`(마커 없는 일반 출력)을 약 200회 반복 공급.
- 단언: `captured.is_none()` **그리고** `acc.len() <= crate::model::death::DISPLAY_TTY_MARKER_MAX`.
- 오늘 red인 이유: 상한이 없어 루프 후 `acc.len()`이 약 `20 + 200*512 ≈ 102,420` 바이트가 되어 `<= DISPLAY_TTY_MARKER_MAX`(약 147) 단언이 실패한다(동시에 O(n²) 재스캔이 돈다).

**(c) 최소 구현**
- Step 1 — `src/model/death.rs`의 `MARKER_CLOSE`(line 15) 바로 뒤에 실제 마커 길이에서 파생한 상수 추가: `pub(crate) const DISPLAY_TTY_MARKER_MAX: usize = MARKER_OPEN.len() + 128;` (`str::len()`은 const 평가 가능). 매직넘버 대신 실제 마커 포맷을 재사용해 드리프트를 막는다.
- Step 2 — `src/display/attachment.rs` `scan_marker_once`(48-57)의 성공-캡처 블록 뒤에, 아무것도 캡처하지 못한 경로에만 인접 `qtail`(322-324)과 **동일한** drain 관용구로 tail 상한 적용: `let cap = crate::model::death::DISPLAY_TTY_MARKER_MAX; if acc.len() > cap { let cut = acc.len() - cap; acc.drain(0..cut); }`. **순서는 append → parse → cap 유지**(큰 청크 안에 온전한 마커가 들어오면 자르기 전에 파싱).
- `parse_display_tty_marker`, pump 루프 구조, `scan_marker_once` 시그니처는 변경하지 않는다.

**(d) 검증**
`cargo test -p xmux scan_marker_once`로 신규 테스트 red→green 확인. 기존 `scan_marker_once_emits_our_tty_then_stops`(:549, captured 조기반환으로 cap 미실행)와 `scan_marker_once_accumulates_across_reads_until_whole`(:585, acc≈27B로 147 상한 이하) 그대로 green. `cargo test -p xmux` / `cargo clippy` / `cargo fmt --check` 클린.

**스코프 제외**: 128B 초과의 비현실적으로 긴 tty 경로는 tail 밖으로 밀려 캡처 안 됨 — 실제 controlling tty 경로는 짧으므로 허용(기존 정상 케이스 회귀 아님).

**(e) 파일**: `src/model/death.rs`, `src/display/attachment.rs`

---

## 단계 2 — S5-5: picker 키 디코더 UTF-8 선두 바이트 오판

**(a) 목표**
`from_utf8` 실패 시 추정된 `len`만큼 전진해 다음 유효 바이트를 삼키는 버그를 1바이트 resync로 고친다.

**(b) 실패 테스트 먼저**
`src/display/decode.rs`의 기존 `mod tests`에 기존 `codes()` 헬퍼를 재사용한 순수 테스트 `invalid_utf8_lead_resyncs_without_swallowing_next` 추가.
- 단언: `assert_eq!(codes(&[0x80, b'x']), vec![KeyCode::Char('x')]);` (선택: `assert_eq!(codes(&[0xff, b'a', b'b', b'c']), vec![Char('a'), Char('b'), Char('c')]);` — len=4 분기 커버).
- 오늘 red인 이유: `0x80`의 추정 `len=2`, `from_utf8([0x80,0x78])` 실패 → Ok 본문 skip → `i += 2`로 `'x'`를 삼켜 결과가 `[]`. 단언 `vec![Char('x')]`가 `[]`에 대해 실패.

**(c) 최소 구현**
`feed`의 `_ =>` 팔(80-85)에서 `if let Ok(s) = std::str::from_utf8(...) { ... } i += len;`을 다음 `match`로 교체:
```rust
match std::str::from_utf8(&self.buf[i..i + len]) {
    Ok(s) => { if let Some(c) = s.chars().next() { out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)); } i += len; }
    Err(_) => { i += 1; }
}
```
`utf8_len`(94-104)과 미완결-tail 가드(77-79)는 변경하지 않는다(유효 선두 바이트에 대한 길이는 이미 정확 → Ok 경로 바이트 동일).

**(d) 검증**
`cargo test --lib display::decode` red→green. 기존 `utf8_multibyte_char`, `csi_arrows`, `unrecognized_csi_consumed_silently` green 유지(유효 경로 불변). `cargo clippy`(2-arm match 클린).

**(e) 파일**: `src/display/decode.rs`

---

## 단계 3 — S5-4: control dispatch가 enqueue 확인 없이 ok 반환

**(a) 목표**
채널이 닫혀 명령이 버려져도 `"ok"`로 거짓 보고하는 세 fire-and-forget 팔(`Op`/`RawKey`/`RawBytes`)을 실패 시 `err:`로 고친다.

**(b) 실패 테스트 먼저**
`src/ui/run.rs`의 `mod tests`에 순수 async 테스트 `dispatch_reports_error_when_channel_closed` 추가:
```rust
let (tx, rx) = mpsc::channel::<Cmd>(8);
drop(rx);
assert!(dispatch("rescan", &tx).await.starts_with("err:"));       // Op
assert!(dispatch("raw:key down", &tx).await.starts_with("err:")); // RawKey
assert!(dispatch("raw:text hi", &tx).await.starts_with("err:"));  // RawBytes
```
- 오늘 red인 이유: 현재 팔이 `let _ = cmd_tx.send(...).await;`로 `SendError`를 삼키고 `"ok"`를 반환 → 세 단언 모두 실패.

**(c) 최소 구현**
`dispatch` 위에 3개 팔이 공유하는 private 헬퍼 추가(단일 사용 아님 → 중복 제거 목적):
```rust
fn enqueue_reply(sent: Result<(), mpsc::error::SendError<Cmd>>) -> String {
    match sent { Ok(()) => "ok".into(), Err(_) => "err: control channel closed".into() }
}
```
`Op`/`RawKey`/`RawBytes` 팔(161-172)을 각각 `... => enqueue_reply(cmd_tx.send(Cmd::...).await),` 한 줄로 교체. `Ping`/`Dump`/`Status`/`Unknown`은 손대지 않는다(Dump/Status는 이미 닫힘 처리, 스코프 밖). 기존 `err:` 접두 와이어 규약 재사용, 신규 import 없음.

**(d) 검증**
`cargo test -p xmux ui::run` red→green. 기존 `assert_eq!(dispatch(...), "ok")` 테스트(L206, L222-234)는 live `rx` 유지 → green. `cargo clippy -- -D warnings` / `cargo fmt --check` 클린.

**(e) 파일**: `src/ui/run.rs`

---

## 단계 4 — S5-2: control 인바운드 요청 라인 무제한

**(a) 목표**
응답 경로(`read_frame`/`MAX_FRAME`)에만 있던 상한을 요청 경로에도 대칭 적용해, 개행 없는 로컬 버그 클라이언트가 메모리를 무한 증가시키지 못하게 한다.

**(b) 실패 테스트 먼저**
`src/control.rs`의 `#[cfg(test)] mod tests`(이미 `Cursor`/`TokioBufReader` 사용)에 두 테스트 추가:
- `read_request_line_bounds_unterminated_input`: `Cursor::new(vec![b'x'; MAX_FRAME + 1])`(개행 없음)을 통과시켜 `read_request_line(&mut r).await.is_err()` 단언.
- `read_request_line_reads_normal_and_eof`: `b"ping\n"` → `Ok(Some("ping\n"))`, 빈 `Cursor` → `Ok(None)` 단언.
- 오늘 red인 이유: `read_request_line`이 없어 컴파일 실패(신규 함수 도입에 대한 표준 TDD red; 기존 `read_frame_oversized`/Cursor 패턴과 동형).

**(c) 최소 구현**
- `src/control.rs`의 `read_frame`(133) 옆에 `MAX_FRAME`·`frame_err`를 재사용하는 bounded reader 추가:
```rust
pub async fn read_request_line<R: AsyncBufRead + Unpin>(r: &mut R) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let mut bounded = r.take(MAX_FRAME as u64);
    let n = bounded.read_line(&mut line).await?;
    if n == 0 { return Ok(None); }
    if bounded.limit() == 0 && !line.ends_with('\n') { return Err(frame_err("request line exceeds MAX_FRAME".into())); }
    Ok(Some(line))
}
```
(`AsyncReadExt`/`AsyncBufReadExt`/`AsyncBufRead`는 23-25에 이미 import.)
- `src/ui/run.rs` `handle_conn`의 인라인 read 블록(132-136)을 `let line = match control::read_request_line(&mut buf).await { Ok(Some(line)) => line, Ok(None) | Err(_) => return, };`로 교체(EOF/오류 시 연결 종료 = 응답 경로와 대칭).
- `src/ui/run.rs` line 15의 이제 고아가 된 `AsyncBufReadExt` import 제거(`BufReader`는 유지).

**(d) 검증**
`cargo test` 신규 2건 green, 기존 request-read 경로를 건드리는 테스트 없음 → 회귀 없음. `Take<&mut R>`의 `AsyncBufRead` impl은 `cargo build`에서 컴파일 오류로 즉시 드러난다. `cargo clippy` / `cargo fmt` 클린.

**참고(심각도)**: 소켓이 owner-only(unix 0600 / Windows namespaced pipe)라 원격 악용 불가한 defense-in-depth. P0 하드닝으로 포함하되 severity는 Low.

**(e) 파일**: `src/control.rs`, `src/ui/run.rs`

---

## 단계 5 — S2-8: 루프 draw-error 무시 + 핫 루프 lock().unwrap()

**(a) 목표**
(1) 버려지던 backend write 결과를 tracing 파일 로그로 관측 가능하게 하고, (2) 핫 루프의 poison 시 panic하는 `inventory.lock().unwrap()` 두 곳을 graceful skip으로 고친다.

**(b) 실패 테스트 먼저**
`src/app/runtime.rs`의 `mod tests`에 `apply_inventory_effect_survives_poisoned_inventory_lock` 추가(기존 `fake_env_with_sources`/`HostManager::insert_fake`/`DisplayWorker::new` 패턴 재사용, runtime.rs:3542-3552 템플릿 참조).
- 셋업: `insert_fake("local")` 후 `inventory`를 clone해 별도 스레드에서 lock 잡고 `panic!`으로 poison(`assert!(inv.is_poisoned())`).
- ACT: `run_event_effect(EventEffect::ApplyInventory{host:"local".into()}, ...)`.
- 단언: `assert!(!rearm)`.
- 오늘 red인 이유: `panic=abort`가 Cargo.toml:24에서 비활성이므로, line 997 `.lock().unwrap()`이 poisoned mutex에서 panic → 테스트가 clean failure로 잡힌다.

**(c) 최소 구현** (한 파일, 6개 국소 wrap, 신규 추상화 0)
- draw(2148): `let _ = match &grid_arc {` → `if let Err(e) = match &grid_arc {` , match 종료 `};`(2194) → `} { tracing::warn!(error = %e, "term_draw_failed"); }` (양 팔 감쌈, `Ok(CompletedFrame)`는 기존대로 무시).
  - *선택적 가독성 형태(동등·비필수, 검증자 note)*: `let draw_result = match &grid_arc {…}; if let Err(e) = draw_result { tracing::warn!… }` — 둘 다 clippy 클린. 최소 형태 채택, 구현자 재량.
- clear(1905/1998/2495): 각 `let _ = term.clear();` → `if let Err(e) = term.clear() { tracing::warn!(error = %e, "term_clear_failed"); }`.
- lock(997): `{ let inv = client.inventory.lock().unwrap(); ... inv.sessions.clone() }` → `if let Ok(inv) = client.inventory.lock() { ... inv.sessions.clone() } else { return false; }` (poison 시 `return false` = 함수 기본 tail(:1094)과 동일; `inv` 스코프는 sync 전에 drop 유지).
- lock(2544): `Some(client) => client.inventory.lock().unwrap().sessions.clone(),` → `Some(client) => match client.inventory.lock() { Ok(inv) => inv.sessions.clone(), Err(_) => continue, },` (기존 `None => continue`와 동형; 가드는 match 종료 시 drop).
- 기존 defensive 패턴(`.lock().ok()` @2151/2425, attachment.rs:216, registry.rs:56)과 통일. `tracing`은 파일 전용 로거(logging.rs, stdout/stderr 미사용)라 ratatui alt-screen 오염 없음.

**(d) 검증**
`cargo test -p xmux` 신규 테스트 green(:2544는 동일 match+continue 패턴으로 커버, 별도 seam 없음). draw/clear 로깅(aspect A)은 clean한 unit seam이 없어 헬퍼 추출은 스코프 크립으로 **거부** — `cargo build` + 유도 실패 시 `xmux.log`에 `term_draw_failed`/`term_clear_failed` 확인은 human/live 게이트. success 경로 바이트 동일. `cargo clippy` / `cargo fmt` 클린.

**스코프 제외**: `term.autoresize()` @2490(finding 미지정), poison 영구성 복구(clear_poison)는 의도적 제외 — poison은 이미 스레드가 죽은 비정상 상태이므로 skip-and-degrade가 옳음.

**(e) 파일**: `src/app/runtime.rs`

---

## 단계 6 — S3-L1: home_dir() 조용한 "." 폴백

**(a) 목표**
home 미해결 시 config/`~/.xmux` 상태/소켓/로그가 조용히 CWD로 재배치되던 것을 tracing 경고로 관측 가능하게 한다(폴백 값 `"."`는 유지).

**(b) 실패 테스트 먼저**
`src/env.rs`의 `mod tests`에 순수 테스트 `home_or_cwd_flags_the_cwd_fallback` 추가.
- 단언: `home_or_cwd(Some(PathBuf::from("/home/u"))) == (PathBuf::from("/home/u"), false)` **그리고** `home_or_cwd(None) == (PathBuf::from("."), true)`.
- 오늘 red인 이유: `home_or_cwd`가 없어 컴파일 실패(기존 `local_socket` 순수-함수+단위테스트 관용구와 동형).

**(c) 최소 구현**
- `src/env.rs`에 순수 결정 헬퍼 추가: `fn home_or_cwd(home: Option<PathBuf>) -> (PathBuf, bool)` — `Some(p)→(p,false)`, `None→(PathBuf::from("."), true)`.
- `home_dir()`을 `home_or_cwd(dirs::home_dir())` 호출로 재작성; `true` 플래그면 반환 전 `tracing::warn!("could not resolve a home directory; falling back to the current directory for config, ~/.xmux state, sockets, and logs")` (완전수식 매크로, import 불요). 해석된-home 경로는 오늘과 바이트 동일.
- `fn xmux_dir_path()`(47) → `pub(crate) fn xmux_dir_path()`로 가시성 확대.
- `src/cli.rs:62-64`의 중복 silent 폴백을 `env::xmux_dir_path()` 호출로 교체(`env` 이미 import; 로그-디렉토리 해석이 단일 경고 지점을 경유).

**(d) 검증**
`cargo test`(env 모듈) red→green. 해석된-home 경로 바이트 동일 → 회귀 없음. `cargo clippy -- -D warnings`(신규 `pub(crate)` fn 클린) / `cargo fmt --check`.

**참고**: `cli.rs:62`는 `logging::init` 이전 호출이라 그 시점 `warn!`은 subscriber 없이 안전한 no-op; 실제 경고는 이후 `build_env`의 `home_dir()`가 로그에 남긴다. no-home는 실기기에 없는 degenerate 경로라 최대 3회 중복 warn 허용(dedup용 `Once`/캐시 = 새 추상화, YAGNI로 제외).

**스코프 제외(P0 owner에게 flag만, 미수정)**: `src/mux/psmux/registry.rs:15-16`, `src/model/death.rs:33-34`의 동일 silent-`"."` 패턴은 psmux 자체 `~/.psmux` substrate를 그림자로 하므로 S3-L1 앵커 밖 — 별도 결정 필요.

**(e) 파일**: `src/env.rs`, `src/cli.rs`

---

## 단계 7 — S3-L2: config mux 값 의미 검증 부재

**(a) 목표**
`local.mux="zellij"`/오타처럼 디코딩은 됐으나 아무 mux도 지칭하지 않는 값이 무경고로 tmux 동작하는 것을, 기존 `cfg_warnings`/`xmux doctor` 채널을 재사용한 값-레벨 자문 경고로 알린다.

**(b) 실패 테스트 먼저**
- `src/config.rs`의 `mod tests`에 순수 테스트 `value_warnings_flags_unrecognized_mux`: `""`/`"auto"`/`"tmux"`/`"psmux"`는 경고 0건; `local.mux="zellij"`는 `"zellij"` 포함 경고 정확히 1건; hosts `[{ssh:"prod",mux:"psmux"}, {ssh:"bad",mux:"kitty"}]`은 `"bad"`+`"kitty"` 포함 경고 정확히 1건.
- `src/mux/mod.rs`의 `mod tests`에 `is_recognized_covers_tmux_and_known_muxes`: `is_recognized("tmux") && is_recognized("psmux") && !is_recognized("zellij") && !is_recognized("")`.
- 오늘 red인 이유: `is_recognized`·`value_warnings`가 없어 컴파일 실패.

**(c) 최소 구현**
- `src/mux/mod.rs`(for_binary/for_kind 뒤, ~276): `pub fn is_recognized(name: &str) -> bool { name == "tmux" || known_muxes().iter().any(|k| k.name == name) }` — `known_muxes()`를 SSOT로 재사용(미래 3rd mux 자동 반영, "add a mux = add a file" 아키텍처 준수). `for_binary`/`for_kind` 동작은 불변(tmux 폴백 유지) — 이건 더 좁은 자문 술어일 뿐.
- `src/config.rs`(`impl Config`, local_bin 근처): `pub fn value_warnings(&self) -> Vec<String>` — (a) `!local.mux.is_empty() && local.mux != "auto" && !crate::mux::is_recognized(&local.mux)`이면 `local mux {:?} is not a recognized mux (psmux/tmux); treating it as tmux-compatible` push; (b) 각 host에 대해 `!h.mux.is_empty() && !is_recognized(&h.mux)`이면 host별 동일 형식 push. 미지 KEY는 기존 `load_verbose`가 처리하므로 값 경고만.
- `src/env.rs` `build_env`에서 `cfg_warnings`를 mut로 하고 Env 생성 전 `cfg_warnings.extend(cfg.value_warnings());`(파싱-에러 분기는 `Config::default()`라 no-op). cli.rs 변경 불요(doctor가 이미 `cfg_warnings` 순회).

**(d) 검증**
`cargo test` 신규 2건 red→green. `for_binary`/`for_kind`/`known_muxes` 불변 → 기존 `for_binary_picks_psmux_else_tmux`, `host_specs_merge` green. `cfg_warnings`를 `Vec::new()`로 직접 세팅하는 테스트 Env(env.rs:298, runtime.rs:2901/3050)는 `build_env` 우회 → 영향 없음. `cargo clippy` / `cargo fmt` 클린. 수동 스모크: `[local] mux="zellij"` 설정 후 `xmux doctor`가 경고 출력.

**스코프 제외(과확장 방지)**: `map_color`의 `Color::Reset` 폴백(ui/switcher.rs) — 잘못된 색은 즉시 눈에 보이고 무해하므로 제외. 신규 subcommand/framework/출력 채널 없음. tmux-호환 커스텀 이름/경로/대소문자 변형은 doctor-only·비치명 자문 false-positive로 허용.

**(e) 파일**: `src/mux/mod.rs`, `src/config.rs`, `src/env.rs`

---

## P0 완료 기준

- `cargo test -p xmux`, `cargo clippy -- -D warnings`, `cargo fmt --check` 모두 클린(리포지토리 baseline: 576 tests / clippy0 / fmt0 유지 또는 상회).
- 7개 단계의 신규 테스트가 각기 red-first로 확인된 뒤 전부 green이고, 기존 테스트 회귀 0건.
- 각 단계의 success/happy 경로는 바이트 동일 — 오직 이전에 침묵하던 실패·누수·거짓보고 경로에만 동작이 추가된다.
- S2-8의 draw/clear 로깅은 unit seam이 없어 `cargo build` + 유도 실패 시 `xmux.log` 확인의 human/live 게이트로 완결한다.
- **P0는 이후 구조 리팩터 단계의 안전망이다** — 정확성 버그와 방어적 하드닝을 먼저 못박아, 파일 이동·모듈 재배치가 관측되지 않은 결함 위에서 진행되지 않도록 보장한다.
