# xmux 설정 보강: 사이드바 너비 · 요소 스타일 · 키 바인딩 · 포커스 표시

날짜: 2026-06-23
브랜치: feat/rust-rewrite

## 목표

`config.toml`의 `[ui]` 영역을 확장해 사용자가 다음을 설정할 수 있게 한다.

1. **사이드바(트리) 너비** 초기값
2. **요소별 색**(레벨 색 + 포커스 강조색)
3. **키 바인딩 오버라이드**(명명 액션 → 단일 키 스펙)
4. **포커스 표시 개선** — tmux의 액티브 페인 표현(구분선 절반 색 분할 + 비활성 페인 흐림)을 모방해 트리/mux 중 어디에 포커스가 있는지 명확히 표시
5. **포커스 시 트리 자동 숨김** — mux에 포커스가 있을 때 트리(사이드바)를 숨겨 mux를 전체 폭으로 띄울지, 아니면 표시한 채로 둘지 설정

설계 원칙: 기존 `config.toml`과 완전 호환(모든 신규 키 선택적·기본값 = 현재 동작), 최소 변경, 잘못된 값은 기본값 유지 + 경고.

## 비목표

- 임의 키→액션 풀 매핑, 체인/멀티 바인딩 (명명 액션의 단일 키 리맵만)
- 요소별 fg/bg/수식자 풀 스타일 객체 (레벨 전경색 + accent만)
- 흐림 강도의 숫자(%) 노브 (tmux도 없음 — 색으로 제어)
- `window-active-style` 대응(활성 페인 별도 스타일) — 활성 페인은 자연색
- 트리 숨김의 런타임 토글 키 (설정 전용; 포커스 상태에만 연동)

## tmux 참조 동작 (조사 결과)

- **비활성 페인 흐림**: `window-style`(비활성 fg/bg) vs `window-active-style`(활성 fg/bg). tmux 2.1+. 기본 OFF(두 스타일 동일). "얼마나"는 숫자 강도가 아니라 **색 선택**으로 제어. `window-style`은 셀의 **기본색(default)에만** 적용 — 앱 명시색은 보존.
- **페인 경계 활성 표시**: `pane-border-style`/`pane-active-border-style`(색) + `pane-border-indicators`(`off`/`colour`/`arrows`/`both`). `colour`는 경계선의 일부를 활성 페인 쪽 색으로 칠해 활성 페인을 가리킨다.

xmux는 좌(트리)/우(mux) 2-페인 + 1열 세로 구분선 구조이므로, 위 모델을 다음과 같이 적응한다.

## 설정 스키마

```toml
[ui]
prefix = "C-g"            # 기존
tree-width = 48           # 신규. 초기 트리 너비. 런타임 prefix h/l 조절은 그대로. 20..100 클램프
hide-tree-on-focus = false # 신규. mux 포커스 시 트리를 숨기고 mux를 전체 폭으로 띄움. 기본 false = 표시 유지(현재 동작)

[ui.style]                # 신규. 값은 ratatui 색 문자열("green", "#5fafff", "12" 등)
host    = "yellow"
session = "green"
window  = "magenta"
pane    = "cyan"
hint    = "darkgray"
accent  = "green"         # 포커스 강조색 (활성 구분선 절반)
dim-inactive = true       # 비활성 페인 흐림 on/off (whether)
inactive-fg  = "darkgray" # 비활성 grid 기본색 셀의 전경색 (how much = 색)
inactive-bg  = ""         # 비활성 grid 기본배경 셀의 배경색 (선택, 빈 값 = 미적용)

[ui.keys]                 # 신규. 명명 액션 → 단일 키 스펙
# 트리 단일키 (switcher)
nav-up = "k"
nav-down = "j"
nav-left = "h"
nav-right = "l"
new = "n"
rename = "R"
kill = "x"
filter = "/"
refresh = "r"
help = "?"
# prefix 후행키 (cockpit, prefix 무장 후 누르는 키)
quit = "q"
focus-mux = "Right"
wider = "l"
narrower = "h"
```

모든 신규 키는 선택적. 누락 시 위 기본값 = 현재 하드코딩 동작.

### 고정(리맵 불가) baseline

오버라이드 가능 키와 별개로 항상 동작하는 baseline:

- 트리 내비게이션: 화살표 ↑↓←→, PageUp/PageDown, Home/End
- focus-mux: `Enter` (prefix 무장 없이)
- prefix 후행 alias: `Tab`(=focus-mux), `Ctrl+Left`(=narrower), `Ctrl+Right`(=wider)
- `<prefix><prefix>`: mux로 literal prefix 1회 전달

리맵은 vi 문자 키(hjkl)와 액션 키, prefix 후행 키만 대상. 화살표 등 baseline은 항상 살아 있다.

## 컴포넌트

### config.rs

- `UiConfig`에 `tree_width: u16`(기본 48), `style: StyleConfig`, `keys: KeysConfig` 추가. 구조체에 `#[serde(rename_all = "kebab-case")]` (기존 `prefix`는 그대로 유지됨).
- `StyleConfig`: 각 필드 `Option<String>`(색 문자열) + `dim_inactive: Option<bool>`. 미설정 구분 위해 Option.
- `KeysConfig`: 각 액션 `Option<String>`(키 스펙).
- `Config::ui_tree_width()`, `Config::theme() -> (Theme, Vec<String> warnings)`, `Config::keymap() -> (Keymap, Vec<String> warnings)` 헬퍼. 색/키 파싱 실패는 기본값 유지 + 경고 문자열. 경고는 기존 `load_verbose` 경고와 함께 호출부(doctor/起動)에서 출력.

### ui/keymap.rs (신규)

- `parse_key_spec(&str) -> Option<(KeyCode, KeyModifiers)>`: `C-`/`c-` 접두 → CONTROL; 명명 키(Enter, Tab, Esc, Space, Left, Right, Up, Down, Home, End, PageUp, PageDown, Backspace); 단일 문자 → `Char`. 파싱 실패 시 `None`.
- `Keybinding { code: KeyCode, mods: KeyModifiers }` + `matches(&KeyEvent) -> bool`.
- `Keymap`: 명명 필드별 `Keybinding`. `Keymap::default()` = 현재 하드코딩 기본값. `Keymap::from_config(&KeysConfig) -> (Keymap, Vec<String>)`: 설정값 파싱, 실패 시 기본 유지 + 경고.

### Theme (config.rs 또는 ui/switcher.rs)

```rust
struct Theme {
    host: Color, session: Color, window: Color, pane: Color, hint: Color,
    accent: Color,
    dim_inactive: bool,
    inactive_fg: Color,
    inactive_bg: Option<Color>,
}
```

`Theme::default()` = 현재 상수값(yellow/green/magenta/cyan/darkgray, accent=green, dim_inactive=true, inactive_fg=darkgray, inactive_bg=None).

### ui/switcher.rs

- `const COLOR_*` 제거 → `Switcher`가 `theme: Theme`, `keymap: Keymap` 보유.
- 생성자(`new`, `from_sources`, `blank`)는 `Theme::default()`/`Keymap::default()`로 시작하고, cockpit이 `set_theme`/`set_keymap`으로 주입(또는 생성자 인자 추가). 헤드리스 테스트/`run.rs`의 `dump_*`는 기본값 사용 — 시그니처 영향 없음.
- `rebuild()`/`render_tree`/`render_divider`의 `COLOR_*` 참조를 `self.theme.*`로 교체.
- `handle_key`: 하드코딩 `match ev.code`를 keymap 비교로 교체. 화살표·PageUp/Down·Home/End는 고정 분기 유지; 그 외 액션은 `self.keymap.<action>.matches(&ev)`로 디스패치.
- `render` 시그니처는 유지(`frame, grid, terminal_focused, tree_width`). `terminal_focused`를 `render_tree`/`render_terminal_view`에 전달.

#### 포커스 표시 렌더링

`terminal_focused`(mux 포커스 여부) 기준:

- **구분선** `render_divider`: 세로선을 상단 절반/하단 절반으로 분할.
  - 트리 포커스(`!terminal_focused`): 상단 절반 = `accent`, 하단 절반 = `hint`(dim)
  - mux 포커스(`terminal_focused`): 상단 절반 = `hint`(dim), 하단 절반 = `accent`
  - 분할 경계 = `area.height / 2` (홀수 높이는 상단이 1행 더). 높이 1이면 포커스 쪽 색 한 칸.
- **비활성 트리** (`terminal_focused == true`, `dim_inactive`): `render_tree`에서 각 행 스타일에 `Modifier::DIM` 추가. 레벨 색(hue) 유지. 선택행은 `REVERSED + DIM` 유지 → 커서 위치 보존.
- **비활성 grid** (`!terminal_focused`... 즉 트리 포커스, `dim_inactive`): `render_terminal_view`에서 `g.render_into` 후 `area` 셀 후처리 — `cell.fg == Color::Reset`이면 `inactive_fg`로, `cell.bg == Color::Reset && inactive_bg.is_some()`이면 `inactive_bg`로 치환. 앱 명시색 셀은 보존(tmux `window-style` 충실).

흐림 적용 방향 요약:

```
   트리 포커스                        mux 포커스
 local            ┃accent           local            ┊dim
 editor   2 wins  ┃accent  vim…dim  editor   2 wins  ┊dim   vim…(밝음)
  win 1: shell    ┊dim              (트리 전체 DIM)   ┃accent
 ...              ┊dim              ...               ┃accent
 트리 밝음, grid 흐림                트리 흐림, grid 밝음
 구분선 위=accent/아래=dim           구분선 위=dim/아래=accent
```

`dim_inactive = false`면 트리 DIM·grid 치환 모두 생략, 포커스 단서는 구분선 절반 분할만.

### cockpit.rs

- 起動 시 `cfg`에서 `Theme`/`Keymap`/`tree_width` 생성:
  - `tree_width` 초기값 = `cfg.ui_tree_width()`를 `adjust_tree_width`로 클램프(20..100). 기존 `TREE_WIDTH` 상수는 `Theme`/기본값 용으로 코드에 잔존.
  - `switcher.set_theme(theme)` / `switcher.set_keymap(keymap.clone())` 주입.
  - prefix 무장 핸들러(`handle_tree_bytes`)의 하드코딩(`'q'`/`'h'`/`'l'`/`Right`/`Tab`/`Ctrl+Left/Right`)을 `keymap`의 prefix-액션 + 고정 alias 비교로 교체. `keymap`을 `handle_tree_bytes` 인자로 전달.
- 색/키 경고는 起動 로그(또는 `doctor`)에서 기존 config 경고와 함께 표출.

### 포커스 시 트리 자동 숨김 (hide-tree-on-focus)

자기완결적 슬라이스 — config.rs · cockpit.rs · switcher.rs 일부만 건드린다.

- **config.rs**: `UiConfig`에 `hide_tree_on_focus: bool`(`#[serde(rename = "hide-tree-on-focus", default)]`, 기본 false) 추가. `Config::ui_hide_tree_on_focus(&self) -> bool` 헬퍼. `UiConfig::default()`도 false. 색/키 파싱과 무관(불리언이라 경고 없음).
- **cockpit.rs**: 별도 Env 필드 없이 `env.cfg.ui_hide_tree_on_focus()`로 읽는다(테스트 Env 리터럴은 `Config::default()` → false, 생성자 변경 불필요).
  - 루프 최상단에서 **유효 트리 너비**를 계산: `let eff_tree_width = if !app.is_overlay() && hide { 0 } else { tree_width };`. 런타임 `tree_width`(prefix h/l 조절값)는 트리의 자연 너비로 그대로 보존 — 숨김은 mux 포커스 동안 effective 값만 0으로 만든다.
  - `eff_tree_width`를 렌더(`switcher.render`)·PTY 사이징(`terminal_view_size`)·마우스 히트테스트 `term_area`·`select_attach`·`handle_host_event`에 전달. h/l 리사이즈·`handle_tree_bytes`·`connect_all_sources`(起動)는 overlay 컨텍스트라 `eff == tree_width`이므로 `tree_width` 유지.
  - **포커스 전환 리사이즈**: 루프 최상단의 reconcile가 effective 너비의 단일 소유자 — 포커스/설정/natural 너비가 바뀌면 `terminal_view_size`로 `registry.resize_all` + `mgr.resize_all` + `dirty`. prefix h/l은 `tree_width_natural`만 갱신하고 reconcile가 다음 패스에서 적용(중복 resize 경로 제거). mux는 SIGWINCH/resize로 자연 reflow.
  - **숨김 중 복귀(의도된 동작)**: 트리가 숨겨지면 클릭 대상 열이 없으므로 마우스로 트리에 되돌아갈 수 없다. 복귀는 prefix 포커스 키(`prefix Tab`/`←`/`Esc`) 전용 — opt-in 기능의 내재적 결과로, 키보드 탈출은 항상 동작한다.
- **switcher.rs**: `render`/`terminal_view_size`가 `tree_width == 0`을 "트리 숨김" sentinel로 처리.
  - `terminal_view_size(cols, rows, 0)` → view_cols = `cols`(구분선 1열 없이 전체 폭). 비-0이면 기존대로 `cols - tw - 1`.
  - `render`: `tree_width == 0`이면 트리/입력/푸터/구분선 생략, 터미널 뷰가 `frame.area()` 전체 차지. 비-0이면 기존 3열 레이아웃.
  - 런타임 `tree_width`는 20..100 클램프라 0은 오직 숨김 sentinel로만 발생.

## 데이터 흐름

```
config.toml ──load──▶ Config
                       ├─ ui_tree_width() ─▶ cockpit tree_width 초기값
                       ├─ theme()  ─▶ Theme ─▶ Switcher.theme ─▶ render(색/dim/divider)
                       ├─ keymap() ─▶ Keymap ─┬▶ Switcher.keymap ─▶ handle_key(트리 액션)
                       │                      └▶ cockpit handle_tree_bytes(prefix 액션)
                       └─ ui_hide_tree_on_focus() ─▶ cockpit eff_tree_width(=0 when mux-focus)
                                                     ─▶ render(no tree/divider) + resize_all PTYs
```

## 에러 처리

- 파일 없음/누락 키: 기본값(현재 동작). 무경고.
- 잘못된 색 문자열 / 키 스펙: 해당 항목만 기본값 유지 + `"invalid ui.style.host color \"...\""` / `"invalid ui.keys.kill key \"...\""` 형식 경고. 전체 로드는 성공.
- 미지 키(typo): 기존 `serde_ignored` 경고 경로 유지.
- `tree-width` 범위 밖: `adjust_tree_width` 클램프(에러 아님).
- `hide-tree-on-focus` 비-불리언 값: toml 디코드 단계에서 거부(불리언 필드). 누락 시 false.

## 테스트

기존 헤드리스 `TestBackend` 하네스(`switcher.rs` 테스트 모듈, `tree_fg_of`/`tree_modifier_of`/`row_of`) 재사용.

- **config.rs**: `tree-width`/`[ui.style]`/`[ui.keys]` 라운드트립; 잘못된 색→기본+경고; 잘못된 키→기본+경고; 미설정 시 기본값; 미지 키 경고 유지.
- **keymap.rs**: `parse_key_spec` 케이스(문자/명명키/`C-`/실패); `Keymap::from_config` 리맵+기본 폴백; `matches`.
- **switcher.rs**:
  - 주입 `Theme`의 레벨 색이 해당 행 fg에 반영(`tree_fg_of`).
  - 리맵 키로 액션 발동(예: `kill="d"` → `d` 키가 kill 무장; 기본 `x`는 미발동).
  - 화살표 등 baseline은 리맵과 무관하게 항상 내비게이션.
  - 구분선: 트리 포커스 시 상단 절반 accent·하단 hint, mux 포커스 시 반대(특정 셀 fg 검사).
  - 비활성 트리: `terminal_focused=true`로 렌더 시 행에 `Modifier::DIM`, 색조 유지.
  - 비활성 grid: 트리 포커스 + `Reset` 셀 → `inactive_fg`로 치환, 명시색 셀 보존.
  - `dim_inactive=false`: DIM/치환 없음, 구분선 분할만.
- **hide-tree-on-focus** (자기완결 슬라이스, 이번 회차 구현 범위):
  - config.rs: `hide-tree-on-focus` 라운드트립(true/false); 누락 시 false; 미지 키 경고 유지.
  - switcher.rs(`TestBackend`): `tree_width == 0`으로 렌더 시 (a) 좌측 트리 열에 트리/구분선 셀 없음, (b) 터미널 뷰가 0열부터 전체 폭 차지(특정 셀 위치 검사); 비-0이면 기존 3열 레이아웃 유지.
  - `terminal_view_size(cols, rows, 0) == (cols, rows+1)`; `terminal_view_size(cols, rows, 48)`는 기존대로 `cols-49`.
  - cockpit.rs: `eff_tree_width` 순수 계산(overlay→tree_width, mux-focus+hide→0, mux-focus+!hide→tree_width)을 단위 검증.

## 건드리는 파일

- `src/config.rs` — 스키마/파싱/헬퍼/테스트 (+ `hide_tree_on_focus` 필드·`ui_hide_tree_on_focus()`)
- `src/ui/keymap.rs` — 신규(키 스펙 파서·Keymap·테스트)
- `src/ui/mod.rs` — `pub mod keymap;`
- `src/ui/switcher.rs` — Theme/Keymap 보유, COLOR_* 제거, handle_key·render_divider·render_tree·render_terminal_view, 테스트 (+ `render`/`terminal_view_size` tree_width==0 sentinel 처리)
- `src/cockpit.rs` — config→theme/keymap/tree_width 주입, handle_tree_bytes prefix 키 교체 (+ `eff_tree_width` 계산·포커스 전환 resize)
- `src/ui/run.rs` — 영향 없음(기본 Theme/Keymap; render 시그니처 불변). 변경 시 dump 경로 기본값 확인만.

### 이번 회차 구현 범위

구현된 것:
- **5) hide-tree-on-focus**: config.rs(필드+헬퍼+테스트), switcher.rs(`render`/`terminal_view_size`의 tree_width==0 sentinel+테스트), cockpit.rs(`tree_width`=effective + `tree_width_natural` + 루프 최상단 reconcile가 effective width·resize의 단일 소유자).
- **4) 포커스 표시 중 구분선 상하 분할**: `render_divider`가 accent(green) 절반의 위치로 활성 쪽을 표시(상단=트리, 하단=mux). 색은 현재 green/darkgray 하드코딩.
- **help 팝업 CJK 안전 여백**: `popup_clear_rect`가 팝업 외곽선 좌우로 반각 1칸씩 Clear 여백을 둬 경계에 걸친 전각문자 잔상을 제거.
- **트리 너비 런타임 영속화**: prefix h/l로 조정한 `tree_width_natural`을 `state`(`~/.xmux/tree_width`)에 저장하고 다음 실행에 복원(범위 밖 stale 값은 클램프). 설정 파일 `tree-width` 초기값(1번)과는 별개 — 런타임 조정값 유지 요구를 충족.
- **prefix 명령 focus 일관화**: help(`?` 토글)·너비(`h`/`l`·`Ctrl+←/→`) 명령이 트리·mux 포커스 **양쪽**에서 동일하게 동작(`TermInput`에 `ShowHelp`/`Width` 액션 추가, `apply_width_delta`로 적용 단일화). focus 의존은 입력 전달·토글 방향·literal prefix에만 남김.
- **tmux식 반복 리사이즈**: prefix로 리사이즈하면 `RESIZE_REPEAT_MS`(400ms) "반복 창"이 열려, 그동안 prefix 없이 `Ctrl+←/→`만으로 계속 리사이즈되고 매 입력마다 창이 갱신됨(`repeat_until` + `ctrl_arrow_delta`). 화살표 아닌 키/타임아웃 시 종료. 양쪽 포커스 공통.

별도 회차(미구현): `[ui] tree-width` 설정 키(파일 기반 초기값), `[ui.style]`/`[ui.keys]`(색·키 설정화 = `COLOR_*`/Theme/Keymap), 그리고 4번의 **비활성 페인 흐림**(`dim_inactive`/`window-style` 대응)과 구분선 accent 색의 설정화.
