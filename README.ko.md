# xmux

[English](README.md) · 한국어

*여러 호스트의 터미널 멀티플렉서 세션을 한자리에서 전환하는 도구.*

xmux는 터미널에 상주하는 Rust 프로그램이다. xmux는 자신을 실행한 터미널을
직접 소유하고, 각 터미널 멀티플렉서(이하 mux)에 연결한 attach를 유지한 상태로
화면을 분할해 표시한다. 왼쪽 창에는 접근할 수 있는 모든 세션의 **목록**이,
터미널 뷰에는 선택한 세션의 **실시간 화면**이 표시된다. 목록에서 커서를
움직이면 터미널 뷰가 해당 세션의 화면으로 즉시 전환된다.

![xmux의 분할 화면. 왼쪽 목록에 이 머신의 psmux 세션과 WSL 배포판 안의 tmux
세션이 함께 있고, 오른쪽에는 선택한 세션의 터미널 뷰가 차 있다.](docs/assets/xmux.png)

## 설치

xmux는 단일 바이너리로, Windows·macOS·Linux용 사전빌드 패키지를
[릴리스](https://github.com/zer0ken/xmux/releases) 페이지에서 제공한다.
운영체제별 단계별 설치 방법(사전빌드 바이너리·패키지 매니저·소스 빌드)은
[`INSTALL.md`](INSTALL.md)에 있다.

패키지 매니저로 빠르게 설치한다.

```sh
brew install zer0ken/xmux/xmux        # macOS
cargo install xmux                    # Rust가 있는 모든 OS
```

winget 설치는 아직 불가능하다. [`packaging/winget`](packaging/winget)의
매니페스트가 커뮤니티 winget-pkgs 리포에 등재되어 있지 않다. Windows에서는
사전빌드 바이너리나 `cargo install xmux`를 사용한다.

소스에서 `xmux` 명령을 `PATH`에 설치한다.

```sh
cargo install --path .
```

설치하지 않고 릴리스 바이너리만 빌드하려면 다음과 같이 한다.

```sh
cargo build --release        # 바이너리 경로는 target/release/xmux
```

xmux는 Windows와 unix 계열에서 동작한다. 원격 호스트를 사용하려면 xmux를
실행한 머신에 `ssh`가 설치되어 있어야 한다. 대상 호스트에는 지원하는 mux가
하나 이상 있어야 한다.

## 지원 mux

xmux가 지원하는 mux는 다음과 같다.

- unix의 `tmux`·GNU `screen`
- Windows의 `psmux`
- `zellij` / `abduco`

xmux는 호스트가 어느 바이너리로 응답하는지를 보고 그 호스트의 mux를 판별한다.
따라서 호스트마다 다른 mux가 설치되어 있어도 설정할 것이 없다.

## 사용법

`xmux`를 인자 없이 실행하면 앱이 실행된다.

```sh
xmux                          # 앱을 실행한다
xmux ls                       # 접근할 수 있는 모든 세션을 나열한다 (스크립트용)
xmux attach <source>/<name>   # 세션 하나에 바로 attach 한다, 예: xmux attach prod/api
xmux doctor                   # 설정과 소스별 접근 가능 여부를 점검한다
xmux instances                # 실행 중인 인스턴스를 나열한다
xmux send <name> <command…>  # 그중 하나를 컨트롤 소켓으로 조작한다
xmux update                  # 설치된 바이너리를 갱신한다
xmux version
```

왼쪽 창이 목록이고, 터미널 뷰는 선택한 세션의 실시간 화면을 보여 준다. 키보드 포커스는
두 영역 중 한쪽에만 있다.

## 키

목록의 키 바인딩이다.

| 키                                   | 동작                                                       |
| ------------------------------------ | ---------------------------------------------------------- |
| `↑` / `↓` (또는 `k` / `j`) | 카드 한 개 이동 (양 끝에서 순환한다)                       |
| `←` / `→` | 이전 / 다음 `host/mux` 구역으로 이동한다. 호스트 카드들은 하나로 친다     |
| `Home` / `End`                   | 첫 카드 / 마지막 카드로 이동                               |
| `PageUp` / `PageDown`            | 카드 열 개 이동                                            |
| `Enter`                            | 선택한 세션의 터미널 뷰로 포커스를 옮긴다                |
| `prefix 1`-`prefix 9`            | 왼쪽 열의 번호로 세션을 선택한다 (10 이상은 계속 입력한다) |
| `prefix n`                         | 선택한 호스트에 새 세션을 만든다                           |
| `/`                                | 목록을 퍼지 필터로 좁힌다                                  |
| `prefix r`                         | 다시 스캔한다. 머신 목록과 각 소스의 세션을 모두 갱신한다  |

xmux에는 tmux의 `set -g prefix`처럼 자체 prefix가 있다. 기본값은 `Ctrl-g`이며,
`[ui] prefix` 설정이 이 값을 대체한다. prefix를 누른 다음 조합키를 입력한다.
`prefix q`는 종료, `prefix ?`는 키 도움말 토글, `prefix Tab`은 목록과 터미널 뷰
사이의 포커스 이동이다. 마우스 입력도 지원한다. 행을 클릭하면 그 행이
선택되고, 터미널 뷰를 클릭하면 포커스가 그쪽으로 옮겨진다. 나머지 키는
[`docs/keybind.md`](docs/keybind.md)에 있다.

## 로스터

xmux가 호스트로 내놓는 머신 후보는 로스터가 조립한다. 로스터는 세 공급자에서
ssh 대상 이름을 모은다.

- `~/.ssh/config` 별칭
- 이 머신 tailnet의 온라인 피어
- 이 머신의 WSL 배포판

모든 공급자는 ssh 대상 이름을 산출한다. 어느 공급자가 이름을 제안했든 하류
동작은 같으며, 제안한 공급자는 이름 옆에 보관되어 도달할 수 없는 호스트가
생기면 그 화면에 표시된다. 사용자는 그 정보로 어느 공급자를 살피거나 끌지
판단한다. 공급자가 도는 데 필요한 CLI가 없거나 데몬이 내려갔거나 출력을
해석하지 못하면, 로스터는 그 공급자를 오류로 삼지 않고 빈 목록으로 처리한다.
따라서 한 공급자가 죽어도 다른 공급자가 제안한 호스트는 계속 제공된다.

로스터가 어떤 머신을 호스트로 만들지 정한다. 어떤 공급자도 이름을 부르지 않는
머신은 xmux가 다룰 것이 없는 머신이다. `local`은 ssh 없이 도달하는 이 머신
자체라 로스터에 포함되지 않는다. 로스터는 시작 시와 재스캔마다 다시 조립된다.
`[discovery]` 표는 공급자를 개별로 끄는 방법이며, 기본값은 전부 켜져 있다.

## 호스트와 소스

**호스트**는 mux가 동작하고 있고 xmux가 접근할 수 있는 머신이다. 호스트
하나가 여러 mux를 동시에 제공할 수 있으므로, xmux는 psmux와
zellij가 함께 동작하는 호스트를 두 소스로 노출한다. 호스트와 mux의 조합
하나하나가 **소스**다. 소스 이름은 호스트가 여러 mux를 제공하면 `local:psmux`
형식이고, 하나만 제공하면 `prod` 형식이다. 목록에 표시되는 이름이 곧 명령에서
`<source>/<session>`으로 지정하는 이름이다. xmux는 원격 호스트를 앱이 실행된
뒤에 조회하므로, 발견한 소스는 각 호스트가 응답하는 대로 하나씩 나타난다.
네트워크에는 응답하지만 자격 증명이 거부된 원격 호스트는 `locked`로 표시된다.
해당 카드에서 Enter를 누르고 사용자 이름과 마스킹된 비밀번호를 입력하면, xmux가
인증된 연결 하나를 확립해 나머지 세션이 그 연결을 재사용한다.

## 설정

설정은 전부 선택 사항이다. xmux는 `~/.config/xmux/config.toml`을 읽는다.

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # 이 머신들은 목록에서 숨긴다

[local]
mux = "auto"          # "auto"(기본값)는 이 머신에 설치된 mux 전부를 뜻한다.
                      # ["psmux", "zellij", "abduco"]처럼 목록도 받는다.

[ui]
theme = "auto-dark"                  # 내장 ANSI 테마: "auto-dark"(기본값) 또는
                                      # "auto-light"(밝은 터미널용)
prefix = "C-g"                        # xmux의 prefix (예: C-g, C-Space, C-b)
auto-hide-nav = false                 # auto-hide-nav의 초기 상태
hide-unreachable = true               # 도달하지 못한 호스트는 nav에서 숨긴다 (필터에 이름을 입력하면 카드가 나타난다)
view-active-border-style = "green"    # 포커스된 view border의 색
hint-bar-style = "bg=blue,fg=white"   # 힌트 바의 색 (tmux status-style)
primary = "brightwhite"               # 역할별 색 오버라이드: primary, secondary,
accent = "lightgreen"                # accent, decoration, warning, error, disabled,
bar-bg = "colour235"                  # 힌트 바의 bar-bg / bar-fg / bar-accent

[[hosts]]
ssh = "prod"          # ssh-config 별칭
mux = "tmux"          # 생략하면 "tmux"
```

xmux는 `config.toml`이 바뀌면 `[ui]` 표시 설정(테마, 역할별 색 오버라이드,
selection-style, hint-bar-style, view-border 스타일)을 재시작 없이 실시간으로
다시 적용한다.
호스트/로스터 변경은 여전히 `prefix r`로 다시 스캔해야 한다.

xmux는 호스트를 먼저 `~/.ssh/config`에서 읽는다. 설정 파일은 그 발견을 보완하며
대체하지 않는다. 다음 실행까지 남는 상태(마지막에 선택한 세션, auto-hide-nav
토글, 로그, 컨트롤 소켓)는 `~/.xmux/` 아래에 있다.

## 컨트롤 소켓

실행 중인 인스턴스마다 이름이 있고, 각 인스턴스는 `~/.xmux/ctl-<name>.sock`에서
요청을 받는다. 세션은 `<source>/<session>`으로 지정한다. 소켓이 받는 명령은
탐색 명령(`ping`, `status`, `dump`, `rescan`, `switch`, `focus`, `width`,
`toggle-auto-hide`, `quit`)과 세션 수명 명령 하나(`new-session`)다. kill,
rename, window 명령은 없다. 세션을 편집하는 일은 mux가 담당한다.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod/api
xmux send am focus terminal          # 겹치지 않는 이름 앞부분으로 지정한다
xmux send - dump                     # 하나만 실행 중일 때는 `-`
```

없는 이름, 여러 인스턴스에 걸리는 앞부분, 여러 인스턴스가 실행 중일 때의 `-`는
모두 후보를 알려주는 오류로 끝난다. xmux는 짐작해서 하나를 고르지 않는다.

## 라이선스

MIT 라이선스다. 전문은 [`LICENSE`](LICENSE)에 있다.

## 더 읽을 것

- [`docs/keybind.md`](docs/keybind.md) - 키 바인딩과 prefix 상세
- [`docs/requirements.md`](docs/requirements.md) - 동작 요구 사항
- [`docs/adr/`](docs/adr/) - 아키텍처 결정 기록
- [`CONTEXT.md`](CONTEXT.md) - 용어와 설계 개요
- [`AGENTS.md`](AGENTS.md) - 디렉터리별 작업 노트
