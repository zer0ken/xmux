#!/bin/sh
# The host's entry point: starts the remote user's sessions, then sshd.
#
# The sessions start before sshd accepts a connection, on the first boot and after every
# restart. xmux polls a host with every mux's listing as soon as ssh answers, and a zellij
# command that reaches a zellij server still setting up its session panics that server
# (zellij-org/zellij#5632), which leaves the `zellij attach -b` that started it waiting
# for good. sshd's pid file therefore also means the sessions are up; a stopped host keeps
# the old one, so it goes first.
set -eu
rm -f /run/sshd.pid /var/run/sshd.pid
su dev -c 'cd && exec sh -lc "sh /opt/e2e/sessions.sh tmux screen zellij abduco tuios herdr"'
exec /usr/sbin/sshd -D -e
