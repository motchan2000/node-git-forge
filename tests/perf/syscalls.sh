#!/bin/sh
# Count the forge's system calls per benchmark workload (strace -f -c), natively (no gVisor needed).
#   KIT=<kit dir> sh tests/perf/syscalls.sh [binary] [workloads]
# Each workload runs 1 rep (5 rounds + warm-ups) against a fresh forge. In steady state every request
# should cost one recvfrom and one sendto; futex/mmap/munmap/mprotect in the thousands mean a regression
# (see TESTING.md). Needs strace.
BIN=${1:-bin/forge-linux-amd64}; WLS=${2:-push,clone,read,write}
D=$(cd "$(dirname "$0")" && pwd); OUT=$(mktemp -d)
for wl in $(echo "$WLS" | tr ',' ' '); do
  MAKEFUN_SHOP_TOKEN=tk strace -f -c -o "$OUT/$wl.txt" "$BIN" 3091 >/dev/null 2>&1 &
  sleep 0.7
  KIT=$KIT node --experimental-strip-types "$D/b.ts" http://127.0.0.1:3091 "$wl" 1 >/dev/null 2>&1
  pkill -f "$BIN 3091"; sleep 0.5
  printf '%-6s ' "$wl"; sort -k4 -n -r "$OUT/$wl.txt" | awk 'NF>=5 && $NF!="syscall" {print $4, $NF}' | head -8 | tr '\n' ' '; echo
done
rm -rf "$OUT"
