# xmux

[English](README.md) · 한국어

*여러 호스트의 터미널 멀티플렉서 세션을 한자리에서 전환하는 도구.*

xmux는 터미널에 상주하는 Rust 프로그램이다. xmux는 자신을 실행한 터미널을
직접 소유하고, 각 터미널 멀티플렉서(이하 mux)에 연결한 attach를 유지한 상태로
화면을 분할해 표시한다. 왼쪽 창에는 접근할 수 있는 모든 세션의 **목록**이,
터미널 뷰에는 선택한 세션의 **실시간 화면**이 표시된다. 목록에서 커서를
움직이면 터미널 뷰가 해당 세션의 화면으로 즉시 전환된다.

xmux는 다음 사용자를 위한 도구다.

- **여러 원격 기기(주로 서버)에서 작업하는 사람**
  - xmux는 여러 기기에 빠르게 접속하고 기기 사이를 전환한다.
- **각 기기에 앱을 추가로 설치하고 싶지 않은 사람**
  - xmux는 모든 동작을 ssh와 각 기기에 설치된 mux에 의존하므로, xmux를 사용할
    기기 한 대에만 설치한다.
- **tmux를 신뢰하는 사람**
  - xmux는 tmux의 대안이 아니다. xmux는 tmux 세션에 접속하는 절차만 간편하게
    만든다.

![xmux의 분할 화면. 왼쪽 목록에 이 머신의 psmux 세션과 WSL 배포판 안의 tmux
세션이 함께 있고, 오른쪽에는 선택한 세션의 터미널 뷰가 차 있다.](docs/assets/xmux.png)

- **모든 세션을 한 목록에.** 이 머신, 이 머신의 WSL 배포판, 닿을 수 있는 모든
  ssh 호스트의 세션이 나란히 보인다.
- **진짜 attach.** 터미널 뷰는 출력을 재구성한 것이 아니라 실제 mux 클라이언트다.
  보이는 화면이 곧 mux가 그린 화면이다.
- **설정할 것이 없다.** 호스트는 `~/.ssh/config`와 이 머신이 이미 닿는 머신에서
  온다. 각 호스트의 mux는 그 호스트가 실행하는 것을 보고 판별한다.
- **스크립트로 조작한다.** 실행 중인 인스턴스마다 로컬 컨트롤 소켓으로 명령을 받는다.

## 빠른 시작

### 1. xmux 설치

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

`The token '&&' is not a valid statement separator` 오류가 나오면 CMD가 아니라
PowerShell에 있는 것이다. `'irm' is not recognized as an internal or external
command` 오류가 나오면 PowerShell이 아니라 CMD에 있는 것이다. PowerShell의
프롬프트는 `PS C:\`로 시작하고, CMD의 프롬프트는 `PS` 없이 `C:\`다.

스크립트는 이 머신에 맞는 빌드를 내려받고, 릴리스가 공개한 체크섬과 일치하지
않으면 설치하지 않으며, 관리자 권한 없이 `xmux` 명령을 `PATH`에 올린다. 설치 후에는
새 터미널을 열어야 바뀐 `PATH`가 적용된다.

> 기본 설치는 `xmux update`로 갱신한다. 새 버전이 나오면 xmux가 시작할 때
> 알려 주지만, 스스로 설치하지는 않는다.

**Homebrew** (macOS)

```sh
brew install zer0ken/xmux/xmux
```

> Homebrew 설치는 스스로 갱신되지 않는다. 새 릴리스를 받으려면 `xmux update`나
> `brew upgrade zer0ken/xmux/xmux`를 실행한다.

**WinGet** (Windows)

```powershell
winget install --id zer0ken.xmux
```

> WinGet 설치는 스스로 갱신되지 않는다. `xmux update`나
> `winget upgrade --id zer0ken.xmux`를 실행한다. winget 카탈로그는 커뮤니티
> 리포의 검토를 거쳐 갱신되므로 최신 릴리스보다 늦을 수 있다. 기본 설치는 늘
> 최신 릴리스를 받는다.

**Cargo** (Rust가 있는 모든 OS)

```sh
cargo install xmux
```

버전 고정, 설치 디렉터리 변경, 사전빌드 바이너리, 소스 빌드는
[`INSTALL.md`](INSTALL.md)에 있다.

### 2. 설치 확인

```sh
xmux version
xmux doctor
```

`xmux doctor`는 실행 중인 xmux와 그 설치 위치를 알려 준 다음, 설정과 소스별
접근 가능 여부를 점검한다.

원격 호스트를 사용하려면 xmux를 실행하는 머신에 `ssh`가 있어야 하고, 대상
호스트마다 [지원하는 mux](#지원-mux)가 하나 이상 있어야 한다.

### 3. 앱 실행

```sh
xmux
```

목록에는 이 머신의 세션이 곧바로 채워지고, 원격 호스트는 응답하는 대로 합류한다.
`↑` / `↓`로 이동하고, `Enter`로 선택한 세션에 입력하며, `Ctrl-g` 다음 `Tab`으로
목록에 돌아온다. `Ctrl-g ?`는 모든 키를 보여 주고, `Ctrl-g q`는 종료한다.

## 지원 mux

| 플랫폼     | mux                                               |
| ---------- | ------------------------------------------------- |
| unix 계열  | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios` |
| Windows    | `psmux`                                           |

xmux는 호스트가 어느 바이너리로 응답하는지를 보고 그 호스트의 mux를 판별한다.
따라서 호스트마다 다른 mux가 설치되어 있어도 설정할 것이 없다.

## 사용법

```sh
xmux                          # 앱을 실행한다
xmux ls                       # 접근할 수 있는 모든 세션을 나열한다 (스크립트용)
xmux attach <source> <name>   # 세션 하나에 바로 attach 한다, 예: xmux attach prod api
xmux doctor                   # 설정과 소스별 접근 가능 여부를 점검한다
xmux instances                # 실행 중인 인스턴스를 나열한다
xmux send <name> <command…>   # 그중 하나를 컨트롤 소켓으로 조작한다
xmux update                   # 설치된 바이너리를 갱신한다
xmux version
```

왼쪽 창이 목록이고, 터미널 뷰는 선택한 세션의 실시간 화면을 보여 준다. 키보드 포커스는
두 영역 중 한쪽에만 있다.

## 키

목록의 키 바인딩이다.

| 키                         | 동작                                                                  |
| -------------------------- | --------------------------------------------------------------------- |
| `↑` / `↓` (또는 `k` / `j`) | 카드 한 개 이동 (양 끝에서 순환한다)                                  |
| `←` / `→` (또는 `h` / `l`) | 이전 / 다음 `host/mux` 구역으로 이동한다. 호스트 카드들은 하나로 친다 |
| `Home` / `End`             | 첫 카드 / 마지막 카드로 이동                                          |
| `PageUp` / `PageDown`      | 카드 열 개 이동                                                       |
| `Enter`                    | 선택한 세션의 터미널 뷰로 포커스를 옮긴다                             |
| `prefix 1`-`prefix 9`      | 왼쪽 열의 번호로 세션을 선택한다 (10 이상은 계속 입력한다)            |
| `prefix n`                 | 선택한 호스트에 새 세션을 만든다                                      |
| `/`                        | 목록을 퍼지 필터로 좁힌다                                             |
| `prefix r`                 | 다시 스캔한다. 머신 목록과 각 소스의 세션을 모두 갱신한다             |

xmux에는 tmux의 `set -g prefix`처럼 자체 prefix가 있다. 기본값은 `Ctrl-g`이며,
`[ui] prefix` 설정이 이 값을 대체한다. prefix를 누른 다음 조합키를 입력한다.

| 조합키       | 동작                                   |
| ------------ | -------------------------------------- |
| `prefix q`   | 종료                                   |
| `prefix ?`   | 키 도움말 토글                         |
| `prefix Tab` | 목록과 터미널 뷰 사이의 포커스 이동    |
| `prefix p`   | nav를 터미널 뷰의 다음 변으로 옮긴다   |

마우스 입력도 지원한다. 행을 클릭하면 그 행이 선택되고, 터미널 뷰를 클릭하면
포커스가 그쪽으로 옮겨진다. 나머지 키는 [`docs/keybind.md`](docs/keybind.md)에 있다.

## 호스트와 소스

**호스트**는 mux가 동작하고 있고 xmux가 접근할 수 있는 머신이다. **소스**는
호스트 하나 위의 mux 하나다. 그래서 psmux와 zellij가 함께 동작하는 호스트는 두
소스가 된다. 소스 이름은 호스트가 여러 mux를 제공하면 `local:psmux` 형식이고,
하나만 제공하면 `prod` 형식이며, 목록에 표시되는 이름이 곧 소스다. 명령은 세션을
소스와 세션 이름으로 따로 지정한다(예: `switch prod api`).

xmux는 원격 호스트를 앱이 실행된 뒤에 조회하므로, 소스는 각 호스트가 응답하는
대로 하나씩 나타난다.

### 호스트에 로그인하기

ssh가 스스로 알아내는 값만으로 닿지 못한 원격 호스트는 `login required`(`?` 표시)로
나타난다. 터미널 뷰의 해당 패널로 포커스를 옮긴다.

1. 패널에는 ssh가 묻지 않는 세 가지, 곧 주소와 포트와 사용자 이름을 입력하는
   칸이 있고, 각 칸은 ssh가 썼을 값으로 시작한다. 마스킹된 비밀번호 칸은 선택이다.
2. 제출하면 xmux가 그 값으로 ssh와 직접 대화한다. 호스트 키 확인과 비밀번호를
   xmux가 대신 답하므로 볼 화면도 입력할 것도 없다. Esc는 시도를 끝낸다.
3. 로그인에 성공하면 xmux가 그 호스트만 다시 조회하고, 패널은 찾아낸 세션 목록으로
   바뀐다. 제출한 값은 그 머신에 기록되므로, 이후 xmux가 그 머신에서 실행하는 모든
   명령이 같은 값으로 접속한다.

입력한 값으로 끝낼 수 없는 로그인은 서버가 무엇을 요구했는지 알린다. 성공한
로그인이 무엇을 남길지는 체크박스 둘이 정한다. 입력한 값을 `~/.ssh/config`
스탠자로 적을지, 그리고 공개키를 그 호스트에 등록해 비밀번호를 다시 묻지 않게
할지다.

## 로스터

xmux가 호스트로 내놓는 머신 후보는 로스터가 조립한다. 로스터는 세 공급자에서
ssh 대상 이름을 모은다.

| 공급자          | 제안하는 이름                                          |
| --------------- | ------------------------------------------------------ |
| ssh config      | `~/.ssh/config`의 별칭                                 |
| 한 홉 네트워크  | 이 머신이 이미 한 홉으로 닿고 ssh에 응답하는 머신      |
| WSL             | 이 머신의 WSL 배포판                                   |

로스터는 시작 시와 재스캔마다 다시 조립된다. `local`은 ssh 없이 도달하는 이 머신
자체라 로스터에 포함되지 않으며, 어떤 공급자도 이름을 부르지 않는 머신은 xmux가
다룰 것이 없는 머신이다. `[discovery]` 표는 공급자를 개별로 끄는 방법이며,
기본값은 전부 켜져 있다.

모든 공급자는 ssh 대상 이름을 산출하고, 어느 공급자가 이름을 제안했든 하류 동작은
같다. 제안한 공급자는 이름 옆에 보관되어 도달할 수 없는 호스트가 생기면 그 화면에
표시되므로, 어느 공급자를 살피거나 끌지 판단할 수 있다. 공급자가 도는 데 필요한
명령이 없거나 운영체제가 답하지 않거나 출력을 해석하지 못하면, 로스터는 그
공급자를 오류로 삼지 않고 빈 목록으로 처리한다. 따라서 한 공급자가 죽어도 다른
공급자가 제안한 호스트는 계속 제공된다.

### 한 홉 네트워크를 읽는 방법

한 홉 공급자는 운영체제의 네트워크 상태를 읽는다. VPN 클라이언트를 깔 필요도,
어딘가에 계정을 둘 필요도 없다. 운영체제에 직접 묻는다. 리눅스와 Android에서는
netlink로, 윈도우에서는 IP Helper로 읽으므로, 명령줄 도구가 없거나 Android처럼
거부하는 환경에서도 동작한다.

- **누가 닿는가.** 기록 둘이 말해 준다. 하나는 라우팅 테이블로, 메시 VPN이
  피어마다 경로를 하나씩 써 둔다. 몇 개를 한 경로로 묶어 두기도 하는데, 그런
  경로는 그 안의 주소들로 펼친다. 다른 하나는 이웃 테이블(ARP 캐시)로, 같은
  링크에서 실제로 프레임을 주고받은 머신이 들어 있다. Android처럼 운영체제가
  이웃 테이블을 거부하면, 이 머신이 속한 링크를 주소 단위로 묻는다.
- **그중 무엇이 머신인가.** 해석에 실패한 항목과, 하드웨어 주소 하나가 여러
  주소를 대표하는 항목(서브넷을 대신 답하는 라우터)은 머신을 가리키지 않는다.
  남은 것에는 ssh에 응답하는지 묻는다. 같은 스위치에 물린 프린터는 이웃이지만
  호스트는 아니기 때문이다.
- **무엇이라 부르는가.** 시스템 리졸버를 먼저 묻는다. 메시 VPN의 이름 짓기가
  이미 거기 있으므로 피어는 자기 네트워크가 준 이름으로 나타난다. 리졸버가 모르는
  머신에는 그 머신 자신에게 이름을 묻는다. 등록 여부와 무관하게 mDNS로 자기
  이름을 답한다. 얻은 이름은 이 머신이 다시 주소로 바꿀 수 있을 때만 쓴다. 그
  이름이 곧 ssh에 넘어가는 값이기 때문이다. 이름이 아무 데도 닿지 않는 머신은
  주소를 그대로 이름으로 쓴다.

## 설정

설정은 전부 선택 사항이다. xmux는 `~/.config/xmux/config.toml`을 읽는다.

```toml
exclude = ["bastion", "wsl.docker-desktop"]   # 이 머신들은 목록에서 숨긴다

[local]
mux = "auto"          # "auto"(기본값)는 이 머신에 설치된 mux 전부를 뜻한다.
                      # ["psmux", "zellij", "abduco", "tuios"]처럼 목록도 받는다.

[ui]
theme = "auto-dark"                  # 내장 ANSI 테마: "auto-dark"(기본값) 또는
                                      # "auto-light"(밝은 터미널용)
prefix = "C-g"                        # xmux의 prefix (예: C-g, C-Space, C-b)
auto-hide-nav = false                 # auto-hide-nav의 초기 상태
hide-unreachable = true               # 도달하지 못한 호스트는 nav에서 숨긴다 (필터에 이름을 입력하면 카드가 나타난다)
nav-position = "left"                 # nav의 기본 위치 (left|top|right|bottom)
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

- **실시간 반영.** `config.toml`이 바뀌면 `[ui]` 표시 설정(테마, 역할별 색
  오버라이드, selection-style, hint-bar-style, view-border 스타일)을 재시작 없이
  다시 적용한다. 호스트/로스터 변경은 `prefix r`로 다시 스캔해야 한다.
- **nav 위치.** nav는 터미널 뷰의 네 변 중 한 곳에 붙는다(왼쪽이나 오른쪽의 세로
  열, 위나 아래의 가로 띠). `[ui] nav-position`이 기본 위치를 정하며, nav는
  스스로 움직이지 않는다. `prefix p`는 nav를 시계 방향으로 한 변 옮기고(left →
  top → right → bottom → 기본값) 그 선택을 `~/.xmux/nav_position`에 기억한다. 이
  기억은 키가 기본값으로 돌아올 때까지 설정보다 우선한다.
- **호스트.** xmux는 호스트를 먼저 `~/.ssh/config`에서 읽는다. 설정 파일은 그
  발견을 보완하며 대체하지 않는다.
- **상태.** 다음 실행까지 남는 상태(마지막에 선택한 세션, auto-hide-nav 토글,
  고정한 nav 위치, 로그, 컨트롤 소켓)는 `~/.xmux/` 아래에 있다.

## 컨트롤 소켓

실행 중인 인스턴스마다 이름이 있고, 각 인스턴스는 `~/.xmux/ctl-<name>.sock`에서
요청을 받는다. 명령은 세션을 소스와 세션 이름으로 따로 지정하며(`switch
<source> <session>`), 목록에는 `<source>/<session>`으로 합쳐 보인다. 소켓이 받는
명령은 탐색 명령(`ping`, `status`, `dump`, `rescan`, `switch`, `focus`, `width`,
`toggle-auto-hide`, `quit`)과 세션 수명 명령 하나(`new-session`)다. kill,
rename, window 명령은 없다. 세션을 편집하는 일은 mux가 담당한다.

```sh
xmux instances                       # NAME · PID · CWD · TTY · displayed · focus
xmux send amber-otter switch prod api
xmux send am focus terminal          # 겹치지 않는 이름 앞부분으로 지정한다
xmux send - dump                     # 하나만 실행 중일 때는 `-`
```

없는 이름, 여러 인스턴스에 걸리는 앞부분, 여러 인스턴스가 실행 중일 때의 `-`는
모두 후보를 알려주는 오류로 끝난다. xmux는 짐작해서 하나를 고르지 않는다.

## 라이선스

MIT 라이선스다. 전문은 [`LICENSE`](LICENSE)에 있다.

## 더 읽을 것

- [`INSTALL.md`](INSTALL.md) - 모든 설치 방법, 갱신, 버전 고정
- [`docs/keybind.md`](docs/keybind.md) - 키 바인딩과 prefix 상세
- [`docs/requirements.md`](docs/requirements.md) - 동작 요구 사항
- [`docs/adr/`](docs/adr/) - 아키텍처 결정 기록
- [`CONTEXT.md`](CONTEXT.md) - 용어와 설계 개요
- [`AGENTS.md`](AGENTS.md) - 디렉터리별 작업 노트
