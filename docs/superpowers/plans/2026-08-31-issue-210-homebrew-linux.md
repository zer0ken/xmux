# Issue 210: Linux support for Homebrew tap installation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reflect Linux (x86_64, arm64) tarballs in the Homebrew formula generation template and the release pipeline so that `brew install zer0ken/xmux/xmux` works on Linuxbrew from the next release onward.

**Architecture:** The formula (`packaging/homebrew/xmux.rb`) is an artifact the release workflow generates. Nest the darwin blocks inside `on_macos`, add an `on_linux` block, and add `ubuntu-24.04-arm` to the build matrix to produce the aarch64-unknown-linux-gnu tarball.

**Tech Stack:** GitHub Actions YAML, Homebrew formula DSL, local verification tools: ruby 3.0 (`/usr/bin/ruby`), python3 + PyYAML, Linuxbrew x86_64 (`/home/linuxbrew/.linuxbrew/bin/brew`). No actionlint.

---

## Findings

- The `release.yml` build matrix (lines 18-32) has two darwin, one linux x86_64, and four windows targets. The `update-packaging` job (from line 103) collects checksums into `GITHUB_ENV`, regenerates `packaging/homebrew/xmux.rb` wholesale through a heredoc, commits the result to main, and syncs it to the tap.
- The checked-in `packaging/homebrew/xmux.rb` is a bot-regenerated release artifact (currently v0.7.3, darwin-only). It is regenerated from the template at the next release.
- This machine is Ubuntu 22.04 (glibc 2.35) WSL, and the v0.7.3 linux x86_64 tarball really exists, so the generated formula can be exercised with `brew fetch` and a run test (the glibc data point). The arm64 linux tarball does not exist yet.
- `src/cli/update` already recognizes the `/home/linuxbrew/` marker, so no code change is needed. The "macOS" wording in the top-level `README.md`/`README.ko.md`/`INSTALL.md` correctly describes the current state until the release that refreshes the tap, so it is out of scope for this PR.

## Plan

### Task 1: Add arm64 Linux to the `.github/workflows/release.yml` build matrix

- [ ] Replace the `include:` block at line 18 with the block below (place the new entry after the ubuntu-latest entry, keeping the OS group order):

```yaml
        include:
          - os: windows-latest
            target: x86_64-pc-windows-msvc
            ext: .exe
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            ext: .tar.gz
          - os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
            ext: .tar.gz
          - os: macos-14
            target: aarch64-apple-darwin
            ext: .tar.gz
          - os: macos-15-intel
            target: x86_64-apple-darwin
            ext: .tar.gz
```

`ubuntu-24.04-arm` is the free arm64 runner for public repositories named in the issue. The build steps are reused unchanged (`cargo build --release --target`, bash staging, and artifact upload are all arch-neutral). The dependencies are pure Rust, so no arm64 system packages need to be added.

Immediate verification: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('ok')"`.

### Task 2: Add the two Linux checksums to the "Resolve version and checksums" step

- [ ] Replace the step near line 122 with the block below (keep the existing variables, add 2, arm first). Each checksum is assigned to a shell variable before it is echoed: under `set -euo pipefail`, a bare assignment propagates the command substitution's exit status to `set -e`, while a substitution inside `echo`'s arguments does not (set -e only sees echo's own status), so the assignment form is what makes a missing artifact fail the job loudly, matching the existing style:

```yaml
      - name: Resolve version and checksums
        run: |
          set -euo pipefail
          VERSION=${GITHUB_REF_NAME#v}
          WIN_SHA=$(sha256sum artifacts/*windows-msvc.exe | cut -d' ' -f1)
          ARM_SHA=$(sha256sum artifacts/*aarch64-apple-darwin.tar.gz | cut -d' ' -f1)
          INTEL_SHA=$(sha256sum artifacts/*x86_64-apple-darwin.tar.gz | cut -d' ' -f1)
          LINUX_ARM_SHA=$(sha256sum artifacts/*aarch64-unknown-linux-gnu.tar.gz | cut -d' ' -f1)
          LINUX_INTEL_SHA=$(sha256sum artifacts/*x86_64-unknown-linux-gnu.tar.gz | cut -d' ' -f1)
          echo "VERSION=$VERSION" >> "$GITHUB_ENV"
          echo "WIN_SHA=$WIN_SHA" >> "$GITHUB_ENV"
          echo "ARM_SHA=$ARM_SHA" >> "$GITHUB_ENV"
          echo "INTEL_SHA=$INTEL_SHA" >> "$GITHUB_ENV"
          echo "LINUX_ARM_SHA=$LINUX_ARM_SHA" >> "$GITHUB_ENV"
          echo "LINUX_INTEL_SHA=$LINUX_INTEL_SHA" >> "$GITHUB_ENV"
```

The globs cannot cross-match darwin with Linux because the target triplet is part of the file name.

### Task 3: Restructure the "Update Homebrew formula" heredoc template

- [ ] Replace the entire step at line 147 with the block below. The existing standalone `on_arm`/`on_intel` blocks look at CPU only, regardless of OS, so on Linux they pick the darwin tarballs. Nest the darwin blocks inside `on_macos` and add an `on_linux` block (nested hardware blocks are the official Homebrew DSL pattern). As in the existing template, the heredoc is unquoted, so `${VERSION}` and `$ARM_SHA` are subject to shell expansion while `#{bin}` is left untouched by the shell:

```yaml
      - name: Update Homebrew formula
        run: |
          set -euo pipefail
          cat > packaging/homebrew/xmux.rb <<EOF
          class Xmux < Formula
            desc "Cross-environment tmux/psmux session switcher"
            homepage "https://github.com/zer0ken/xmux"
            license "MIT"
            version "$VERSION"

            on_macos do
              on_arm do
                url "https://github.com/zer0ken/xmux/releases/download/v${VERSION}/xmux-v${VERSION}-aarch64-apple-darwin.tar.gz"
                sha256 "$ARM_SHA"
              end

              on_intel do
                url "https://github.com/zer0ken/xmux/releases/download/v${VERSION}/xmux-v${VERSION}-x86_64-apple-darwin.tar.gz"
                sha256 "$INTEL_SHA"
              end
            end

            on_linux do
              on_arm do
                url "https://github.com/zer0ken/xmux/releases/download/v${VERSION}/xmux-v${VERSION}-aarch64-unknown-linux-gnu.tar.gz"
                sha256 "$LINUX_ARM_SHA"
              end

              on_intel do
                url "https://github.com/zer0ken/xmux/releases/download/v${VERSION}/xmux-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz"
                sha256 "$LINUX_INTEL_SHA"
              end
            end

            def install
              bin.install "xmux"
            end

            test do
              system "#{bin}/xmux", "version"
            end
          end
          EOF
```

Note: do not touch `packaging/homebrew/xmux.rb` (the checked-in formula) in this PR. The release workflow regenerates that file from the template above at the next release, and version 0.7.3 has no aarch64-unknown-linux-gnu artifact, so hand-inserting the four blocks would leave the formula pointing at tarballs that do not exist.

### Task 4: Fix the macOS-only wording in `packaging/homebrew/README.md`

- [ ] Replace the entry in the final Files section (this rewrite also drops the em-dash that line contained):

```markdown
- `Formula/xmux.rb`: the formula; installs the prebuilt binary for macOS
  (Apple Silicon and Intel) and Linux (x86_64 and arm64).
```

The rest of the file (the tap registration procedure) is OS-neutral and stays untouched. The file stays in English, as it already is.

### Task 5: Template dry run (update-packaging reproduced locally)

- [ ] Run the checksum collection and formula generation scripts verbatim against the real v0.7.3 artifacts to verify the globs, the expansion, and the Ruby syntax:

```bash
mkdir -p /tmp/xmux-210/artifacts /tmp/xmux-210/gen/packaging/homebrew
cd /tmp/xmux-210/artifacts
for a in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  curl -sSLO "https://github.com/zer0ken/xmux/releases/download/v0.7.3/xmux-v0.7.3-${a}.tar.gz"
done
touch xmux-v0.7.3-aarch64-unknown-linux-gnu.tar.gz   # placeholder: the first arm64 tarball exists only from the next release

cd /home/hrlee/xmux-wt/210-homebrew-linux
python3 - <<'PY'
import yaml
wf = yaml.safe_load(open('.github/workflows/release.yml'))
steps = {s.get('name'): s for s in wf['jobs']['update-packaging']['steps']}
open('/tmp/xmux-210/resolve.sh', 'w').write(steps['Resolve version and checksums']['run'])
open('/tmp/xmux-210/formula.sh', 'w').write(steps['Update Homebrew formula']['run'])
PY

cd /tmp/xmux-210
GITHUB_REF_NAME=v0.7.3 GITHUB_ENV=/tmp/xmux-210/env bash resolve.sh
set -a; source /tmp/xmux-210/env; set +a
cd /tmp/xmux-210/gen && bash /tmp/xmux-210/formula.sh
```

- [ ] Then verify (ruby 3.0 and Linuxbrew are present locally):

```bash
ruby -c /tmp/xmux-210/gen/packaging/homebrew/xmux.rb
brew style /tmp/xmux-210/gen/packaging/homebrew/xmux.rb
brew fetch --formula /tmp/xmux-210/gen/packaging/homebrew/xmux.rb
```

- `ruby -c`: Ruby syntax check.
- `brew style`: Homebrew style check (can be skipped if it rejects a path argument; `ruby -c` and the fetch below already cover syntax and actual behavior).
- `brew fetch`: on this x86_64 Linux host the `on_linux` + `on_intel` path is selected, so the real linux x86_64 tarball is downloaded and its checksum verified. This demonstrates that the URL and the checksum agree.
- Additional cross-check: compare the two darwin sha256 values in the generated file against the values in the checked-in `packaging/homebrew/xmux.rb`:

```bash
diff <(grep -A1 'apple-darwin.tar.gz' /tmp/xmux-210/gen/packaging/homebrew/xmux.rb | grep 'sha256 "' | sort) \
     <(grep -A1 'apple-darwin.tar.gz' /home/hrlee/xmux-wt/210-homebrew-linux/packaging/homebrew/xmux.rb | grep 'sha256 "' | sort) \
  && echo MATCH
```

The generated output stays under `/tmp` only; nothing goes into the repository.

### Task 6: glibc data point and (optional) real install test

- [ ] 

```bash
mkdir -p /tmp/xmux-210/run
tar -xzf /tmp/xmux-210/artifacts/xmux-v0.7.3-x86_64-unknown-linux-gnu.tar.gz -C /tmp/xmux-210/run
/tmp/xmux-210/run/xmux version; echo "exit=$?"
```

This machine is Ubuntu 22.04 (glibc 2.35), and v0.7.3 was most likely built on ubuntu-latest (24.04, glibc 2.39), so a `GLIBC_2.3x not found` failure is expected. Either way, record the outcome verbatim and use it as the PR body data point.

- [ ] Optional (a reversible real install test; skip if brew already has xmux):

```bash
export PATH="/home/linuxbrew/.linuxbrew/bin:$PATH"
brew list --versions xmux    # skip this step if there is output
brew install /tmp/xmux-210/gen/packaging/homebrew/xmux.rb
brew test /tmp/xmux-210/gen/packaging/homebrew/xmux.rb
brew uninstall xmux
```

### Task 7: Run the CI gates locally

- [ ] No Rust source is touched, but `ci.yml` runs unchanged on the PR branch, so check the three gates locally (cargo is not on the default PATH, so the toolchain path is prepended; the first build takes several minutes):

```bash
export PATH="/home/hrlee/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cd /home/hrlee/xmux-wt/210-homebrew-linux
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If a local toolchain problem blocks this, state that explicitly in the PR body (CI verifies the gates on the PR branch instead).

### Task 8: Commit and PR

- [ ] Stage exactly the two files:

```bash
cd /home/hrlee/xmux-wt/210-homebrew-linux
git add .github/workflows/release.yml packaging/homebrew/README.md
git commit -m "packaging: add Linux support to the Homebrew formula" -m "The formula's on_arm/on_intel blocks select by CPU architecture on any OS, so Linuxbrew picked the darwin tarballs and installed a binary that cannot run there. Nest the darwin blocks under on_macos, add an on_linux block with the x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu tarballs, build aarch64-unknown-linux-gnu on ubuntu-24.04-arm, and collect the two Linux checksums in update-packaging. The tap serves the new formula from the next release."
```

- [ ] Create the PR (`gh` lives at `/usr/bin/gh`). Title: `packaging: Linux support for the Homebrew tap`. Body (English, per the English public documentation rule; replace `[FILL-IN]` with the Task 6 measured result):

```markdown
Closes #210

## What

- The release build matrix gains `aarch64-unknown-linux-gnu` on
  `ubuntu-24.04-arm` (the free arm64 runner for public repositories).
- `update-packaging` collects the two Linux tarball checksums
  (`LINUX_ARM_SHA`, `LINUX_INTEL_SHA`).
- The generated formula moves the darwin blocks under `on_macos` and adds an
  `on_linux` block with nested `on_arm`/`on_intel` blocks, so Linuxbrew no
  longer selects darwin tarballs (the old standalone blocks matched CPU
  architecture on any OS).
- `packaging/homebrew/README.md` states the formula covers macOS and Linux.

`packaging/homebrew/xmux.rb` is regenerated by the release workflow and is
intentionally unchanged in this diff. The tap serves the new formula from the
next release.

## Verified locally

- YAML parse of `release.yml` (python3 + PyYAML).
- The two `update-packaging` `run` scripts extracted from the workflow and
  executed against the real v0.7.3 artifacts: the checksum globs match the
  artifact names, the heredoc renders valid Ruby (`ruby -c`), and `brew style`
  passes on Linuxbrew.
- `brew fetch` of the generated formula on Linuxbrew x86_64 downloads
  `xmux-v0.7.3-x86_64-unknown-linux-gnu.tar.gz` and its sha256 matches the
  release artifact.
- [FILL-IN: glibc data point, e.g. "the v0.7.3 x86_64-unknown-linux-gnu
  binary runs / fails on Ubuntu 22.04 (glibc 2.35) with: <output>."]
- cargo fmt --check, cargo clippy --all-targets -- -D warnings, and cargo
  test pass.

## Not verifiable until the next release

- The actual `aarch64-unknown-linux-gnu` release build on `ubuntu-24.04-arm`.
- `brew install zer0ken/xmux/xmux` on Linuxbrew (x86_64 and arm64); the arm64
  checksum value itself only exists once the first arm64 tarball is published.

## glibc note

The Linux tarballs are dynamically linked against the build runner's glibc:
`ubuntu-latest` (24.04, glibc 2.39) today, `ubuntu-24.04-arm` (glibc 2.39) for
the new target. Distributions with an older glibc fail with a
`GLIBC_2.x not found` error. If next-release verification shows install
failures on older distributions, the options are pinning older runner images
(a lower glibc floor) or adding musl targets (static binaries).
```

## Files to Modify

- `.github/workflows/release.yml`: add a `ubuntu-24.04-arm` + `aarch64-unknown-linux-gnu` entry to the build matrix, add `LINUX_ARM_SHA`/`LINUX_INTEL_SHA` to checksum collection, and replace the formula heredoc with the `on_macos`-nested + `on_linux` block structure.
- `packaging/homebrew/README.md`: reword the Files section from macOS-only to macOS and Linux (x86_64, arm64), removing the em-dash on that line.

## New Files (if any)

None. `packaging/homebrew/xmux.rb` is deliberately left unmodified (the release workflow regenerates it at the next release; editing it now would reference an arm64 artifact that does not exist in the current version).

## Risks

- **glibc floor**: ubuntu-latest (24.04) and ubuntu-24.04-arm are both glibc 2.39 based, so the binaries may fail to run under Linuxbrew on older distributions. The issue explicitly defers mitigation (pinning runner versions or adding musl targets) to a verification follow-up, so this PR leaves it out and records the options plus the locally measured result in the PR body. If mitigation becomes necessary, pinning x86_64 to `ubuntu-22.04` and arm64 to `ubuntu-22.04-arm` (as long as both remain free public runners) is a smaller change than adding musl.
- **tap refresh timing**: this PR alone does not change the tap formula; the next release's update-packaging does. The issue assumes the same. The "macOS" wording in the top-level `README.md` (line 27), `README.ko.md` (line 26), and `INSTALL.md` (line 19) needs updating at the next release (changing it now would describe behavior that does not exist yet). Left as follow-up work.
- **local dry run limits**: the linux arm checksum is a placeholder (the sha256 of an empty file) and is never committed. The real combination of four checksums only comes together at the next release.
- **brew tool argument handling**: whether `brew style`/`brew fetch` accept a single file path argument depends on the Homebrew version. If rejected, skip style and route fetch through `brew install` instead (including cleanup).
- **first arm64 runner run**: `dtolnay/rust-toolchain` and `Swatinem/rust-cache` support arm64, but the first build on ubuntu-24.04-arm is only exercised when the next tag is pushed. The dependencies are pure Rust, so no extra setup is needed (confirmed in Cargo.toml).
- **CI gates**: the change is packaging-only, so there is no reason for the Rust gates to break, but `ci.yml` runs on the PR branch, so run the three gates locally as well (Task 7).
- **character constraints**: no em-dash/en-dash in any newly written line. The existing em-dash on the README line being replaced disappears with the rewrite.
