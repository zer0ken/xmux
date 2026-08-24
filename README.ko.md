# xmux

*호스트를 넘나드는 터미널 멀티플렉서 전환기. tmux의 `prefix + s` / `switch-client`를 그대로 쓰되, 모든 머신에 닿는다.*

xmux는 Rust로 만든 상주형 터미널 관리자다. 자신을 띄운 터미널을 직접 소유하고
살아 있는 mux attach를 유지하면서 화면을 둘로 나눠 보여준다. 왼쪽에는 닿을 수
있는 모든 세션의 **목록**이, 오른쪽에는 지금 고른 세션의 **실시간 화면**이 뜬다.
목록에서 커서를 내리면 오른쪽 창이 그 세션으로 곧장 바뀐다. 로컬 psmux 세션이든
ssh 너머의 tmux 세션이든, 또 다른 머신의 zellij 세션이든 똑같다. 떼었다 다시
붙일 일도, 목록 창을 거쳐 고를 일도 없다.

![xmux의 분할 화면. 왼쪽 목록에 이 머신의 psmux 세션과 WSL 배포판 안의 tmux
세션이 함께 있고, 오른쪽에는 고른 세션의 실시간 화면이 차 있다.](docs/assets/xmux.png)

## 설치

xmux는 Cargo 프로젝트다. 릴리스 바이너리를 빌드한다.

```sh
cargo build --release        # 바이너리는 target/release/xmux
```

또는 `PATH`에 설치한다.

```sh
cargo install --path .
```

Windows와 unix 계열에서 돈다. 원격 호스트를 쓰려면 xmux를 띄운 머신에 `ssh`가
있어야 하고, 대상 호스트마다 지원하는 mux가 하나는 있어야 한다. unix는 `tmux`,
Windows는 `psmux`, 아니면 `zellij`다. 호스트의 mux는 그 호스트가 어느 바이너리로
응답하는지 보고 알아내므로, 호스트마다 다른 것이 깔려 있어도 설정할 것이 없다.

## 사용법

인자 없이 실행하면 대화형 분할 화면이 열린다.

```sh
xmux                          # 대화형 목록 + 실시간 화면 앱
xmux ls                       # 닿을 수 있는 모든 세션 나열 (스크립트용)
xmux attach <source>/<name>   # 세션 하나에 바로 attach, 예: xmux attach prod/api
xmux doctor                   # 설정과 소스별 도달 가능 여부 점검
xmux instances                # 실행 중인 인스턴스 나열
xmux send <name> <command…>  # 그중 하나를 컨트롤 소켓으로 조종
xmux version
```

왼쪽 창이 목록이고, 오른쪽 창이 고른 세션의 실시간 화면이다. 키보드 포커스는 한
쪽에만 있다.

## 키

목록에서 쓰는 키다.

| 키 | 동작 |
|---|---|
| `↑` / `↓` (또는 `k` / `j`) | 카드 하나 이동 (양 끝에서 순환) |
| `Home` / `End` | 첫 카드 / 마지막 카드로 |
| `PageUp` / `PageDown` | 카드 열 개 이동 |
| `Enter` | 고른 세션의 실시간 화면으로 포커스를 옮긴다 |
| `prefix 0`-`prefix 9` | 왼쪽 열의 번호로 세션을 고른다 (10 이상은 계속 입력) |
| `prefix n` | 고른 호스트에 새 세션을 만든다 |
| `/` | 목록을 퍼지 필터로 좁힌다 |
| `prefix r` | 모든 소스를 다시 스캔한다 |

xmux에는 tmux의 `set -g prefix`처럼 자체 prefix가 있다. 기본값은 `Ctrl-g`이고
`[ui] prefix`로 바꾼다. prefix를 누른 다음 조합키를 누른다. `prefix q`는 종료,
`prefix ?`는 키 도움말 토글, `prefix Tab`은 목록과 화면 사이 포커스 이동이다.
마우스도 된다. 행을 클릭하면 선택되고, 오른쪽 창을 클릭하면 포커스가 간다.
나머지는 [`docs/keybind.md`](docs/keybind.md)에 있다.

## 호스트와 소스

**호스트**는 mux를 올려 두고 xmux가 닿을 수 있는 머신이다. 호스트는
`~/.ssh/config`, tailnet, 이 상자의 WSL 배포판, 그리고 `local`에서 알아서
찾아낸다. 호스트 하나가 여러 mux를 한꺼번에 올릴 수 있으므로, psmux와 zellij를
같이 돌리는 호스트는 둘 다 내놓는다. 호스트와 mux를 짝지은 것 하나하나가
**소스**이고, 여러 개를 올린 호스트는 `local:psmux`처럼, 하나만 올린 호스트는
`prod`처럼 이름이 붙는다. 목록에 뜨는 이름이 곧 명령에서
`<source>/<session>`으로 가리킬 때 쓰는 이름이다. 원격 호스트는 앱이 뜬 뒤에
물어보므로, 찾아낸 소스는 그 호스트가 답하는 대로 나타난다.

## 설정

설정은 전부 선택 사항이다. xmux는 `~/.config/xmux/config.toml`을 읽는다.

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # 이 기계들은 목록에서 숨긴다

[local]
mux = "auto"          # "auto"(기본값): 여기 깔려 있는 mux 전부.
                      # ["psmux", "zellij"]처럼 목록도 받는다.

[ui]
prefix = "C-g"                        # xmux의 prefix (예: C-g, C-Space, C-b)
auto-hide-nav = false                 # auto-hide-nav 초기 상태
view-active-border-style = "green"    # 포커스된 view border 색
hint-bar-style = "bg=blue,fg=white"   # 힌트 바 색 (tmux status-style)

[[hosts]]
ssh = "prod"          # ssh-config 별칭
mux = "tmux"          # 생략하면 "tmux"
```

호스트는 `~/.ssh/config`에서 먼저 온다. 설정 파일은 그 발견을 거들 뿐 대체하지
않는다. 남는 상태(마지막에 고른 세션, auto-hide-nav 토글, 로그, 컨트롤 소켓)는
`~/.xmux/` 아래에 있다.

## 컨트롤 소켓

실행 중인 인스턴스마다 이름이 있고 `~/.xmux/ctl-<name>.sock`에서 듣는다. 세션은
`<source>/<session>`으로 가리킨다. 소켓이 받는 것은 탐색 명령(`ping`, `status`,
`dump`, `rescan`, `switch`, `focus`, `width`, `toggle-auto-hide`, `quit`)과 세션
수명 명령 하나(`new-session`)다. kill, rename, window 명령은 없다. 세션을 고치는
일은 mux의 몫이다.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod/api
xmux send am focus terminal          # 겹치지 않는 이름 앞부분이면 된다
xmux send - dump                     # 하나만 돌고 있을 때는 `-`
```

없는 이름, 여러 개에 걸리는 앞부분, 여러 개가 도는데 쓴 `-`는 후보를 알려주는
오류로 끝난다. 짐작해서 고르지 않는다.

## 라이선스

MIT - [`LICENSE`](LICENSE)를 본다.

## 더 읽을 것

- [`docs/keybind.md`](docs/keybind.md) - 키 바인딩과 prefix 상세
- [`docs/requirements.md`](docs/requirements.md) - 동작 요구 사항
- [`docs/adr/`](docs/adr/) - 아키텍처 결정 기록
- [`CONTEXT.md`](CONTEXT.md) - 용어와 설계 개요
- [`AGENTS.md`](AGENTS.md) - 디렉터리별 작업 노트
