# xmux

[English](README.md) · 한국어

*여러 머신과 여러 mux의 세션을 한 화면에서 전환하는 도구.*

xmux는 터미널에 상주하는 Rust 프로그램이다. xmux는 자신을 실행한 터미널을
소유하고, 각 터미널 멀티플렉서(이하 mux)에 연결한 attach를 유지한 상태로 화면을
split view(분할 화면)로 표시한다. 왼쪽의 **nav**(세션 목록)에는 접근할 수 있는
세션마다 card가 하나씩 있고, 오른쪽의 **terminal view**(세션 화면)에는 선택한
세션의 실시간 화면이 있다. 선택을 옮기면 xmux는 terminal view를 그 세션의 화면으로
바로 전환한다.

xmux는 다음 사용자를 위한 도구다.

- **여러 원격 기기(주로 서버)에서 작업하는 사람**
  - xmux는 각 기기에 접속하고, 다시 연결하는 절차 없이 기기 사이의 세션을 전환한다.
- **각 기기에 앱을 추가로 설치하고 싶지 않은 사람**
  - xmux는 모든 동작을 ssh와 각 기기에 설치된 mux로 수행하므로, xmux를 사용하는
    기기 한 대에만 설치한다.
- **tmux를 신뢰하는 사람**
  - xmux는 tmux의 대안이 아니다. xmux는 tmux 세션에 접속하는 절차만 간편하게
    만든다.

![이 GIF는 같은 키 입력 속도로 나란히 녹화한 두 터미널을 보여준다. 왼쪽 터미널에서는
ssh로 gpu-01에 접속해 tmux 세션 목록을 확인하고 attach하기까지 7.1초가 걸린다.
오른쪽 터미널에서는 xmux nav에서 같은 세션을 선택하기까지 2.7초가 걸린다.](docs/assets/xmux-demo.gif)

- **모든 세션을 하나의 nav에.** 이 머신, 이 머신의 WSL 배포판, 접근할 수 있는 모든
  ssh host의 세션이 nav에 함께 표시된다.
- **실제 attach.** terminal view는 출력을 재구성한 화면이 아니라 실제 mux
  클라이언트다. terminal view는 mux가 그린 화면을 그대로 표시한다.
- **설정이 필요 없음.** host 목록은 `~/.ssh/config`와 이 머신이 이미 접근하는
  머신에서 만들어진다. xmux는 각 host가 실행하는 mux를 검사해 그 host의 mux를
  판별한다.
- **스크립트 조작.** 실행 중인 인스턴스마다 로컬 컨트롤 소켓으로 명령을 받는다.

**세션 전환**

![xmux는 card 한 개를 내려간 뒤 번호로 5번과 3번 세션으로 이동한다. terminal view는
선택한 세션을 따라 바뀐다.](docs/assets/xmux-nav-switch.gif)

**nav 폭 조절**

![xmux는 prefix 뒤 Ctrl-→를 누를 때마다 nav의 폭을 한 열씩 넓히고, Ctrl-←를 누를
때마다 한 열씩 좁힌다.](docs/assets/xmux-nav-resize.gif)

**nav 위치 이동**

![xmux는 prefix p를 누를 때마다 nav를 terminal view의 다음 변으로 옮긴다. nav는 위,
오른쪽, 아래를 거쳐 왼쪽으로 돌아온다.](docs/assets/xmux-nav-move.gif)

**nav 자동 숨기기**

![자동 숨기기가 켜진 상태에서 terminal view로 포커스를 옮기면 xmux는 nav를 숨기고
terminal view에 전체 폭을 할당한다. prefix Tab으로 nav에 포커스를 옮기면 nav가 다시
나타난다.](docs/assets/xmux-nav-autohide.gif)

## 빠른 시작

### 1. 설치

**기본 설치 (권장)**

macOS, Linux, WSL, Android Termux:

```sh
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/zer0ken/xmux/releases/latest/download/install.ps1 | iex
```

Windows CMD:

```batch
curl -fsSL https://github.com/zer0ken/xmux/releases/latest/download/install.cmd -o install.cmd && install.cmd && del install.cmd
```

`The token '&&' is not a valid statement separator` 오류는 CMD용 명령을
PowerShell에서 실행했다는 뜻이다. `'irm' is not recognized as an internal or
external command` 오류는 PowerShell용 명령을 CMD에서 실행했다는 뜻이다.
PowerShell의 프롬프트는 `PS C:\`로 시작하고, CMD의 프롬프트는 `PS` 없이 `C:\`다.

설치 스크립트는 다음 순서로 동작한다.

- 실행한 머신에 맞는 빌드를 다운로드한다.
- 릴리스가 공개한 체크섬과 일치하지 않는 빌드는 설치하지 않는다.
- 관리자 권한 없이 `xmux` 명령을 `PATH`에 추가한다.

바뀐 `PATH`는 설치 후에 새로 연 터미널에 적용된다.

> 기본 설치는 `xmux update`로 갱신한다. xmux는 새 릴리스가 나오면 시작할 때
> 알리지만, 새 릴리스를 스스로 설치하지는 않는다.

**Homebrew** (macOS)

```sh
brew install zer0ken/xmux/xmux
```

> Homebrew 설치는 스스로 갱신되지 않는다. `xmux update`나
> `brew upgrade zer0ken/xmux/xmux`가 새 릴리스를 설치한다.

**WinGet** (Windows)

```powershell
winget install --id zer0ken.xmux
```

> WinGet 설치는 스스로 갱신되지 않는다. `xmux update`나
> `winget upgrade --id zer0ken.xmux`가 새 릴리스를 설치한다. winget 카탈로그는
> 커뮤니티 저장소의 검토를 거쳐 갱신되므로 최신 릴리스보다 늦을 수 있다. 기본
> 설치는 항상 최신 릴리스를 받는다.

**Cargo** (Rust가 있는 모든 OS)

```sh
cargo install xmux
```

[`INSTALL.md`](INSTALL.md)는 나머지 설치 방법을 다룬다.

- 버전 고정
- 설치 디렉터리 변경
- 사전 빌드 바이너리
- 소스 빌드
- `xmux uninstall`을 이용한 제거

### 2. 설치 확인

```sh
xmux version
xmux doctor
```

`xmux doctor`는 실행 중인 xmux와 그 설치 위치를 알린 다음, 설정과 source별 접근
가능 여부를 점검한다.

원격 host를 사용하려면 xmux를 실행하는 머신에 `ssh`가 있어야 하고, 각 host에
[지원하는 mux](#지원-mux)가 하나 이상 있어야 한다.

### 3. 첫 실행

```sh
xmux
```

xmux는 이 머신의 세션으로 nav를 바로 채우고, 원격 host의 세션은 각 host가 응답하는
대로 추가한다. nav에서 쓰는 키는 다음과 같다.

- `↑` / `↓`는 선택을 옮긴다.
- `Enter`는 키보드 입력을 선택한 세션으로 보낸다.
- `Ctrl-g` 다음 `Tab`은 포커스를 nav로 되돌린다.
- `Ctrl-g ?`는 모든 키를 표시하고, `Ctrl-g q`는 xmux를 종료한다.

## 지원 mux

| 플랫폼     | mux                                                        |
| ---------- | ---------------------------------------------------------- |
| unix 계열  | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios`, `herdr` |
| Windows    | `psmux`, `herdr`                                            |

xmux는 host가 어느 바이너리로 응답하는지를 검사해 그 host의 mux를 판별한다.
따라서 host마다 다른 mux가 설치되어 있어도 설정이 필요 없다.

## 사용법

```sh
xmux                          # 앱을 실행한다
xmux ls                       # 접근할 수 있는 모든 세션을 나열한다 (스크립트용)
xmux attach <source> <name>   # 세션 하나에 바로 attach 한다, 예: xmux attach prod api
xmux doctor                   # 설정과 source별 접근 가능 여부를 점검한다
xmux instances                # 실행 중인 인스턴스를 나열한다
xmux send <name> <command…>   # 그중 하나를 컨트롤 소켓으로 조작한다
xmux update                   # 설치된 바이너리를 갱신한다
xmux uninstall                # 확인을 받은 뒤 설치된 xmux를 제거한다
xmux version
```

nav는 왼쪽에 있고, 오른쪽의 terminal view는 선택한 세션의 실시간 화면을 표시한다. 키보드
포커스는 두 view 중 한쪽에만 있다.

## 키

nav에 포커스가 있을 때 nav가 받는 키는 다음과 같다.

| 키                         | 동작                                                                  |
| -------------------------- | --------------------------------------------------------------------- |
| `↑` / `↓` (또는 `k` / `j`) | card 한 개 이동 (양 끝에서 순환한다)                                  |
| `←` / `→` (또는 `h` / `l`) | 이전 / 다음 `host/mux` 구역으로 이동한다. host card들은 한 구역으로 취급한다 |
| `Home` / `End`             | 첫 card / 마지막 card로 이동                                          |
| `PageUp` / `PageDown`      | card 열 개 이동                                                       |
| `Enter`                    | 선택한 세션의 terminal view로 포커스를 옮긴다                         |
| `prefix 1`-`prefix 9`      | 왼쪽 열의 번호로 세션을 선택한다 (10 이상은 계속 입력한다)            |
| `prefix n`                 | 선택한 host에 새 세션을 만든다                                        |
| `prefix /`                 | card를 퍼지 필터로 좁힌다                                             |
| `prefix r`                 | 다시 스캔한다. 머신 목록과 각 source의 세션을 모두 갱신한다           |
| `prefix L`                 | 선택한 SSH host에서 로그아웃한다                                      |

xmux에는 tmux의 `set -g prefix`처럼 자체 prefix가 있다. 기본값은 `Ctrl-g`이며,
`[ui] prefix` 설정이 이 값을 대체한다. 조합키는 prefix 다음에 누르는 키 하나다.

| 조합키       | 동작                                      |
| ------------ | ----------------------------------------- |
| `prefix q`   | 종료                                      |
| `prefix ?`   | 키와 기호 도움말 토글 (입력으로 검색)     |
| `prefix m`   | 작업 결과와 배경 사건 기록 토글           |
| `prefix Tab` | nav와 terminal view 사이의 포커스 이동    |
| `prefix p`   | nav를 terminal view의 다음 변으로 옮긴다  |

prefix를 누르면 prefix 표시 옆에 그 prefix로 쓸 수 있는 키 전체를 나열한 상자가
열린다. 마우스로 card를 클릭하면 그 card가 선택되고, terminal view를 클릭하면 포커스가
terminal view로 옮겨진다. 설치 후 처음 키를 누르면 xmux는 설정된 prefix와 도움말 키를
잠시 안내하고, 안내를 표시했다는 사실을 기록한다. 나머지 키는 [`docs/keybind.md`](docs/keybind.md)에 있다.

## host와 source

**host**는 mux가 동작하고 있고 xmux가 접근할 수 있는 머신이다. **source**는
host 하나 위의 mux 하나다. 그래서 psmux와 zellij가 함께 동작하는 host는 source
두 개가 된다. source 이름은 host가 여러 mux를 제공하면 `local:psmux` 형식이고,
하나만 제공하면 `prod` 형식이며, nav에 표시되는 이름이 곧 source 이름이다. 명령은
세션을 source와 세션 이름으로 따로 지정한다(예: `switch prod api`).

xmux는 원격 host를 앱이 실행된 뒤에 조회하므로, source는 각 host가 응답하는 대로
하나씩 나타난다.

### host 로그인

ssh가 스스로 알아내는 값만으로 접속하지 못한 원격 host는 `login required`(`?`
표시)로 나타난다. terminal view에 있는 그 host의 패널이 로그인을 받는다.

1. 패널에는 ssh가 묻지 않는 값을 입력하는 칸이 있고, 각 칸은 ssh가 썼을 값으로
   시작한다.
   - 주소
   - 포트
   - 사용자 이름
   - 마스킹된 비밀번호 (선택)
2. 제출하면 xmux가 그 값을 ssh에 전달하고, 호스트 키 확인과 비밀번호 질문에
   xmux가 대신 응답한다. 따라서 로그인에 추가 입력이 필요 없다. Esc는 시도를
   끝낸다.
3. 로그인에 성공하면 xmux가 그 host만 다시 조회하고, 패널은 찾아낸 세션 목록으로
   바뀐다. 제출한 값은 그 머신에 기록되므로, 이후 xmux가 그 머신에서 실행하는
   모든 명령이 같은 값으로 접속한다.

입력한 값으로 끝낼 수 없는 로그인은 서버가 요구한 것을 알린다. 성공한 로그인이
남기는 것은 라디오 선택 하나가 정한다.

- 아무것도 남기지 않음
- 입력한 값을 `~/.ssh/config` 스탠자로 기록
- 사용자의 공개키를 그 host에 등록해 비밀번호를 다시 묻지 않게 함

xmux가 추가하는 key 줄은 comment 끝에 `xmux-registered` 표시를 포함한다. sshd는 이
표시를 인증에 사용하지 않으며, xmux는 이 표시로 자기가 추가한 줄과 사용자가 추가한 줄을
구분한다. 같은 key 본문이 이미 있는 host에는 comment와 option이 달라도 줄을 추가하지
않으며, 기존 줄은 표시 없이 남는다.

xmux는 공개키를 등록한 뒤 그 키만 허용하는 로그인을 한 번 따로 실행한다. 등록 결과는
그 로그인이 명령을 실행했을 때만 성공이다. host가 키를 받아들인 뒤 세션을 열지 못하면
xmux는 서버의 오류를 실패로 알리고 이번 등록이 추가한 줄을 제거하므로, 그 host에는
계속 비밀번호로 접속할 수 있다. 그 로그인을 시도조차 하지 못하면 xmux는 키를 남겨 두고
검증하지 못했다고 알린다.

정보 화면의 `SSH login` 행은 선택한 세션의 display 연결이 보고한 SSH 인증 방식을
표시한다. host card에서는 그 머신에서 마지막으로 관찰한 방식을 표시한다. SSH가 인증
방식을 보고하지 않고 연결을 재사용하면 이 행은 `not observed`를 표시한다. xmux가
보관하던 비밀번호가 사라지면 xmux는 그 머신의 메타데이터 연결과 display 연결을 닫는다.
다시 접속하려면 새로 로그인하거나 명시적으로 다시 스캔해야 한다.

SSH host에서 `prefix L`을 누르면 확인 창이 열린다. 확인 창은 선택한 세션, 그 세션에서
관찰한 SSH 로그인 방식, 보관한 비밀번호와 이 PC의 key의 처리, 연결을 닫을 머신을 표시한다.
`logout`을 입력하면 xmux는 host에서 이 PC의 공개키를 제거한 뒤, 메모리에 보관한
비밀번호를 지우고 그 머신의 연결과 SSH master를 닫는다. xmux는 연결을 닫기 전에 host의
key 파일에서 이 PC의 공개키와 key 종류, key 본문이 같은 줄을 찾는다. option과 comment는
비교에서 제외한다. `xmux-registered` 표시가 있는 줄은 바로 제거한다. 표시가 없는 줄은
xmux가 추가하지 않은 줄이고, 그 줄을 제거하면 xmux 밖의 ssh도 그 key를 사용하지 못한다.
그래서 xmux는 같은 위치에 확인 창을 한 번 더 연다. `remove`를 입력하면 그 줄도 제거하고,
Esc를 누르면 그 줄은 남긴다. host에 연결할 수 없거나 제거에 실패해도 로그아웃은 비밀번호와
연결 정리를 계속하고, toast로 key가 남았다는 사실과 이유를 알린다. ssh config는 바꾸지
않는다. 다시 스캔하면 host가 아직 받아들이는 key로만 다시 접속하고, 그런 key가 없으면 다시
로그인해야 한다.

## roster

roster는 xmux가 host로 제공할 머신을 조립한다. roster는 provider 세 개에서 ssh
대상 이름을 모은다.

| provider        | 제안하는 이름                                                  |
| --------------- | -------------------------------------------------------------- |
| ssh config      | `~/.ssh/config`의 별칭                                         |
| neighbours      | 이 머신이 이미 한 홉으로 접근하고 ssh에 응답하는 머신          |
| WSL             | 이 머신의 WSL 배포판                                           |

xmux는 roster를 시작할 때와 다시 스캔할 때마다 조립한다. `local`은 ssh 없이
접근하는 이 머신 자체라 roster에 포함되지 않으며, 어떤 provider도 이름을 제안하지
않는 머신은 xmux가 다루지 않는다. `[discovery]` 표는 provider를 하나씩 끄는
설정이며, 기본값은 모두 켜져 있다. 각 source의 첫 접속과 세션 목록 조회는 10초의
스캔 제한을 함께 쓴다. 10초 안에 응답하지 않은 card는 스캔을 멈추고 timeout을 표시한다.

모든 provider는 ssh 대상 이름을 산출하고, xmux는 어느 provider가 이름을 제안했든
같게 동작한다. xmux는 제안한 provider를 이름과 함께 보관했다가 host에 접근하지
못하게 되면 그 화면에 표시하므로, 사용자는 어느 provider를 점검하거나 끌지 판단할
수 있다. provider가 실행하는 명령이 없거나, 운영체제가 응답하지 않거나, 출력을
해석하지 못하면, roster는 그 provider를 오류로 처리하지 않고 빈 목록으로 처리한다.
따라서 한 provider가 실패해도 다른 provider가 제안한 host는 계속 제공된다.

### neighbour 탐색

neighbours provider는 운영체제의 네트워크 상태를 읽으므로, VPN 클라이언트를
설치하거나 계정을 만들 필요가 없다. 이 provider는 운영체제에 직접 질의하므로(Linux와
Android에서는 netlink, Windows에서는 IP Helper), 명령줄 도구가 없거나 Android처럼
명령줄 도구를 거부하는 환경에서도 동작한다.

- **접근할 수 있는 머신.** 기록 두 개가 이를 알린다. 라우팅 테이블에는 메시 VPN이
  피어마다 기록한 경로가 있고, 피어 여러 개를 한 경로로 묶은 항목은 그 안의
  주소들로 분해해 읽는다. neighbour 테이블(ARP 캐시)에는 같은 링크에서 이 머신과
  실제로 프레임을 주고받은 머신이 있다. Android처럼 운영체제가 neighbour 테이블을
  거부하면, provider는 이 머신이 속한 링크를 주소 단위로 질의한다.
- **그중 머신인 항목.** 이름 해석에 실패한 항목, 그리고 하드웨어 주소 하나가 여러
  주소를 대표하는 항목(서브넷을 대신 응답하는 라우터)은 머신을 가리키지 않는다.
  provider는 남은 항목마다 ssh에 응답하는지 확인한다. 같은 스위치에 연결된
  프린터는 neighbour이지만 host가 아니기 때문이다.
- **머신의 이름.** provider는 시스템 리졸버에 먼저 질의한다. 메시 VPN이 부여한
  이름은 이미 리졸버에 있으므로, 피어는 자기 네트워크가 부여한 이름으로 나타난다.
  리졸버가 모르는 머신에는 그 머신 자신에게 이름을 질의하며, 그 머신은 등록 여부와
  무관하게 mDNS로 자기 이름을 응답한다. provider는 이 머신이 다시 주소로 해석할 수
  있는 이름만 사용한다. 그 이름이 ssh에 전달되는 값이기 때문이다. 이름으로 접근할
  수 없는 머신은 주소를 그대로 이름으로 사용한다.

## 설정

설정은 모두 선택 사항이다. xmux는 `~/.config/xmux/config.toml`을 읽는다.

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # 이 머신들은 목록에서 숨긴다

[local]
mux = "auto"          # "auto"(기본값)는 이 머신에 설치된 mux 전부를 뜻한다.
                      # ["psmux", "zellij", "abduco", "tuios", "herdr"]처럼 목록도 받는다.

[ui]
theme = "auto-dark"                  # 내장 ANSI 테마: "auto-dark"(기본값) 또는
                                      # "auto-light"(밝은 터미널용)
prefix = "C-g"                        # xmux의 prefix (예: C-g, C-Space, C-b)
auto-hide-nav = false                 # auto-hide-nav의 초기 상태
renumbering = true                     # 카드 번호를 현재 nav 정렬 순서대로 부여한다
notifications = true                  # 작업 결과를 toast로 띄운다 (끄더라도 prefix m 기록에는 남는다)
braille-animation = true             # 스캔 화면과 호스트 화면의 중앙 점자 X 표시
nav-position = "left"                 # nav의 기본 위치 (left|top|right|bottom)
max-fps = 30                          # xmux의 초당 최대 화면 갱신 횟수 (10~120)
view-active-border-style = "green"    # 포커스된 view border의 색
hint-bar-style = "bg=blue,fg=white"   # 힌트 바의 색 (tmux status-style)
primary = "brightwhite"               # 역할별 색 오버라이드: primary, secondary,
accent = "lightgreen"                 # accent, decoration, warning, error, disabled,
bar-bg = "colour235"                  # 힌트 바의 bar-bg / bar-fg / bar-accent

[update]
check = true                          # 하루에 한 번 새 릴리스가 있는지 묻는다

[[hosts]]
ssh = "prod"          # ssh-config 별칭
mux = "tmux"          # 생략하거나 "auto"이면 호스트가 답한 mux 전부
```

- **실시간 반영.** `config.toml`이 바뀌면 xmux는 재시작 없이 다음 `[ui]` 표시
  설정을 다시 적용한다.
  - 테마
  - 역할별 색 오버라이드
  - selection-style
  - hint-bar-style
  - view-border 스타일
  - max-fps
  - notifications
  - renumbering
  - braille-animation
  - nav-position

  host와 roster 변경은 `prefix r`로 다시 스캔해야 반영된다.
- **nav 위치.** nav는 terminal view의 네 변 중 한 곳에 붙는다(왼쪽이나 오른쪽의
  세로 열, 위나 아래의 가로 띠). `[ui] nav-position`이 기본 위치를 정하며, nav는
  스스로 움직이지 않는다. `prefix p`는 nav를 시계 방향으로 한 변 옮기고(left →
  top → right → bottom → 기본값) 그 선택을 `~/.xmux/nav_position`에 기록한다. 이
  기록은 키가 기본값으로 돌아올 때까지 설정보다 우선한다.
- **host.** xmux는 host를 먼저 `~/.ssh/config`에서 읽는다. 설정 파일은 그 탐색
  결과를 보완하며 대체하지 않는다.
- **상태.** 다음 실행까지 남는 상태는 `~/.xmux/` 아래에 있다.
  - 마지막에 선택한 세션
  - auto-hide-nav 토글
  - 고정한 nav 위치
  - 로그
  - 컨트롤 소켓

## 컨트롤 소켓

실행 중인 인스턴스마다 이름이 있고, 각 인스턴스는 `~/.xmux/ctl-<name>.sock`에서
요청을 받는다. 명령은 세션을 source와 세션 이름으로 따로 지정하며(`switch
<source> <session>`), nav에는 `<source>/<session>`으로 합쳐 표시된다. 소켓이 받는
명령은 탐색 명령(`ping`, `status`, `dump`, `rescan`, `switch`, `focus`, `width`,
`toggle-auto-hide`, `quit`)과 세션 수명 명령 하나(`new-session`)다. kill,
rename, window 명령은 없다. 세션을 편집하는 일은 mux가 담당한다.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod api
xmux send am focus terminal          # 겹치지 않는 이름 앞부분으로 지정한다
xmux send - dump                     # 하나만 실행 중일 때는 `-`
```

xmux는 다음 경우에 후보를 알리는 오류로 끝내며, 짐작해서 하나를 고르지 않는다.

- 없는 이름
- 여러 인스턴스에 걸리는 이름 앞부분
- 여러 인스턴스가 실행 중일 때의 `-`

## 라이선스

MIT 라이선스다. 전문은 [`LICENSE`](LICENSE)에 있다.

## 더 읽을 것

- [`INSTALL.md`](INSTALL.md) - 모든 설치 방법, 갱신, 버전 고정
- [`docs/keybind.md`](docs/keybind.md) - 키 바인딩과 prefix 상세
- [`docs/requirements.md`](docs/requirements.md) - 동작 요구 사항
- [`docs/principles.md`](docs/principles.md) - 설계 원칙
- [`CONTEXT.md`](CONTEXT.md) - 용어와 설계 개요
- [`AGENTS.md`](AGENTS.md) - 디렉터리별 작업 노트
