#!/bin/sh
# Installs the muxes that ship as release binaries. zellij goes on the system PATH;
# tuios and herdr go into the remote user's ~/.local/bin, which only the login
# profile puts on PATH, the way a user installs them without root.
set -eu
ZELLIJ=0.45.1
TUIOS=0.8.5
HERDR=0.9.3
cd /tmp
curl -fsSL "https://github.com/zellij-org/zellij/releases/download/v$ZELLIJ/zellij-no-web-x86_64-unknown-linux-musl.tar.gz" | tar xz
install -m 755 zellij /usr/local/bin/zellij
curl -fsSL "https://github.com/Gaurav-Gosain/tuios/releases/download/v$TUIOS/tuios_${TUIOS}_Linux_x86_64.tar.gz" | tar xz tuios
mkdir -p /home/dev/.local/bin
install -m 755 tuios /home/dev/.local/bin/tuios
curl -fsSL -o herdr "https://github.com/herdrdev/herdr/releases/download/v$HERDR/herdr-linux-x86_64"
install -m 755 herdr /home/dev/.local/bin/herdr
rm -f /tmp/zellij /tmp/tuios /tmp/herdr
# A user who runs zellij or herdr has a config and has seen the first-run screen it
# shows without one; abduco runs the login shell instead of dvtm, which no host installs.
mkdir -p /home/dev/.config/zellij /home/dev/.config/herdr
zellij setup --dump-config > /home/dev/.config/zellij/config.kdl
printf 'show_startup_tips false\nshow_release_notes false\n' >> /home/dev/.config/zellij/config.kdl
echo 'onboarding = false' > /home/dev/.config/herdr/config.toml
echo 'export ABDUCO_CMD="$(awk -F: -v u="$(id -un)" '"'"'$1 == u { print $7 }'"'"' /etc/passwd)"' > /etc/profile.d/abduco.sh
