# xmux

*호스트를 넘나드는 터미널 멀티플렉서 전환기. tmux의 `prefix + s` / `switch-client`를 그대로 쓰되, 모든 머신에 닿는다.*

xmux는 Rust로 만든 상주형 터미널 관리자다. 자신을 띄운 터미널을 직접 소유하고
살아 있는 mux 화면 연결을 유지하면서 화면을 둘로 나눠 보여준다. 왼쪽에는 닿을 수
있는 모든 세션의 **목록**이, 오른쪽에는 지금 고른 세션의 **실시간 화면**이 뜬다.
목록에서 커서를 옮기면 오른쪽 창이 그 세션으로 곧장 바뀐다. 로컬 psmux 세션이든
ssh 너머의 tmux 세션이든, 또 다른 머신의 zellij 세션이든 똑같다. 떼었다 다시 붙이는
절차도, 목록 창을 거치는 왕복도 없다.

목표는 tmux에서 이미 익숙한 `switch-client` 경험을 호스트 경계 밖까지 넓히는
것이다. 설정해 둔 어느 머신의 mux 세션이든, 터미널 하나에서 즉시 제자리에서
오간다.

## 기능

- **모든 호스트를 하나의 목록으로.** 세션 하나가 카드 하나, 로컬과 ssh를
  한 화면에 담는다. 호스트는 `~/.ssh/config`에서 알아서 찾아낸다.
- **호스트를 넘는 제자리 전환.** 다른 머신의 세션을 고르면 같은 터미널 창 안에서
  다시 연결되고, 같은 서버의 다른 세션을 고르면 클라이언트가 제자리에서 넘어간다.
  손으로 뗄 일도, 원격에 뭔가를 깔 일도 없다.
- **미리보기가 아니라 실시간 화면.** 오른쪽 창은 세션마다 붙는 진짜 PTY 연결이라,
  눈에 보이는 것이 세션의 실제 화면이다. 목록을 옮겨 다니는 동안에도 계속 살아
  있다.
- **직교하는 두 축.** `Mux` 축(**tmux**·**psmux**·**zellij**)과 `Transport` 축(**로컬**·**ssh**)이
  자유롭게 조합된다. 어느 mux든 어느 전송 방식 위에서든, 서로를 몰라도 맞물린다.
- **꼭 필요한 곳은 폴링 없이.** tmux 호스트는 컨트롤 모드(`-CC`)로 추적한다. psmux와
  zellij 호스트는 폴링한다. 둘 다 밀어주는 통로가 없기 때문이다. 어느 쪽이든 목록은
  서버를 그대로 비춘다. 판단 기준은 늘 서버 쪽에 있다.
- **넘나들기에만 집중한다.** 세션을 오가고 거르고 번호로 뛰어가고, 세션이 없는
  호스트에 새 세션 하나를 만든다. 이름 바꾸기와 없애기, 창과 pane 작업은 그걸 이미 잘하는 mux에
  남겨 둔다.
- **이름 붙은 인스턴스.** 실행 중인 인스턴스마다 이름이 있어서
  `xmux send <name> <command>`로 특정 인스턴스를 골라 구동한다
  ([컨트롤 소켓](#컨트롤-소켓) 참고).

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
`psmux`다. 둘은 같은 명령어 언어를 쓴다. `zellij`도 된다. 이쪽은 명령어 언어가 아예
달라서 자기 CLI로 따로 다룬다. 머신의 mux는 그 바이너리가 스스로 답하는 이름으로
알아내므로, 호스트마다 셋이 섞여 있어도 설정할 것이 없다.

## 사용법

인자 없이 실행하면 대화형 분할 화면이 열린다:

```sh
xmux                          # 대화형 목록 + 실시간 화면 앱
xmux ls                       # 닿을 수 있는 모든 세션 나열 (스크립트용)
xmux attach <source>/<name>   # 세션 하나에 바로 연결, 예: xmux attach prod/api
xmux doctor                   # 설정과 호스트별 접속 가능 여부 점검
xmux instances                # 실행 중인 인스턴스 목록
xmux send <name> <command…>  # 그중 하나를 컨트롤 소켓으로 구동
xmux version
```

### 앱 안에서

왼쪽 창은 세션 목록, 오른쪽 창은 고른 세션의 실시간 화면이다. 키보드 초점은 한 번에 한
영역에만 놓인다.

**목록 이동:**

| 키 | 동작 |
|---|---|
| `↑` / `↓` (또는 `k` / `j`) | 카드 한 칸 이동 (양쪽 끝에서 순환) |
| `Home` / `End` | 첫 카드 / 마지막 카드로 |
| `PageUp` / `PageDown` | 카드 열 개씩 이동 |
| `Enter` | 고른 세션의 실시간 화면으로 초점 옮기기 |
| `prefix 0`~`prefix 9` | 왼쪽에 붙은 번호로 세션 바로 이동 (10 이상은 이어서 입력) |
| `prefix n` | 고른 호스트에 새 세션 만들기 |
| `/` | 목록 퍼지 필터 |
| `prefix r` | 모든 호스트 다시 훑기 |

마우스도 된다. 행을 클릭하면 선택되고, 오른쪽 창을 클릭하면 그쪽으로 초점이 간다.
목록 위에서 휠을 굴리면 스크롤한다.

**Prefix 키.** xmux는 tmux의 `set -g prefix`처럼 자체 prefix를 둔다. 기본값은
`Ctrl-g`이고 `[ui] prefix`로 바꾼다(아래 참고). prefix를 누른 다음:

| 키 조합 | 동작 |
|---|---|
| `prefix q` | xmux 종료 |
| `prefix ?` | 키 도움말 켜고 끄기 |
| `prefix t` | 목록 자동 숨김 켜고 끄기 (화면에 초점을 주면 전체 폭을 쓴다) |
| `prefix h` / `prefix l` (또는 `prefix Ctrl-←/→`) | 목록 좁히기 / 넓히기 |
| `prefix Tab` / 화살표 / `Esc` | 목록과 화면 사이 초점 이동 |
| `prefix prefix` | prefix 바이트 하나를 초점 세션에 그대로 보내기 |

nav의 맨 아래 줄이 상태 표시줄이다. 평소에는 prefix만 보여준다. prefix를 누르면 창
전체 폭으로 넓어져 실시간 화면 위에 떠서, 그 prefix가 여는 키 목록을 보여준다.

prefix에 관한 자세한 내용은 [`docs/keybind.md`](docs/keybind.md)를 참고한다.

## 한 머신에 여러 mux

머신 하나가 mux를 여러 개 동시에 띄울 수 있고, xmux는 그 각각을 별도 소스로 다룬다.
이 머신에 관해서는 적을 필요조차 없다. `mux`를 기본값으로 두면 xmux가 지원하는 mux 중
이 컴퓨터에 실제로 깔려 있는 것을 찾아 각각을 소스로 올린다. psmux 옆에 zellij를 깔면
다음 실행부터 그냥 보인다.

특정 조합을 고정하고 싶을 때는 이 머신이든 원격이든 목록으로 적는다.

```toml
[local]
mux = ["psmux", "zellij"]

[[hosts]]
ssh = "prod"
mux = ["tmux", "zellij"]
```

그러면 목록에 `local/psmux`가 자기 세션들 위에, `local/zellij`가 자기 세션들 위에 나란히
올라온다. 둘 사이를 오가는 것은 호스트 사이를 오가는 것과 같은 키다. mux를 여러 개 준
머신의 소스 이름은 `local:psmux`, `local:zellij`가 되고, 이 이름이 `xmux ls`가 찍고
`xmux send switch`가 받는 이름이다. mux를 하나만 준 머신은 예전 그대로 맨 이름(`local`,
`prod`)을 지킨다. `exclude`는 머신을 가리키므로, 하나를 빼면 그 위의 mux가 전부 빠진다.

그 머신에 깔려 있지 않은 mux를 적었다면 조용히 넘기지 않는다. 그 소스는 mux가 낸 메시지와
함께 닿을 수 없는 것으로 뜬다. 적어 넣은 이름은 뜻이 있는 이름이기 때문이다. 반대로 찾아낸
목록에는 이 규칙이 적용되지 않는다. 적어 넣은 이름이 없으니, 답한 mux만 올라온다. 지금
어느 쪽인지는 `xmux doctor`가 알려준다.

원격 호스트는 검사하지 않는다. 원격의 mux는 설정한 값이고, 없으면 `tmux`다. 호스트마다
mux마다 물어보면 화면에 아무것도 뜨기 전에 mux 수만큼 ssh 연결을 열어야 한다.

## 호스트 목록의 출처

기본값은 `~/.ssh/config`의 `Host` 별칭과 `local`이다. tailnet에서 목록을 받아올 수도
있다. 그러면 닿을 수 있는 기계가 곧 xmux가 보여주는 기계이고, 손으로 맞춰 둘 것이 없다.

```toml
[discovery]
ssh-config = true   # 기본값. false로 두면 아래 제공자만 쓴다
tailscale = false   # 기본값. 이 기계가 속한 tailnet의 온라인 피어
```

tailnet 피어는 DNS 라벨(`jupiter00`)로 올라온다. 실제로 이름이 풀리는 형태이고, ssh
설정에 적었을 이름과 같다. 오프라인 피어와 이 기계 자신은 제외한다. 이 기계는 `local`
이고, 오프라인 피어는 훑을 것이 없다. 답하지 못하는 제공자(CLI 없음, 데몬 정지)는 실행을
실패시키지 않고 아무것도 보태지 않는다. `~/.ssh/config`의 이름이 먼저 오고 제공자가 같은
이름을 다시 보고해도 아무 일도 없으므로, 직접 설정한 호스트는 준 자리를 그대로 지킨다.

## 설정

설정은 전부 선택 사항이다. 아무것도 설정하지 않는 것이 기본이다. xmux는
`~/.config/xmux/config.toml`을 읽는다:

```toml
# 로컬 머신에서 쓸 mux.
[local]
mux = "auto"          # "auto"(기본값): xmux가 지원하는 mux 중 이 컴퓨터에 실제로
                      # 깔려 있는 것 전부. 관례적인 것(Windows는 psmux, 그 외는 tmux)이
                      # 맨 앞에 온다.
                      # "tmux", "psmux", "zellij"도 받고,
                      # ["psmux", "zellij"]처럼 목록도 받는다.

# 발견된 ssh 호스트의 mux를 바꾸거나, ssh-config 발견이
# 잡아내지 못한 호스트를 더한다.
[[hosts]]
ssh = "prod"          # ssh-config 별칭
mux = "tmux"          # 생략하면 "tmux"

# 이 ssh 별칭들은 목록에서 숨긴다.
exclude = ["bastion"]

[ui]
prefix = "C-g"                        # xmux prefix (예: C-g, C-Space, C-b)
auto-hide-nav = false                # 목록 자동 숨김 초기 상태
view-active-border-style = "green"    # 초점 뷰 테두리 색 (tmux 색 표기)
view-border-style = "default"         # 비초점 뷰 테두리 색
view-border-hover-style = "yellow"    # 크기 조절 드래그 호버 표시
hint-bar-style = "bg=blue,fg=white"   # 힌트 바 색 (tmux status-style; 비우면 내장 바)
selection-style = "#2d4f6b"           # 선택된 카드의 배경. 비우면(기본값) 반전 영상,
                                      # 즉 터미널 테마 자신의 선택 표시를 쓴다.
```

호스트는 먼저 `~/.ssh/config`에서 온다. 접속 정보(사용자, 포트, 키, 점프 호스트)를
거기서 가져온다. 설정 파일은 그 발견을 보강할 뿐 대체하지 않는다. `xmux doctor`를
돌리면 확정된 로컬 mux, ssh 사용 가능 여부, 호스트별 접속 가능 여부를 보여준다.
상태 정보(마지막으로 고른 세션, 지금 켜진 목록 자동 숨김 값, 로그, 컨트롤 소켓)는
`~/.xmux/` 아래에 있다.

## 컨트롤 소켓

실행 중인 인스턴스마다 이름이 있다. 시작할 때 `<형용사>-<명사>` 꼴로 자동 생성되고,
`xmux --name <name>`으로 직접 지정할 수도 있다. 그 이름으로
`~/.xmux/ctl-<name>.sock`을 듣는다. 세션은 `<source>/<session>`으로 지정한다.

이동과 표시 명령은 `ping`, `status`, `dump`, `rescan`, `switch <source>/<session>`,
`focus <nav|terminal>`, `width <delta>`(목록 폭을 부호 있는 열 수만큼 조정한다. 절대
폭이 아니라 증분이다), `toggle-auto-hide`, `quit`이다. 세션 수명을 다루는 명령은
`new-session <source> [name]` 하나뿐이다. 없애기·이름 바꾸기·창 관련 명령은
없다. 키를 없앤 이유와 같다. 세션을 고치는 일은 mux가 한다. 저수준 키·바이트
주입용으로 불안정한 `raw:` 네임스페이스를 예약해 두었다.

```sh
xmux instances                       # NAME · PID · CWD · TTY · 표시 중 · 초점
xmux send amber-otter switch prod/api
xmux send am focus terminal          # 겹치지 않는 이름 앞부분이면 된다
xmux send - dump                     # 하나만 돌고 있을 때는 `-`
```

없는 이름, 여러 개에 걸리는 앞부분, 여럿이 돌고 있을 때의 `-`는 모두 후보를 알려주는
에러다. 짐작해서 보내지 않는다. 명령을 주지 않으면 stdin에서 한 줄씩 읽는다. 거부된
명령은 0이 아닌 코드로 끝난다.

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
