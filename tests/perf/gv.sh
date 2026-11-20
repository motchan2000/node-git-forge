#!/bin/sh
# usage: gv.sh <binary> <workloads> <reps> [env...]
# KIT=<kit dir> must be set. Runs the forge and selected workloads in one gVisor sandbox pinned to 2 CPUs.
BIN=$1; WL=$2; REPS=$3; shift 3; D=$(cd "$(dirname "$0")" && pwd)
INNER=$(mktemp)
cat > "$INNER" <<EOF2
env MAKEFUN_SHOP_TOKEN=tk $* $BIN 3000 >/dev/null 2>&1 &
sleep 1
KIT=$KIT node --experimental-strip-types $D/b.ts http://127.0.0.1:3000 $WL $REPS 2>&1 | grep -v -i warn
EOF2
taskset -c ${CPUS:-0-1} runsc --network=none --ignore-cgroups do sh "$INNER" 2>&1 | grep -v '^\['
rm -f "$INNER"
