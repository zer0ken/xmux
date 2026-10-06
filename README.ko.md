# xmux

[English](README.md) · 한국어

*여러 머신과 여러 mux의 세션을 한 화면에서 전환하는 도구.*

![이 GIF는 같은 키 입력 속도로 나란히 녹화한 두 터미널을 보여준다. 왼쪽 터미널에서는
ssh로 gpu-01에 접속해 tmux 세션 목록을 확인하고 attach하기까지 7.1초가 걸린다.
오른쪽 터미널에서는 xmux가 landing 화면에서 화살표 키 두 번과 Enter로 같은 세션을
열기까지 2.3초가 걸린다.](docs/assets/xmux-demo.gif)

**landing 화면에서 세션 열기**

![xmux는 landing 화면으로 시작한다. landing 화면은 host가 응답하는 대로 찾은 세션을
번호와 함께 나열한다. 화살표 키 두 번과 Enter로 gpu-01의 세션을 연다.](docs/assets/xmux-landing.gif)

**세션 전환**

![xmux는 card 한 개를 내려간 뒤 번호로 5번과 3번 세션으로 이동한다. terminal view는
선택한 세션을 따라 바뀐다.](docs/assets/xmux-nav-switch.gif)

**source와 host로 올라가기**

![Ctrl-↑는 세션의 source를 선택해 그 화면을 표시하고, Ctrl-↑를 한 번 더 누르면 host를
선택해 그 화면을 표시한다. Ctrl-↓는 세션까지 다시 내려간다.](docs/assets/xmux-hierarchy.gif)

**비밀번호 host 로그인**

![비밀번호만 받는 host는 login needed로 표시된다. 그 host의 로그인 패널이 비밀번호를
받아 이 머신의 공개키를 host에 등록하고, host의 세션이 nav에 추가된다.](docs/assets/xmux-login.gif)

**nav 폭 조절**

![xmux는 prefix 뒤 Ctrl-→를 누를 때마다 nav의 폭을 한 열씩 넓히고, Ctrl-←를 누를
때마다 한 열씩 좁힌다.](docs/assets/xmux-nav-resize.gif)

**nav 배치**

![xmux는 prefix p를 누를 때마다 nav를 terminal view의 다음 변에 배치한다. nav는 위,
오른쪽, 아래를 거쳐 왼쪽으로 돌아온다.](docs/assets/xmux-nav-place.gif)

**nav 자동 숨기기**

![자동 숨기기가 켜진 상태에서 terminal view로 포커스를 옮기면 xmux는 nav를 숨기고
terminal view에 전체 폭을 할당한다. prefix Tab으로 nav에 포커스를 옮기면 nav가 다시
나타난다.](docs/assets/xmux-nav-autohide.gif)

## xmux 소개

xmux는 자신을 실행한 터미널을 소유하고 화면을 둘로 나눈다. **nav**에는 이 머신, 이
머신의 WSL 배포판, 접근할 수 있는 모든 ssh host의 세션마다 card가 하나씩 있고,
**terminal view**는 선택한 세션을 실제 mux 클라이언트로 표시한다. host 목록은
`~/.ssh/config`와 이 머신이 이미 접근하는 머신에서 만들어지고, xmux는 각 host가
실행하는 mux를 검사해 그 host의 mux를 판별한다.

xmux는 ssh와 각 host에 이미 있는 mux로 동작하므로, xmux를 사용하는 머신 한 대에만
설치한다. xmux는 tmux의 대안이 아니며, tmux 세션에 접속하는 절차만 간편하게 만든다.

## 설치

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

Homebrew, WinGet, Cargo 설치와 버전 고정, 갱신, 제거는 [`INSTALL.md`](INSTALL.md)가
다룬다.

## 첫 실행

```sh
xmux
```

landing 화면은 host가 응답하는 대로 모든 세션을 나열한다. nav에서 쓰는 키는 다음과
같다.

- `↑` / `↓`는 선택을 옮긴다.
- `Enter`는 키보드 입력을 선택한 세션으로 보낸다.
- `Ctrl-g` 다음 `Tab`은 포커스를 nav로 되돌린다.
- `Ctrl-g ?`는 모든 키를 표시하고, `Ctrl-g q`는 xmux를 종료한다.

## 지원 mux

| 플랫폼     | mux                                                        |
| ---------- | ---------------------------------------------------------- |
| unix 계열  | `tmux`, GNU `screen`, `zellij`, `abduco`, `tuios`, `herdr` |
| Windows    | `psmux`, `herdr`                                            |

원격 host를 사용하려면 xmux를 실행하는 머신에 `ssh`가 있어야 하고, 각 host에 이 중
하나의 mux가 있어야 한다.

## 더 읽을 것

- [`INSTALL.md`](INSTALL.md) - 모든 설치 방법, 설치 확인, 갱신, 버전 고정
- [`docs/guide.md`](docs/guide.md) - 명령줄, host와 로그인, 설정, 컨트롤 소켓
- [`docs/keybind.md`](docs/keybind.md) - 모든 키, prefix, popup, 마우스
- [`docs/principles.md`](docs/principles.md) - 설계 원칙
- [`docs/requirements.md`](docs/requirements.md) - 동작 요구 사항
- [`CONTEXT.md`](CONTEXT.md) - 용어와 설계 개요
- [`AGENTS.md`](AGENTS.md) - 디렉터리별 작업 노트

## 라이선스

MIT 라이선스다. 전문은 [`LICENSE`](LICENSE)에 있다.
