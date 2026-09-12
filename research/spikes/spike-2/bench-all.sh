#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Section 3f: RSS at 1 and 100 emulators, then pure parse throughput on 50 MB.
set -u
B=$SD/target/release/spike2
mkdir -p "$SD/out"
[ -f "$SD/out/workload.bin" ] || python3 "$SD/make-workload.py"
[ -f "$SD/out/flood50.bin" ] || python3 "$SD/make-workload.py" --flood
echo "== RSS, live window 1000 rows, 120x40 =="
for be in vt100 alacritty wezterm wezterm14; do
  for n in 1 100; do
    timeout 300 "$B" bench-rss "$be" "$n" "$SD/out/workload.bin" 120 40 1000
  done
done
echo "== pure parse throughput, 50 MB =="
for be in vt100 alacritty wezterm wezterm14; do
  timeout 900 "$B" bench-cpu "$be" "$SD/out/flood50.bin" 120 40 1000
done
