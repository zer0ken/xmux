# P2 구현 계획 — 단일 호스트 레지스트리 (DRY · SoC · DIP)

> **상태: 설계(미구현).** xmux 이상적 구조 리팩토링 · 2026-07-02 · 브랜치 `refactor/ideal-structure`
> (워크트리 `.claude/worktrees/refactor-ideal/`, 베이스 `cf5e574`, HEAD `63d36f7` = cf5e574 + P0 + P1).
> 마스터 스펙: `docs/superpowers/specs/2026-07-02-ideal-structure-refactor-design.html`
> (§1 북극성 · §2 확정 결정 · §5 P2 표 · §6 불변식). CONTEXT.md "Improvement Notes"(184–215)가 이 단계의 방향을 명시한다.
> 선행: P1 완료(`ba844f5`..`63d36f7`). **베이스라인: 589 tests / clippy0 / fmt0.**
> 경로는 repo-상대. 모든 `file:line` 앵커는 **이 워크트리의 실코드**(0818679 감사 → cf5e574 병합 → P1 편집으로
> 이중 이동)에서 **재확인한 값**이며, 구현 중 심볼/패턴으로 다시 확인한다(**LSP는 신뢰 말 것**).
> TDD 규율: 각 단계 = 실패 테스트(red, 이유 명시) → 최소 구현 → green + 회귀 0.

---

## ⚠ 이 단계의 위험 프로필 (반드시 먼저 읽을 것)

- **P2는 로드맵에서 가장 크고 위험한 단계다.** 호스트 열거·스캔·attach·switch의 라이브 경로를 재배선한다.
  자동 테스트가 argv/상태 동등성은 잡지만, **화면 착지(attach/switch가 안 비고 올바른 세션으로 전환)는
  사람 라이브 게이트**로만 확인된다(스펙 §4 P2, §7). 아래에서 각 단계를 **자동-테스트-충분** / **라이브-게이트-필요**로 표시.
- **HOT 조율 불필요.** boxed-migration stash(타 세션의 `*::boxed` 마이그레이션)는 publish 결정에 따라 폐기되므로,
  P2는 `driver.rs`/`mux/tmux/display.rs`/`mux/psmux/display.rs`에 대한 rebase/조율 의존이 **없다**. P2가 만지는
  `src/display/attachment.rs`는 HOT 목록(스펙 §7)에 없다.
- **행동 가시 변화가 있는 단계 = 없음(성공 경로 바이트 동일이 목표)** — 단 시그니처 변화(예: `spawn_attachment`가
  env-clear 키를 인자로 받음, `HostManager::ensure`에서 `_src` 제거)는 컴파일 표면 변화지 런타임 출력 변화가 아니다.
  유일하게 런타임 로그 문구가 바뀔 수 있는 곳은 S3-M1의 enumerate 실패 경로(아래 명시).

---

## 확정 방향 (CONTEXT.md Improvement Notes 184–215 + 스펙 §2)

- **`Hosts`가 유일 런타임 레지스트리.** 이벤트 루프의 render/host-orchestration은 `Hosts::ids()`/`hosts.get()` 하나만
  읽는다. `Env{srcs, by_alias}`의 **루프 내 병렬 threading을 제거**한다.
- **`Env`는 config/CLI 조립 계층으로 남는다.** off-loop `Ops`(manage 라이프사이클)와 CLI(`ls`/`attach`/`doctor`)의
  소스 데이터 조립은 `Env`/config 몫이며, 이는 "루프에 병렬로 흐르는 런타임 레지스트리"가 아니다.
- **라이브 프로세스/태스크 소유는 `model::Host`에 넣지 않는다.** `host::HostManager`가 control 클라이언트·poll 태스크를
  계속 소유한다(스펙 §1 host 계층).
- **`Source`는 얇은 어댑터로 축소하거나 소멸.** 새 실행 의미는 `Transport`/`Mux`에. manage·enumerate·attach를
  `Host + Mux + Transport`로 포팅한 뒤 `Source::transport`/`Source::list_sessions`를 삭제(스펙 §5 S3-H2/M1/M3).
- **가산성 불변**(스펙 §1 북극성): P2 후에도 WSL 머신 / tmux-호환 mux 추가는 순수 가산(새 파일 + 단일 사이트)이어야 한다.

---

## 서브배치 분할 (checkpoint 가능 단위)

P2는 **독립 배포 가능한 3개 하위 배치**로 쪼갠다. 각 배치 끝에서 `cargo test`/clippy/fmt 그린 + (B·C는) 라이브 게이트를
통과하면 그 지점에서 안전하게 체크포인트/ff-merge 할 수 있다.

| 배치 | 내용 | 위험 | 게이트 |
|---|---|---|---|
| **A** (안전 이동) | S3-M2(`is_mux_var`→vocab + dead `mux_clean_env` 삭제 + display env-strip 역전), S3-L3(Liveness 수렴, Low) | 낮음 · 바이트 동일 | 자동 테스트 충분 |
| **B** (레지스트리 통합) | S3-H3(`_src` 제거) → S3-H1(render/orchestration → `Hosts`) → S3-H2+M1+M3(Source 축소: enumerate/manage/attach를 Host로, `Source::transport`/`list_sessions`/`transport_for_source` 삭제) | **높음** · 라이브 경로 재배선 | **라이브 게이트**(B3 끝) |
| **C** (inventory 소유) | S4-M5(control-reader inventory를 `HostEvent`로 실어 `model::Host.inventory` 단일 소유로; `HostClient.inventory`+`ReaderState.inventory` 삭제) | 중~높음 · 가장 미묘 | **라이브 게이트**(control host 트리) |

**배치 간 독립성**: A는 B/C와 완전 독립(먼저/병렬 가능). B는 A와 독립이나 B 내부는 강한 순서 의존(B1→B2→B3). C는 B와
대체로 독립(C는 메타데이터 inventory 흐름, B는 호스트 레지스트리 threading)이나, **C를 B 뒤에 두면** `model::Host.inventory`가
B3의 `Host::enumerate`(poll/scan 경로)와 control-reader(C) 양쪽 소유로 자연히 수렴하므로 순서를 **A → B → C**로 확정한다.

---

## 단계 순서 & 의존 그래프

```
A1 (is_mux_var→vocab, dead code)  ─┐
A2 (display env-strip 역전)        ─┼─ 배치 A: 독립·병렬 가능·바이트 동일
A3 (Liveness 수렴, Low/optional)  ─┘

B1 (_src 제거: ensure/rescan)  →  B2 (render loop → Hosts)  →  B3 (Source 축소)   ← 배치 B: 강한 순서 의존
                                                                    │
                                                                    └─ 라이브 게이트

C1 (S4-M5 inventory 단일 소유)   ← 배치 C: B 뒤 · 라이브 게이트
```

- **B1은 B2의 전제**: `ensure`/`rescan`이 `_src`를 버려야 render 루프가 `env.by_alias`를 읽지 않게 된다.
- **B2는 B3의 전제**: B2가 `spawn_host_detection`을 host-clone 기반으로 바꾸면 `transport_for_source`가 미사용이 되어
  B3에서 삭제 가능. B2가 render 루프의 `env.srcs`/`by_alias`를 걷어낸 뒤라야 `Source` 축소가 off-loop/CLI 영역으로 국한된다.
- **B3는 P2 위험의 집결지**: enumerate/manage/attach를 `Source`에서 `Host`로 포팅 + CLI(`ls`/`attach`/`doctor`) 재배선.
  내부적으로 다시 쪼갤 수 있다(B3a enumerate → B3b manage → B3c attach → B3d `Source` 필드/메서드 삭제). 한 증분이
  너무 커지면 B3a/b/c 각각에서 체크포인트.

---

## 배치 A — 안전 이동 (바이트 동일 · 자동-테스트-충분)

### 단계 A1 — S3-M2(1/2): `is_mux_var`를 `mux/vocab.rs`로 + 죽은 `mux_clean_env` 삭제

**(a) 목표**
mux env 어휘(`is_mux_var`)를 소유 계층인 `mux/vocab.rs`로 옮겨(mux 어휘의 SSOT), `source.rs`와 `display/`가
"어느 env 키가 mux var인가"를 직접 판정하지 않게 한다(응집도/SoC). 프로덕션 호출부 0개인 죽은 `mux_clean_env`
(source.rs:85, 호출부 없음 — 정의 + 테스트 2개뿐)를 삭제한다(dead-code).

**(b) 실패 테스트 먼저**
`src/mux/vocab.rs`의 `mod tests`에 `is_mux_var_matches_exactly_tmux_and_psmux_markers` 추가(source.rs의
`is_mux_var_is_precise`를 이관):
- 단언: `assert!(is_mux_var("TMUX")); assert!(is_mux_var("TMUX_PANE")); assert!(is_mux_var("PSMUX_SESSION"));`
  `assert!(!is_mux_var("TMUXP_LAYOUT")); assert!(!is_mux_var("TMUX_TMPDIR")); assert!(!is_mux_var("PATH"));`
- 오늘 red인 이유: `crate::mux::vocab::is_mux_var`가 없어 **컴파일 실패**(경로 미해결).

**(c) 최소 구현** (심볼로 재탐색)
- `src/mux/vocab.rs`에 `pub fn is_mux_var(key: &str) -> bool { matches!(key, "TMUX" | "TMUX_PANE") || key.starts_with("PSMUX") }`
  추가(source.rs:77–82 본문 그대로 이식, 독스트링 포함).
- `src/source.rs`:
  - `is_mux_var`(77–82) **삭제**. `ExecRunner::run`의 필터(58) `if !is_mux_var(&k)` → `if !crate::mux::vocab::is_mux_var(&k)`.
  - `mux_clean_env`(85–90) **삭제** + 테스트 `mux_clean_env_keeps_lookalike_vars`(375)·`mux_clean_env_strips_mux_vars`(387)
    **삭제**. `is_mux_var_is_precise`(363)는 (b)로 이관됐으므로 삭제.
  - 모듈 헤더 독스트링(4행 `is_mux_var`/`mux_clean_env` 언급)에서 두 이름을 제거(현재 사실만: mux env 어휘는 `mux/vocab`).
- `src/display/attachment.rs`(276) `crate::source::is_mux_var(&k)` → 단계 A2가 이 호출부 자체를 없애므로 **여기서는 경로만**
  `crate::mux::vocab::is_mux_var`로 바꾼다(A2가 뒤이어 인자화). *(A1·A2를 인접 배치해 이 파일을 두 번 지나지 않게 함.)*

**(d) 검증**
`cargo test -p xmux` red→green. 기존 그린 유지: `env.rs`의 `list_sessions_probes_one_source`(ExecRunner 미경유이므로
무관하지만 컴파일 확인), `source.rs`의 나머지(`interactive_attach_*`, `list_sessions_*`, `build_puts_local_first`).
`display::attachment` 테스트(`scan_marker_once_*`) 그린. `grep -rn "is_mux_var\|mux_clean_env" src/source.rs` 결과 0(이관
완결 확인). `cargo clippy -- -D warnings`(미사용 함수 경고 소멸)/`cargo fmt --check` 클린. **자동 테스트 충분.**

**(e) 파일**: `src/mux/vocab.rs`, `src/source.rs`, `src/display/attachment.rs`

---

### 단계 A2 — S5-3 (S3-M2 2/2): display env-strip 역전 — mux 어휘 주입

**(a) 목표**
`display/attachment.rs::spawn_attachment`이 `std::env::vars()`를 훑어 `is_mux_var`로 mux var를 판정·제거하는
상향 의존(display → mux 어휘)을 역전시킨다. **호출부(mux/argv를 조립하는 쪽)가 제거할 env 키 목록을 공급**하고,
`spawn_attachment`는 넘겨받은 키만 지운다 — display는 어떤 mux var도 이름 붙이지 않는다(SoC/DIP/응집도).

**(b) 실패 테스트 먼저**
`src/display/attachment.rs`의 `mod tests`에 순수 단위 테스트 `spawn_attachment_clears_only_the_supplied_env_keys`
— 실제 PTY spawn 없이 "키 선택" 로직만 검증 가능한 형태로. spawn 자체는 실 PTY라 순수 테스트가 어려우므로 **키 계산
헬퍼를 분리**해 그 헬퍼를 테스트한다:
- 새 순수 헬퍼 `pub(crate) fn mux_env_keys_to_clear(vars: impl IntoIterator<Item = String>) -> Vec<String>`를
  **호출부 쪽(예: `display/worker.rs` 또는 attach 코디네이터)**에 두거나 `mux/vocab.rs`에 두고, 테스트:
  `assert_eq!(mux_env_keys_to_clear(["TMUX=x".into(),"PATH=/b".into(),"PSMUX_SESSION=d".into()].map(|s:String| s.split('=').next().unwrap().to_string())), vec!["TMUX","PSMUX_SESSION"]);`
  (실제로는 키만 넘기므로 `["TMUX","PATH","PSMUX_SESSION"] → ["TMUX","PSMUX_SESSION"]`).
- 오늘 red인 이유: `mux_env_keys_to_clear`가 없어 컴파일 실패.

**(c) 최소 구현** (결정: 키 리스트 인자화 — "설계 선택 §1" 참조)
- `src/mux/vocab.rs`에 `pub fn mux_env_keys_to_clear(keys: impl IntoIterator<Item = String>) -> Vec<String>`
  추가: `keys.into_iter().filter(|k| is_mux_var(k)).collect()`.
- `src/display/attachment.rs` `spawn_attachment`(253) 시그니처에 `env_clear: &[String]` 파라미터 추가.
  본문의 env 순회(272–279)를 `for k in env_clear { cmd.env_remove(k); }`로 교체 — `std::env::vars()` 순회와
  `crate::source::is_mux_var`(또는 A1 후 `mux::vocab::is_mux_var`) 호출을 **display에서 완전 제거**(상향 의존 소멸).
- 호출부: `spawn_attachment`를 호출하는 곳(display 워커 / attach 스폰 경로 — `grep -rn "spawn_attachment(" src`로 재탐색,
  현재 `display/worker.rs` 계열)에서 `let env_clear = crate::mux::vocab::mux_env_keys_to_clear(std::env::vars().map(|(k,_)| k));`
  을 계산해 인자로 전달. (호출부는 attach argv를 조립하며 이미 mux를 아는 쪽이므로 어휘 주입이 자연스럽다.)
- 독스트링(248, 272–274)의 "same precise strip … source::is_mux_var" 서술을 "호출부가 공급한 mux env 키를 제거"로 갱신(AS-IS).

**(d) 검증**
`cargo test -p xmux` red→green. `spawn_attachment`를 태우는 스모크(`spawn_attachment_*`가 있으면)와 `scan_marker_once_*`
그린. `grep -rn "is_mux_var\|source::" src/display/` 결과 0(display가 mux 어휘/`source::`를 이름 붙이지 않음 확인 — 불변식
"display는 mux var를 이름 붙이지 않음"·"의존 방향" 게이트). `cargo clippy`/`fmt` 클린. **자동 테스트 충분**(env 제거 대상이
동일 집합 → 런타임 동작 바이트 동일; 시그니처만 변화).

**(e) 파일**: `src/mux/vocab.rs`, `src/display/attachment.rs`, `src/display/worker.rs`(또는 실제 `spawn_attachment` 호출부)
*(문서: `src/AGENTS.md`의 display/source 서술은 배치 끝 스윕에서 갱신; 넓은 CONTEXT.md 스윕은 P6 소유)*

---

### 단계 A3 — S3-L3: 도달성 표현을 `Liveness`로 수렴 `[Low · optional]`

**(a) 목표**
병렬 도달성 표현(`discovery::ScanResult.err: Option<String>` ↔ `model::Host.liveness: Liveness`)을 **하나의 도달성
어휘**로 수렴한다. 목표: 스캔/ls 경로도 `Liveness`(Connecting/Live/Unreachable)로 도달성을 표현하고, 실패 메시지는
그 곁에 둔다.

**(b) 실패 테스트 먼저**
`src/discovery.rs`(또는 `model`)에 `scan_result_projects_liveness`:
- `ScanResult{err:None,..}.liveness() == Liveness::Live`; `ScanResult{err:Some("boom".into()),..}.liveness() == Liveness::Unreachable`.
- 오늘 red인 이유: `ScanResult::liveness()`(또는 자유 함수)가 없어 컴파일 실패.

**(c) 최소 구현** (결정: 투영 헬퍼 — "설계 선택 §2" 참조; `Liveness`는 `Copy`라 메시지를 담지 않음)
- `src/model/host.rs`의 `Liveness`에 `pub fn from_scan_err(err: &Option<String>) -> Liveness { if err.is_some() { Liveness::Unreachable } else { Liveness::Live } }` 추가(연결-중 상태는 스캔 결과에 없으므로 2-분기).
- `src/discovery.rs` `ScanResult`에 `pub fn liveness(&self) -> Liveness { Liveness::from_scan_err(&self.err) }` 추가.
  **`err` 필드는 유지**(ls 출력 `(unreachable: <msg>)`가 메시지를 필요로 함 — Low 항목이므로 완전 병합은 하지 않는다).
- (선택) `env.rs::ls_lines`가 `g.err`가 아니라 `g.liveness()==Unreachable`로 분기하도록 조정해 도달성 판정을 한 어휘로.

**(d) 검증**
`cargo test -p xmux` red→green. `ls_lines_*`·`to_groups_sorts_sessions_by_recency`·`scan_all_*` 그린. `cargo clippy`/`fmt`
클린. **자동 테스트 충분.**

> **범위 주의**: `Liveness`를 `Unreachable(String)`으로 만들어 메시지까지 완전 병합하는 것은 `Liveness: Copy` 파기 →
> `model::Host`/`Hosts` 전반 ripple이므로 **P2 범위 밖**(Low 대비 비용 과다). A3는 "도달성 판정을 한 어휘로"까지만; 완전
> 병합이 필요하면 별도 단계로 승격. **A3는 optional — B/C 위험에 집중하려면 생략하고 P6 문서 정리로 넘겨도 무방.**

**(e) 파일**: `src/model/host.rs`, `src/discovery.rs`, (선택) `src/env.rs`

---

## 배치 B — 레지스트리 통합 (높은 churn · 라이브 게이트)

### 단계 B1 — S3-H3: `HostManager::ensure`/`rescan`의 죽은 `_src: &Source` 제거

**(a) 목표**
`ensure`(host/mod.rs:718)의 파라미터 `_src: &crate::source::Source`(725)는 **이미 죽은 인자**(독스트링 722–724가
"the source is no longer read here"라 명시; control argv는 `host`에서 조립됨). 이를 제거하고, `rescan`(778)이
forwarding하던 `src`도 제거해, 짝지어진 `env`+`hosts` 호출 체인을 축약한다(결합도/YAGNI/명시성). B2가 render 루프에서
`env.by_alias`를 걷어낼 수 있게 하는 **전제**.

**(b) 실패 테스트 먼저**
`src/host/mod.rs`의 `#[cfg(test)] impl HostManager` 테스트에 `ensure_needs_no_source_arg`(기존 ensure/reap 테스트 시그니처
갱신 형태):
- `mgr.ensure("local", &host, cols, rows)`를 **`&src` 없이** 호출하고 `Ok(true)`(첫 ensure) / 재호출 `Ok(false)`(idempotent).
- 오늘 red인 이유: 현 시그니처가 `_src`를 요구해 인자 3개(host,src,…) 호출을 인자 없는 형태로 바꾸면 **컴파일 실패**(시그니처
  변경 유도 red).

**(c) 최소 구현**
- `src/host/mod.rs` `ensure`(718–728): 파라미터 `_src: &crate::source::Source`(725) **삭제**. 독스트링 722–724의
  "It stays in the signature because rescan forwards it…" 문단 삭제(AS-IS).
- `rescan`(778–794): 파라미터 `src: &crate::source::Source`(782) **삭제**; 본문의 `self.ensure(id, host, src, cols, rows)`(792)
  → `self.ensure(id, host, cols, rows)`.
- 호출부(runtime.rs) 인자 제거: `ensure_current_host`(583) `mgr.ensure(&id, host, src, cols, rows)` → `mgr.ensure(&id, host, cols, rows)`,
  `dispatch_detected_host`(623), `kick_rescan`(685 `mgr.rescan(&src.alias, host, src, …)` → `mgr.rescan(&src.alias, host, …)`),
  틱 재연결 스윕(2559 `mgr.ensure(&src.alias, host, src, vc, vr)`). *(이 시점엔 아직 `env.by_alias`로 `src`를 얻는 코드가 남아
  있어도 되지만, `ensure`가 `src`를 안 받으므로 그 `src` 바인딩 중 ensure-전용인 것은 미사용이 된다 → B2에서 제거.)*
- `#[cfg(test)]` 헬퍼(`insert_fake` 등 host/mod.rs:833+)와 app 테스트(runtime.rs)의 ensure 호출도 인자 제거.

**(d) 검증**
`cargo test -p xmux` red→green. host manager 테스트(ensure/reap idempotency, `reap`, `resize_all`) 그린. app 테스트의
metadata e2e(`jupiter06` 계열, host/mod.rs:923~)와 runtime 테스트 그린. `cargo clippy`(미사용 파라미터 경고 소멸)/`fmt`
클린. **자동 테스트 충분**(순수 시그니처 정리, 런타임 동작 불변).

**(e) 파일**: `src/host/mod.rs`, `src/app/runtime.rs`

---

### 단계 B2 — S3-H1: render/host-orchestration을 `Hosts`에서만 읽게 (병렬 레지스트리 threading 제거)

**(a) 목표**
이벤트 루프가 `Hosts`와 `Env{srcs, by_alias}`를 **동시에 threading**하는 이중 레지스트리를 걷어내, 루프의 render/host-orchestration이
`Hosts::ids()`/`hosts.get()` **하나만** 읽게 한다(DRY/SRP/결합도/SoC). 구체적으로: (1) 소스 반복을 `hosts.ids()`로,
(2) `env.by_alias.get()` 조회를 제거, (3) 호스트 스켈레톤 seed를 `hosts.ids()`에서, (4) `spawn_host_detection`이 `Source`가
아니라 **host의 transport/mux를 clone**해서 탐지하도록. `Env`는 off-loop `Ops`/CLI용 config 조립으로만 남는다.

**(b) 실패 테스트 먼저**
두 층:
- `src/model/hosts.rs` tests에 이미 있는 `build_*`·`ids()`가 안전망. 추가로 render-seed 계약을 못박는
  `state_seed_uses_hosts_ids`(state/mod.rs 또는 app tests): `Hosts::build(...)` 후
  `State::from_sources(hosts.ids().to_vec())`가 `hosts.ids()`와 동일 순서/집합의 소스 스켈레톤을 만든다
  (`state.groups`의 source 목록 == `hosts.ids()`).
- `spawn_host_detection`의 host-clone 경로: `detection_clones_transport_and_mux_from_host`(app tests) — 가짜 host를
  넣고 detection 태스크가 emit하는 `HostEvent::Scanned{source}`의 `source == host.id()`이며, `env` 없이 동작.
- 오늘 red인 이유: `State::from_sources`가 여전히 `env.srcs`로 호출되고(1870), `spawn_host_detection`이 `Source`를 받는
  시그니처(593)라 host-기반 호출로 바꾸면 **컴파일 실패**.

**(c) 최소 구현** (심볼로 재탐색; 이 단계가 P2 최대 churn 중 하나)
- **호스트 빌드 입력을 `env.srcs`에서 분리**: run_app(1730)의 `ssh_aliases`(1844–1849)·`local_socket_opt`(1851–1855)를
  `env.srcs` 순회가 아니라 `env`가 보유한 config/aliases에서 얻도록. 최소안: `Env`에 이미 있는 `cfg` + ssh alias 목록을
  쓴다 — `build_env`(env.rs:81)가 계산하는 `aliases`(env.rs:91)를 `Env` 필드로 노출(`pub ssh_aliases: Vec<String>`,
  config 조립 산물)하거나, `local_socket`을 `Env` 필드로 보관. 그러면 `Hosts::build(&env.cfg, &env.ssh_aliases, os, &env.xmux_dir, env.local_socket.clone())`.
- **State seed**: `State::from_sources(env.srcs.iter().map(|s| s.alias.clone()).collect())`(1870) →
  `State::from_sources(hosts.ids().to_vec())`. `hosts`는 1856에서 이미 빌드됨 → 순서 동일(local first).
- **detection이 host에서 clone**: `spawn_host_detection`(593)을 `fn spawn_host_detection(source: String, transport: Box<dyn Transport>, mux: Box<dyn Mux>, tx: …)`로 바꾸고 내부 `crate::model::Host::new(transport, mux)` 후 `detect_and_correct`.
  호출부 `scan_or_dispatch_host`(640–643)에서 `env.by_alias.get(source)`가 아니라 `hosts.get(source)`로 host를 얻어
  `spawn_host_detection(source.into(), host.transport.clone_box(), host.mux.clone_box(), mgr.events())` 호출.
  `transport_for_source`(589)는 이제 미사용(B3에서 삭제 예정 — 여기선 남겨두거나 `#[allow(dead_code)]` 없이 즉시 삭제 가능;
  단 `Source::transport`가 아직 다른 곳에서 쓰이므로 함수만 삭제).
- **`env.by_alias` 조회 제거**: `ensure_current_host`(581) `if let (Some(host), Some(src)) = (hosts.get(&id), env.by_alias.get(&id))`
  → `if let Some(host) = hosts.get(&id)`(B1이 `ensure`의 `src`를 없앴으므로 `src` 불필요). `dispatch_detected_host`(619–624)
  → `env.by_alias` 조회 삭제, `mgr.ensure(source, host, cols, rows)`만. `scan_or_dispatch_host`(636–647) → `env.by_alias`
  조회 삭제(detection이 host-clone이므로).
- **소스 반복을 `hosts.ids()`로**: `kick_rescan`(682 `for src in &env.srcs`)·`connect_all_sources`(707)·틱 재연결 스윕(2555·2573
  `for src in &env.srcs`) → `for id in hosts.ids()`(그리고 `&src.alias` → `id`, `hosts.get(id)`/`hosts.get(&src.alias)` → `hosts.get(id)`).
  주의: `hosts.ids()`는 `&self` 빌림이고 루프 안에서 `mgr.ensure(id, hosts.get(id)…)`처럼 `hosts`를 다시 불변 빌림 →
  `ids()`가 반환한 `&[String]`을 먼저 `to_vec()`로 복제해 빌림 충돌 회피(또는 인덱스 순회).
- **함수 시그니처 정리**: `ensure_current_host`/`dispatch_detected_host`/`scan_or_dispatch_host`/`kick_rescan`/`connect_all_sources`에서
  더 이상 쓰지 않는 `env: &Env` 파라미터 제거(짝지어진 env+hosts 체인 축약 — S3-H3 "collapse"의 실현). 호출부의 `&env` 인자 제거.

**(d) 검증**
`cargo test -p xmux` red→green. 회귀 게이트: `from_sources_renders_scanning_skeletons`(switcher.rs:3180),
`build_puts_local_first_then_ssh_hosts_in_order`·`build_local_socket_threads_into_the_transport`(hosts.rs), state
`apply_event_*` 전부 그린(seed 순서/집합 불변). `grep -n "env.by_alias\|env\.srcs" src/app/runtime.rs` 결과가 **off-loop/CLI
영역(EnvOps·doctor)으로만 국한**되고 render/orchestration 루프(ensure/dispatch/scan/kick/connect/틱-스윕)에는 **0**임을 확인
(불변식 "supervisor 무지" + 단일 레지스트리). `cargo clippy`(미사용 `env` 파라미터·`transport_for_source` 경고 소멸)/`fmt` 클린.
- **⚠ 라이브 게이트(사람) 권장**: host 열거/detection/재연결 스윕의 소스를 `Hosts`로 재배선하므로, 시작 시 로컬+원격 호스트가
  모두 트리에 뜨고, `r` 재스캔이 동작하는지 실기 확인(jupiter06 throwaway). argv/상태 동등성은 자동이 잡지만 host-orchestration
  타이밍은 눈으로. (B3와 함께 한 번의 라이브 세션에 실을 수 있음.)

**(e) 파일**: `src/env.rs`(필드 노출), `src/app/runtime.rs`, `src/state/mod.rs`(테스트)

---

### 단계 B3 — S3-H2 + S3-M1 + S3-M3: `Source` 축소 — enumerate/manage/attach를 `Host`로

> **P2 위험 집결지 · 라이브 게이트 필수.** 내부적으로 B3a(enumerate) → B3b(manage) → B3c(attach) → B3d(`Source` 삭제)로
> 쪼개 각 지점에서 체크포인트 가능. 각 하위 단계는 red-first 유지.

**(a) 목표**
`Source`가 여전히 소유한 실행 책임(`Source::transport()` — 호출마다 새 `Box` 할당하는 getter형 팩토리, S3-M3;
`Source::list_sessions` — enumerate, S3-M1; `Source::interactive_attach_command` — attach argv)과 `transport_for_source`
래퍼(S3-H2)를 제거하고, 그 책임을 `Host + Mux + Transport`로 포팅한다. **transport 생성은 오직 `MachineKind::transport()`
(= `Hosts::build` 소비)에서만** 일어나게 하고, `Source`를 얇은 config 어댑터로 축소하거나 소멸시킨다(DRY/DIP/SoC).

**설계 결정 (구현 전 확정 — "설계 선택 §3" 상세)**: off-loop `Ops`(manage 라이프사이클)와 CLI(`ls`/`attach`/`doctor`)는
라이브 루프의 `&mut Host`를 빌릴 수 없다(별도 태스크/프로세스). 두 선택지:
- **(선택 A, 권장) config에서 임시 `Host` 조립**: `Host`는 config에서 값으로 조립 가능(`Host::new(MachineKind{..}.transport(), for_binary(bin))`).
  off-loop/CLI 경로는 필요 시 임시 `Host`를 만들어 `Host`의 메서드(enumerate/attach/manage)를 호출한다. runner 주입성은
  `Host`에 runner-주입 변형(`enumerate_with(&dyn Runner)` 등)을 더해 확보.
- (선택 B) 라이브 루프의 dispatch 시점에 `&hosts`로부터 host 데이터(`transport.clone_box()`+`mux.clone_box()`+runner)를
  스냅샷해 op에 실어 보냄. `EnvOps`를 걷어내지만 dispatch 사이트 변경이 큼.
→ **선택 A 채택**: `Env`(config/CLI 조립)가 `Host`를 조립하는 얇은 팩토리를 제공하고, manage/enumerate/attach는 `Host`/`Mux`/`Transport`
  API로 표현. `Source` 구조체는 소멸(또는 `runner` 주입만 남긴 테스트 심으로 축소).

#### B3a — enumerate를 `Host::enumerate`로 (S3-M1)

**(b) 실패 테스트 먼저**
- `model::Host`에 runner-주입 enumerate가 필요: `src/model/host.rs` tests에 `enumerate_with_runner_fills_inventory`:
  가짜 runner(canned list-sessions)로 `host.enumerate_with(&fake).await` → `inventory.sessions` 채워지고 `liveness==Live`.
- `discovery::scan_all`이 `Source` 대신 `Host`를 받는 계약: `scan_all_preserves_order_and_content`(discovery.rs:135)를
  `Host` 기반으로 재작성(가짜 runner 주입 host).
- 오늘 red인 이유: `Host::enumerate_with`가 없고 `scan_all`이 `&[Source]`를 받아 컴파일 실패.

**(c) 최소 구현**
- `src/model/host.rs`: `Host::enumerate`(125)가 `ExecRunner`를 하드코딩 → runner 주입형으로 리팩터:
  `pub async fn enumerate_with(&mut self, runner: &dyn Runner) -> Result<(), RunError>`(본문은 현 enumerate에서 runner만
  치환), 그리고 `enumerate(&mut self)`는 `self.enumerate_with(&ExecRunner).await`로 위임. `detect_and_correct`도 동형으로
  runner 주입 가능하게(이미 `&dyn Runner` 받음).
- `src/discovery.rs`: `scan_all(srcs: &[Source], …)` → `scan_all(hosts: &mut [Host], …)` 또는 host 스냅샷을 받는 형태.
  `s.list_sessions()`(48) → `host.enumerate_with(runner).await` 후 `ScanResult{source: host.id().into(), sessions: host.inventory.sessions.clone(), err}`.
  (scan은 `&Host`를 clone 못 하므로 `&mut [Host]` 또는 임시 host들의 Vec을 조립해 넘김 — "설계 선택 §3" 선택 A.)
- `src/env.rs`: `Env::scan`(131) → `Hosts::build(...)`로 임시 hosts 조립 후 `discovery::scan_all(&mut hosts_vec, …)`.
  `EnvOps::list_sessions`(207) → 임시 `Host` 조립 후 `enumerate_with(runner)`. `EnvOps::source`(181)의 `Source` 조립은
  `Host` 조립으로.
- `src/source.rs`: `Source::list_sessions`(164) **삭제**. `reason_is_no_sessions` 재-export(174)는 유지(mux 경로가 씀).

**(d) 검증**
`cargo test -p xmux` red→green. `scan_all_*`(재작성), `list_sessions_probes_one_source`(env, host-기반 재작성),
`enumerate_*`(host.rs) 그린. `ls_lines_*` 그린(스캔 결과 형태 불변). **로그 문구 주의**: enumerate 실패의 WARN 메시지가
`run_poll`(host/mod.rs:654)과 `Host::enumerate` 경로에서 일관되게 나오는지 확인(런타임 로그가 유일하게 문구 바뀔 수 있는
지점 — 출력 데이터는 불변). **자동 테스트 충분**(단, ls 실기 확인은 라이브 게이트에 포함 권장).

#### B3b — manage를 `Host`/`Transport`로 (S3-H2 부분)

**(b) 실패 테스트 먼저**
`src/manage.rs` tests(현 `got[0].panes[0].command` 계열)를 `Host` 기반으로 재작성: `manage::create(&host, name)` 형태로
호출하고 assigned name/panes 파싱 동등. red 이유: `manage::*`가 `&Source`를 받아 `&Host` 호출 시 컴파일 실패.

**(c) 최소 구현**
- `src/manage.rs`: `run` 헬퍼(15) `s.transport().exec_argv(...)` + `s.run_with().run(...)` → `host.transport.exec_argv(...)` +
  runner. 8개 `pub async fn`(create/kill/rename/kill_window/rename_window/panes/new_window/split_window)의 `s: &Source` →
  `host: &Host`(+ runner 주입 인자 또는 `Host`가 runner 보유). mux argv는 `host.mux.<verb>_plan(...)`로(이미 mux plan 존재).
- `src/ui/ops.rs`/`src/env.rs` `EnvOps`의 manage 위임(new_session/kill/rename/…)이 임시 `Host`를 조립해 `manage::*(&host, …)` 호출.

**(d) 검증** `cargo test -p xmux` red→green. `run_op` 경로 테스트(switcher `apply_op_result` 계열), manage 파싱 테스트 그린.
**라이브 게이트**: 트리에서 new session/kill/rename/new window/split이 실제로 동작하는지 실기(jupiter06). 

#### B3c — attach argv를 `Host`로 (S3-H2 부분)

**(b) 실패 테스트 먼저** `Host::interactive_attach_command`(신규)로 `source.rs`의 `interactive_attach_*` 6개 테스트를 이관:
`host.interactive_attach_command("dev", None)` → 로컬 psmux `new-session -A -s dev`, 원격 tmux `ssh … exec tmux attach -t api`
등 동일 argv. red 이유: `Host::interactive_attach_command`가 없어 컴파일 실패.

**(c) 최소 구현**
- `src/model/host.rs`에 `pub fn interactive_attach_command(&self, name: &str, window: Option<i64>) -> Vec<String>`
  추가(source.rs:120–130 본문 이식: `self.mux.attach_plan(name)` + `self.transport.interactive_attach_argv(...)`).
- `src/cli.rs` `run_direct_attach`(134–147): `env.by_alias.get(source)` + `src.interactive_attach_command(...)` →
  config에서 임시 `Host` 조립 후 `host.interactive_attach_command(&target.name, None)`. (CLI는 config/조립 계층 — `Host` 조립
  자연스러움.)
- `src/source.rs`: `Source::interactive_attach_command`(120) **삭제** + 이관된 테스트 삭제.

**(d) 검증** `cargo test -p xmux` red→green. 이관된 attach argv 테스트(host.rs) 그린. **라이브 게이트**: `xmux <source>/<session>`
직접 attach가 로컬/원격 모두 화면에 착지하는지 실기.

#### B3d — `Source::transport` / `transport_for_source` 삭제 + `Source` 축소 (S3-H2·M3 완결)

**(c) 최소 구현**
- `src/app/runtime.rs`: `transport_for_source`(589–591) **삭제**(B2 후 미사용).
- `src/source.rs`: `Source::transport()`(143–145) **삭제**(B3a/b/c가 마지막 호출부를 없앰). `Source::local_socket()`은
  `Env`가 `Hosts::build` 입력(local_socket)을 계산하는 데 쓰이면 유지, 아니면 삭제. `Source` 필드에서 실행 관련이 모두
  빠지면 구조체를 **`runner` 주입 심 + config 값**만 남긴 얇은 형태로 축소하거나, `Env`/CLI가 `Host`를 직접 조립하면
  **완전 삭제**(스펙 "Source가 얇은 어댑터로 축소하거나 소멸"). `build`(178)·`Runner`/`ExecRunner`/`RunError`의 소유 위치
  재검토(runner는 `mux::enumerate`/`manage`가 계속 쓰므로 `source.rs` 또는 `mux`로).
- 문서(AS-IS): `src/AGENTS.md:60–64`의 `source.rs` 서술(`transport()`/`list_sessions`/`interactive_attach_command` 언급)을
  실제 남은 표면으로 갱신.

**(d) 검증** `cargo test -p xmux` 전부 green. `grep -rn "\.transport()\|transport_for_source\|list_sessions\b" src/source.rs src/app/runtime.rs`
결과가 의도대로 0(또는 `Source` 소멸). transport 생성 사이트가 `MachineKind::transport()`(machine/mod.rs:150) **한 곳뿐**임을
`grep -rn "\.transport()\|::transport(" src`로 확인(불변식: transport는 `Hosts::build`에서만 생성). `cargo clippy`/`fmt` 클린.
- **⚠ 라이브 게이트(사람) 필수(B3 종합)**: enumerate(ls·트리) + manage(new/kill/rename/split) + attach(직접·트리 선택) +
  switch가 로컬 psmux / 원격 tmux(jupiter06 throwaway 먼저, 그다음 사용자 실서버) 양쪽에서 **화면 공백 없이** 올바른 세션에
  착지하는지 사람 눈으로 확인(메모리 "attach/switch = 사람 시각 게이트" 규율).

**(e) 파일**: `src/model/host.rs`, `src/discovery.rs`, `src/env.rs`, `src/manage.rs`, `src/ui/ops.rs`, `src/cli.rs`,
`src/source.rs`, `src/app/runtime.rs` (+ 문서 `src/AGENTS.md`)

---

## 배치 C — inventory 단일 소유 (가장 미묘 · 라이브 게이트)

### 단계 C1 — S4-M5: control-reader inventory를 `HostEvent`로 실어 `model::Host.inventory`로 수렴

**(a) 목표**
per-host inventory가 **두 곳에 소유**된 것을 하나로 모은다. 현재:
- **Control host(tmux)**: 리더 스레드가 공유 `Arc<Mutex<HostInventory>>`(`ReaderState.inventory` host/mod.rs:128 =
  `HostClient.inventory` host/mod.rs:372)에 `sessions`/`panes`를 쓰고(258·270), `Connected`/`Inventory` 이벤트(데이터 없음)를
  emit → 루프의 `EventEffect::ApplyInventory`(runtime.rs:1017)가 `client.inventory.lock()`을 읽어 switcher에 적용.
- **Poll host(psmux)**: poll 태스크가 데이터를 **이벤트에 실어** 보냄(`HostEvent::Sessions{sessions}` / `Panes{panes}`) →
  `apply_event`(state/mod.rs:343·359)가 순수 fold로 직접 적용.

수렴: **control-reader도 poll처럼 파싱 데이터를 이벤트에 실어** 보내고, 루프가 이를 `model::Host.inventory`(host.rs:78 —
현재 런타임에서 안 읽히는 test-only 필드; B3a가 enumerate 경로에서 채우기 시작)에 fold한 뒤 그 단일 소유에서 트리에 적용한다.
`HostClient.inventory` + `ReaderState.inventory`(병렬 `Arc<Mutex>`)를 **삭제**(SRP/결합도).

**(b) 실패 테스트 먼저** — 두 층
- **리더가 데이터를 이벤트에 싣는다**: `src/host/mod.rs` tests의 `reader_resolves_list_sessions_block_into_inventory`(890)를
  재작성 — 리더가 `state.inventory`에 쓰는 대신 `HostEvent::Inventory{host, sessions}`(신규 payload)를 emit하는지 단언
  (`emit`된 이벤트의 `sessions`에 파싱된 세션이 담김). `reader_resolves_list_panes_block_into_inventory`(1403)도 동형으로
  `HostEvent::Panes{host, address, panes}`.
- **`apply_event`가 `model::Host.inventory`로 fold**: `src/state/mod.rs` 또는 app tests에 `apply_event_control_inventory_folds_into_host`:
  `Inventory{host, sessions}` → 효과가 `hosts.get(host).inventory.sessions`를 채우고 switcher에 반영(현
  `ApplyInventory{host}`가 `client.inventory`를 읽던 것을 대체).
- 오늘 red인 이유: `HostEvent::Inventory`/`Connected`가 payload 없이 정의(host/mod.rs:57–60)돼 있고 `ApplyInventory`가
  `client.inventory`를 읽으므로, payload/host-fold를 요구하는 단언이 **컴파일/논리 실패**.

**(c) 최소 구현** (결정: 이벤트가 데이터 운반 + 루프가 `model::Host.inventory`에 fold — "설계 선택 §4")
- **HostEvent payload**: `HostEvent::Inventory`(60)에 `sessions: Vec<Session>` 추가(또는 리더가 `Sessions`처럼 새 carrier를
  emit). `Connected`(58)는 "첫 연결" 신호로 유지(데이터는 뒤따르는 Inventory가 운반) — 또는 `Connected`에도 sessions를 실어
  단일화. panes는 별도 carrier(`HostEvent::Panes`는 이미 poll이 씀; control도 이를 재사용하되 `host`/`address` 포함).
- **리더**: `resolve_block`(246)의 `ListSessions` 팔(255–266) — `state.inventory.lock()...sessions = sessions`(258) **삭제**,
  `emit(HostEvent::Inventory{host, sessions})`. `ListPanes` 팔(267–274) — `state.inventory.lock()...panes.insert`(270) 삭제,
  `emit(HostEvent::Panes{host, address, panes})`. `ReaderState`(127–130)에서 `inventory` 필드 **삭제**(→ `connecting`만 남음;
  `ReaderState`가 `connecting`뿐이면 구조체 축소/제거 검토).
- **HostClient**: `inventory: Arc<Mutex<HostInventory>>`(372) 필드 **삭제**. `spawn`(395~)에서 inventory 생성/clone(450·458·492)
  삭제. `HostInventory` 타입(17)은 `model::Host.inventory`가 계속 쓰므로 유지(소유만 이동).
- **apply_event / 루프**: `state/mod.rs` `apply_event`의 `Connected|Inventory`(294–299) 팔이 sessions를 실은 효과를 반환하도록
  변경 — 순수 fold는 `hosts`에 접근 못 하므로, **데이터를 실은 `EventEffect::ApplyInventory{host, sessions}`**를 반환하고,
  루프(`run_event_effect` runtime.rs:1017, `&mut hosts` 보유)가 `hosts.get_mut(host).inventory.sessions = sessions` fold 후
  `switcher.apply_source_result(host, sessions, None, state)`. `client.inventory.lock()` 읽기(1020–1031) **삭제**.
  panes 적용(1023–1025 `inv.panes.iter()`)은 `HostEvent::Panes`가 `apply_event`(state/mod.rs:359)에서 이미 순수 처리하므로
  control panes도 그 경로로 흐르게 통일.
- **문서(AS-IS)**: `host/AGENTS.md`("The app reads the inventory to (re)build the tree" — control이 공유 inventory를 읽는다는
  서술)와 CONTEXT.md Improvement Notes 192–197(per-host metadata ownership 미정/`control` 슬롯 stale)을 "inventory 단일 소유 =
  `model::Host.inventory`, 이벤트가 운반"으로 갱신(넓은 CONTEXT 스윕은 P6이나 이 stale 노트는 여기서 정정 — S4-L5와 연계).

**(d) 검증**
`cargo test -p xmux` red→green. 회귀 게이트: `reader_resolves_list_sessions_block_into_inventory`·
`reader_resolves_list_panes_block_into_inventory`(재작성), metadata e2e(`jupiter06` connect→list-sessions→inventory,
host/mod.rs:923~ — 이제 이벤트 payload로 검증), `apply_event_connected_marks_connected_and_emits_apply_inventory`(298)·
`apply_event_sessions_applies_tree_and_emits_sync_on_success`(state, poll 경로 불변)·`apply_event_panes_loads_subtree`(359).
`inventory_starts_empty`(host/mod.rs:863)는 `model::Host` inventory 기준으로 이동. `grep -rn "client.inventory\|ReaderState" src`
로 병렬 소유 소멸 확인. `cargo clippy`/`fmt` 클린.
- **⚠ 라이브 게이트(사람) 필수**: control host(원격 tmux, jupiter06)의 트리가 connect 직후 세션/윈도우로 채워지고, `%`-change
  알림(다른 클라이언트가 세션 추가/이름변경/윈도우전환) 시 트리가 refetch로 갱신되는지 실기 확인(리더→이벤트→`model::Host.inventory`
  경로 재배선이라 argv/유닛만으로 스레드 타이밍/락 제거를 완전 보장 못 함).

**(e) 파일**: `src/host/mod.rs`, `src/state/mod.rs`, `src/model/action.rs`(`EventEffect::ApplyInventory` payload), `src/app/runtime.rs`
(+ 문서: `src/host/AGENTS.md`, `CONTEXT.md`의 stale 노트)

---

## P2 완료 기준

- `cargo test -p xmux` · `cargo clippy -- -D warnings` · `cargo fmt --check` 클린 — **589 tests 유지 또는 상회**(각 단계
  신규 테스트만큼 증가; 회귀 0). 이관/삭제된 테스트(mux_clean_env×2, source의 attach/list_sessions 계열 → host로 이동)는
  등가 이동이며 커버리지 순감 아님.
- 각 배치가 red-first로 확인된 뒤 green. **성공/happy 경로는 바이트 동일**(단일 예외: S3-M1 enumerate 실패 WARN 로그 문구가
  경로 통일로 달라질 수 있음 — 출력 데이터는 불변).
- **단일 레지스트리 불변식**: render/host-orchestration 루프가 `Hosts`만 읽는다 — `grep`로 루프 영역에 `env.by_alias`/`env.srcs` 0.
  transport 생성 사이트가 `MachineKind::transport()` 한 곳뿐.
- **의존 방향 불변식**: `display/`가 mux var/`source::`를 이름 붙이지 않는다(A2). `Source`는 config 어댑터로 축소/소멸(B3).
- **inventory 단일 소유**(C1): `model::Host.inventory`가 control+poll+scan 모두의 소유; `HostClient.inventory`/`ReaderState.inventory` 소멸.
- **라이브 게이트(사람)**: **B(B2·B3)와 C**는 자동 테스트로 화면 착지를 보장 못 하므로 실기 필수 —
  (1) 시작 시 로컬+원격 호스트 트리 표시 + `r` 재스캔, (2) `xmux ls`/직접 attach, (3) 트리에서 new/kill/rename/split,
  (4) 로컬 psmux + 원격 tmux **cross-host switch가 공백 없이 in-place 착지**, (5) control host 트리가 `%`-change에 갱신.
  **A는 자동 테스트로 충분**(라이브 불요).

---

## 설계 선택 (구현 전 확정 사항 — 리뷰 포인트)

1. **display env-strip 역전 형태** = `spawn_attachment(env_clear: &[String])` — 호출부가 `mux::vocab::mux_env_keys_to_clear`로
   계산한 **제거 대상 키 리스트**를 주입. `is_mux_var`를 프레디킷으로 넘기는 대안도 있으나, 키 리스트가 display를 mux 어휘에서
   완전히 분리(display는 `Fn`도 mux도 이름 안 붙임)하므로 채택. `is_mux_var`의 SSOT는 `mux/vocab.rs`.
2. **`Liveness` 수렴 깊이**(S3-L3, Low) = **투영 헬퍼까지만**. `Liveness`는 `Copy`라 실패 메시지를 담을 수 없고,
   `Unreachable(String)`으로 만들면 `Host`/`Hosts` 전반 ripple(Copy 파기)이라 Low 대비 과비용. `ScanResult.err`(메시지)는
   유지하되 도달성 **판정**을 `Liveness::from_scan_err`로 단일 어휘화. **A3는 optional** — 위험 집중을 위해 생략 가능(P6 흡수).
3. **off-loop/CLI의 per-host 데이터 획득**(B3의 핵심) = **선택 A: config에서 임시 `Host` 조립**. off-loop `Ops`·CLI는 라이브
   루프의 `&mut Host`를 빌릴 수 없으므로, `Host::new(MachineKind{..}.transport(), for_binary(bin))`로 임시 host를 만들어
   `Host`의 enumerate/manage/attach API를 호출. runner 주입성은 `Host::enumerate_with(&dyn Runner)` + manage의 runner 인자로
   확보(테스트 주입 유지). 선택 B(dispatch 시점 스냅샷)는 dispatch 사이트 churn이 커 기각. **이 결정이 `Source` 소멸 여부를
   좌우** — 임시 `Host` 조립이 서면 `Source` 구조체는 완전 삭제 가능; 안 서면 `runner` 심만 남긴 얇은 어댑터로 축소.
4. **S4-M5 수렴 방식** = **이벤트가 데이터 운반 + 루프가 `model::Host.inventory`에 fold**(Interpretation A, 스펙 문구 충실).
   리더가 파싱한 sessions/panes를 `HostEvent`에 실어(poll 경로와 대칭) 공유 `Arc<Mutex>`를 없애고, `apply_event`는 데이터를
   실은 `EventEffect::ApplyInventory{host, sessions}`를 반환, `&mut hosts` 보유 루프가 `model::Host.inventory`에 fold 후 트리에
   적용. **대안(Interpretation B, 경량)**: 공유 `Arc<Mutex>`만 없애고 데이터를 switcher에 직접 적용(poll처럼), `model::Host.inventory`는
   B3a의 enumerate 소유로만 두기 — "병렬 `Arc<Mutex>` 제거"는 달성하나 `model::Host.inventory`가 control 경로의 소유가 되진
   않음. **A 권장**(B3a와 합쳐 `model::Host.inventory`가 진짜 단일 소유가 됨). 구현 착수 전 A/B를 리뷰로 확정.
5. **배치 순서** = A → B → C. C를 B 뒤에 두는 근거: B3a가 `model::Host.inventory`를 enumerate(poll/scan) 소유로 만든 뒤 C가
   control 경로를 같은 필드로 합류시켜야 "단일 소유"가 완성(그 반대 순서면 C가 중간 상태에서 두 번 만짐).
6. **`Env` 잔존 범위** = config/CLI 조립 계층으로 존치(CONTEXT.md 203–208). `Env.srcs`/`by_alias`의 **루프 threading**은
   제거하되, `Env`가 off-loop `Ops`/CLI를 위해 config에서 `Host`를 조립하는 얇은 팩토리는 유지. "retire `Env.srcs`/`by_alias`"는
   **런타임 레지스트리로서의 두 필드 제거**를 뜻하며, 필요 시 `Env`는 alias 목록/local_socket 같은 config 산물만 보관.

---

## 북극성 가산성 재확인 (P2 후 수용 테스트)

P2 완료 후에도 아래가 **새 파일 + 단일 사이트**만으로 성립해야 한다(스펙 §1). P2는 오히려 이를 강화한다(레지스트리·transport
생성·inventory 소유가 각 한 곳으로 모임).

- **WSL 머신 추가**: `machine/wsl.rs` + `machine::wsl()` + `MachineKind::Wsl` arm 1줄. `Hosts::build`가 유일 transport 생성 →
  `Env`/`Source`에 machine-kind 분기 **0**(B3가 `Source`의 transport 표현을 없앴으므로). host-orchestration 루프는 `Hosts`만
  읽으므로 수정 0.
- **tmux 호환 mux 추가**: `mux/<kind>/` + `known_muxes()` 엔트리 1줄. enumerate가 `Host::enumerate`/`Mux::enumerate`로 흐르고
  (B3a), manage가 `Mux::*_plan`으로 흐르므로 `host/`·`app/`·`state/`·`ui/` 수정 0. control host inventory는 `HostEvent` +
  `model::Host.inventory`로 통일(C1)돼 새 control mux도 같은 경로 재사용.
