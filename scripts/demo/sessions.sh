#!/bin/sh
# Creates the demo tmux sessions for one machine, named by its hostname.
set -e
mk() { tmux new-session -d -s "$1" -x 92 -y 26; }
show() { tmux send-keys -t "$1" "clear; $2" Enter; }
case "$(hostname)" in
  laptop)
    mk dotfiles; mk notes
    show notes 'printf "todo\n  - renew the TLS cert\n  - review PR 142\n"'
    ;;
  gpu-01)
    mk my-important-session; mk train-llm
    show my-important-session 'printf "\033[1;32mtraining run 42\033[0m  model=resnet-152  gpus=8\n\n"; for e in 14 15 16 17; do printf "epoch %2d/50  loss %.3f  val_acc %.1f%%\n" $e $(echo "0.9-$e*0.04" | bc) $(echo "60+$e*1.5" | bc); done'
    show train-llm 'printf "\033[1;36mllm pretrain\033[0m  7B  seq=4096\n\n"; for s in 1100 1150 1200; do printf "step %d/5000  loss %.2f  tok/s 41.2k\n" $s $(echo "3.4-$s*0.0009" | bc); done'
    ;;
  web-01)
    mk api-server; mk deploy
    show api-server 'echo "listening on :8080"'
    show deploy 'echo "deployed v2.4.1 to 3 replicas"'
    ;;
esac
sleep 1
