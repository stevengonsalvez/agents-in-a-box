#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for be in vt100 alacritty wezterm wezterm14; do
  echo "----- $be -----"
  timeout 150 bash "$SD/flood2.sh" "$be"
done
