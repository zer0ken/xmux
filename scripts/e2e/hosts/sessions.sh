#!/bin/sh
# Starts two detached sessions, <mux>1 and <mux>2, in every mux named on the command
# line, skipping one that already runs. It also runs on a host that was stopped and
# started again: screen leaves dead sockets behind that `screen -wipe` does not always
# remove, so only a socket screen does not call dead counts as a running session; and
# tuios restores its saved sessions when its daemon starts, which `tuios new` then
# reports as existing. Runs as the remote user through a login shell, so ~/.local/bin
# is on PATH.
set -eu
shell=$(awk -F: -v u="$(id -un)" '$1 == u { print $7 }' /etc/passwd)
screen -wipe >/dev/null 2>&1 || true
for mux in "$@"; do
  for n in 1 2; do
    s="$mux$n"
    case "$mux" in
      tmux) tmux has-session -t "=$s" 2>/dev/null || tmux new-session -d -s "$s" -x 100 -y 30 ;;
      screen) screen -ls | grep "[0-9]\.$s[[:space:]]" | grep -qiv dead || screen -dmS "$s" ;;
      zellij) zellij ls -n 2>/dev/null | grep "^$s " | grep -qv EXITED || zellij-new "$s" ;;
      abduco) abduco | awk -v s="$s" '$NF == s' | grep -q . || abduco -n "$s" "$shell" ;;
      tuios) tuios new "$s" --detach >/dev/null 2>&1 || tuios ls --json | grep -q "\"name\": \"$s\"" ;;
      herdr) herdr session list --json | grep -q "\"name\":\"$s\",\"running\":true" \
               || { nohup herdr --session "$s" server >/dev/null 2>&1 & } ;;
    esac
  done
done
sleep 1
