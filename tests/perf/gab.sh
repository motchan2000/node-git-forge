#!/bin/sh
# usage: gab.sh "bin1 ENV=..." "bin2 ENV=..." ... -> servers on ports 3001.. inside one gVisor sandbox
D=$(cd "$(dirname "$0")" && pwd); INNER=$(mktemp); P=3000; PORTS=""
for spec in "$@"; do P=$((P+1)); set -- $spec; B=$1; shift; echo "env MAKEFUN_SHOP_TOKEN=tk $* $B $P >/dev/null 2>&1 &" >> $INNER; PORTS="$PORTS${PORTS:+,}$P"; done
echo "sleep 1; node $D/ab.mjs $PORTS ${BLOCKS:-12}" >> $INNER
taskset -c 0-1 runsc --network=none --ignore-cgroups do sh "$INNER" 2>&1 | grep -v '^\['
rm -f $INNER
