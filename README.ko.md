# xmux

*호스트를 넘나드는 터미널 멀티플렉서 전환기. tmux의 `prefix + s` / `switch-client`를 그대로 쓰되, 모든 머신에 닿는다.*

xmux는 Rust로 만든 상주형 터미널 관리자다. 자신을 띄운 터미널을 직접 소유하고
살아 있는 mux 화면 연결을 유지하면서 화면을 둘로 나눠 보여준다. 왼쪽에는 닿을 수
있는 모든 세션의 **트리**가, 오른쪽에는 지금 고른 세션의 **실시간 화면**이 뜬다.
트리에서 커서를 옮기면 오른쪽 창이 그 세션으로 곧장 바뀐다. 로컬 psmux 세션이든
ssh 너머의 tmux 세션이든 똑같다. 떼었다 다시 붙이는 절차도, 목록 창을 거치는
왕복도 없다.

목표는 tmux에서 이미 익숙한 `switch-client` 경험을 호스트 경계 밖까지 넓히는
것이다. 설정해 둔 어느 머신의 mux 세션이든, 터미널 하나에서 즉시 제자리에서
오간다.

## 기능

- **모든 호스트를 하나의 트리로.** 호스트 → 세션 → 창 → 페인까지, 로컬과 ssh를
  한 화면에 담는다. 호스트는 `~/.ssh/config`에서 알아서 찾아낸다.
- **호스트를 넘는 제자리 전환.** 다른 머신의 세션을 고르면 같은 터미널 창 안에서
  다시 연결되고, 같은 서버의 다른 세션을 고르면 클라이언트가 제자리에서 넘어간다.
  손으로 뗄 일도, 원격에 뭔가를 깔 일도 없다.
- **미리보기가 아니라 실시간 화면.** 오른쪽 창은 세션마다 붙는 진짜 PTY 연결이라,
  눈에 보이는 것이 세션의 실제 화면이다. 트리를 옮겨 다니는 동안에도 계속 살아
  있다.
- **직교하는 두 축.** `Mux` 축(**tmux**·**psmux**)과 `Transport` 축(**로컬**·**ssh**)이
  자유롭게 조합된다. 어느 mux든 어느 전송 방식 위에서든, 서로를 몰라도 맞물린다.
- **꼭 필요한 곳은 폴링 없이.** tmux 호스트는 컨트롤 모드(`-CC`)로 추적하고, psmux
  호스트는 폴링한다. 어느 쪽이든 트리는 서버를 그대로 비춘다. 판단 기준은 늘 서버
  쪽에 있다.
- **마우스도 키보드도.** 세션을 오가고 거르고 만들고 이름 바꾸고 없애는 일 모두
  키보드로 한다. 클릭·스크롤·우클릭도 된다.
- **컨트롤 소켓.** 로컬 소켓이 스크립팅과 헤드리스 구동을 위한 의미 단위 명령을
  노출한다([컨트롤 소켓](#컨트롤-소켓) 참고).

## 설치

xmux는 Cargo 프로젝트다. 릴리스 바이너리를 빌드하려면:

```sh
cargo build --release        # 바이너리는 target/release/xmux
```

또는 `PATH`에 바로 설치한다:

```sh
cargo install --path .
```

Windows와 unix 계열에서 돈다. xmux를 돌리는 머신에는 원격 호스트용 `ssh`가 있어야
하고, 대상으로 삼는 머신마다 지원되는 mux가 있어야 한다. unix는 `tmux`, Windows는
`psmux`다. 둘은 같은 명령어 언어를 쓰며, xmux는 어느 쪽이든 다룬다.

## 사용법

인자 없이 실행하면 대화형 분할 화면이 열린다:

```sh
xmux                          # 대화형 트리 + 실시간 화면 앱
xmux ls                       # 닿을 수 있는 모든 세션 나열 (스크립트용)
xmux attach <source>/<name>   # 세션 하나에 바로 연결, 예: xmux attach prod/api
xmux doctor                   # 설정과 호스트별 접속 가능 여부 점검
xmux ctl <command…>           # 실행 중인 인스턴스를 컨트롤 소켓으로 구동
xmux version
```

### 앱 안에서

왼쪽 창은 트리, 오른쪽 창은 고른 세션의 실시간 화면이다. 키보드 초점은 한 번에 한
영역에만 놓인다.

**트리 이동:**

| 키 | 동작 |
|---|---|
| `↑` / `↓` (또는 `k` / `j`) | 같은 단계의 형제 항목 사이 이동 |
| `→` / `←` (또는 `l` / `h`) | 자식으로 내려가기 / 부모로 올라가기 |
| `Home` / `End` | 첫 행 / 마지막 행으로 |
| `PageUp` / `PageDown` | 열 행씩 이동 |
| `Enter` | 고른 세션의 실시간 화면으로 초점 옮기기 |
| `prefix n` | 만들기 (고른 단계에 따라 세션 / 창 / 분할) |
| `prefix R` | 고른 세션이나 창 이름 바꾸기 |
| `prefix x` | 고른 세션 없애기 (확인 창이 뜬다) |
| `/` | 트리 퍼지 필터 |
| `prefix r` | 모든 호스트 다시 훑기 |

마우스도 된다. 행을 클릭하면 선택되고, 오른쪽 창을 클릭하면 그쪽으로 초점이 간다.
트리 위에서 휠을 굴려 스크롤하고, 행을 우클릭하면 컨텍스트 메뉴가 뜬다.

**Prefix 키.** xmux는 tmux의 `set -g prefix`처럼 자체 prefix를 둔다. 기본값은
`Ctrl-g`이고 `[ui] prefix`로 바꾼다(아래 참고). prefix를 누른 다음:

| 키 조합 | 동작 |
|---|---|
| `prefix q` | xmux 종료 |
| `prefix ?` | 키 도움말 켜고 끄기 |
| `prefix t` | 트리 자동 숨김 켜고 끄기 (화면에 초점을 주면 전체 폭을 쓴다) |
| `prefix h` / `prefix l` (또는 `prefix Ctrl-←/→`) | 트리 좁히기 / 넓히기 |
| `prefix Tab` / 화살표 / `Esc` | 트리와 화면 사이 초점 이동 |
| `prefix prefix` | prefix 바이트 하나를 초점 세션에 그대로 보내기 |

prefix에 관한 자세한 내용은 [`docs/keybind.md`](docs/keybind.md)를 참고한다.

## 설정

설정은 전부 선택 사항이다. 아무것도 설정하지 않는 것이 기본이다. xmux는
`~/.config/xmux/config.toml`을 읽는다:

```toml
# 로컬 머신에서 쓸 mux.
[local]
mux = "auto"          # "auto"(기본값): Windows는 psmux, 그 외는 tmux

# 발견된 ssh 호스트의 mux를 바꾸거나, ssh-config 발견이
# 잡아내지 못한 호스트를 더한다.
[[hosts]]
ssh = "prod"          # ssh-config 별칭
mux = "tmux"          # 생략하면 "tmux"

# 이 ssh 별칭들은 트리에서 숨긴다.
exclude = ["bastion"]

[ui]
prefix = "C-g"                        # xmux prefix (예: C-g, C-Space, C-b)
auto-hide-tree = false                # 트리 자동 숨김 초기 상태
view-active-border-style = "green"    # 초점 뷰 테두리 색 (tmux 색 표기)
view-border-style = "default"         # 비초점 뷰 테두리 색
view-border-hover-style = "yellow"    # 크기 조절 드래그 호버 표시
hint-bar-style = "bg=blue,fg=white"   # 힌트 바 색 (tmux status-style; 비우면 tmux 기본값)
```

호스트는 먼저 `~/.ssh/config`에서 온다. 접속 정보(사용자, 포트, 키, 점프 호스트)를
거기서 가져온다. 설정 파일은 그 발견을 보강할 뿐 대체하지 않는다. `xmux doctor`를
돌리면 확정된 로컬 mux, ssh 사용 가능 여부, 호스트별 접속 가능 여부를 보여준다.
상태 정보(마지막으로 고른 세션, 지금 켜진 트리 자동 숨김 값, 로그, 컨트롤 소켓)는
`~/.xmux/` 아래에 있다.

## 컨트롤 소켓

실행 중인 xmux 인스턴스는 로컬 소켓(`~/.xmux/ctl-<pid>.sock`)을 듣는다. 세션은
`<source>/<session>`으로, 창은 `<source>/<session>:<window>`로 지정한다. 이 소켓은
이동·표시 명령을 받는다. `ping`, `status`, `dump`, `rescan`,
`switch <source>/<session>`, `focus <tree|terminal>`, `width <delta>`(트리 폭을
부호 있는 열 수만큼 조정한다. 절대 폭이 아니라 증분이다), `toggle-auto-hide`,
`quit`이 있고, 세션 수명을 다루는 명령도 있다:

- `new-session <source> [name]`
- `kill-session <source>/<session>`
- `rename-session <source>/<session> <name>`
- `new-window <source>/<session> [name]`
- `split-window <source>/<session>:<window> [v|h]` — 기본은 세로
- `kill-window <source>/<session>:<window>`
- `rename-window <source>/<session>:<window> <name>`

저수준 키·바이트 주입용으로 불안정한 `raw:` 네임스페이스를 예약해 두었다. 이렇게
쓴다:

```sh
xmux ctl status
xmux ctl switch prod/api
```

인스턴스가 하나만 돌고 있으면 `xmux ctl`이 알아서 그것을 겨냥한다. 여럿이 돌고
있으면 함부로 짐작하지 않는다. 목록을 보고 pid로 하나를 짚는다.

```sh
xmux ctl list                 # PID · CWD · TTY · 표시 중 세션 · 초점
xmux ctl --pid 51907 switch local/logs
```

## 구조

xmux는 직교하는 두 축을 중심으로 짜였다. `Mux`(mux별 동작)와 `Transport`(머신별
실행)다. 이렇게 나눈 덕분에 mux 계열과 머신 계열이 서로 뒤섞이지 않고 조합된다.
메타데이터 경로와 표시 경로는 따로 두었고, 관리자는 mux에 특화된 어떤 것에도
분기하지 않는다.

정식 안내는 디렉터리별 작업 노트([`AGENTS.md`](AGENTS.md) 파일들)와, 어휘와 직교
설계 개요를 담은 [`CONTEXT.md`](CONTEXT.md)에 있다. 아키텍처 결정은
[`docs/adr/`](docs/adr/)에, 동작 요구사항은
[`docs/requirements.md`](docs/requirements.md)에 적혀 있다.

## 라이선스

MIT. [`LICENSE`](LICENSE) 참고.
