#!/bin/sh
# Run the kit's conformance suite and four timed workloads with the forge and the suite together inside
# ONE gVisor sandbox (runsc do), the way the trial's pod runs them. Needs root (or a privileged
# container), Node 22, the unpacked kit, and gVisor's runsc (+ gvisor-bin/) on PATH.
#
#   sh tests/perf/gvisor-perf.sh <kit dir> [rust|node] [extra env for the forge, e.g. FORGE_SERVER=epoll]
#
# Getting runsc (official release, checked against its published SHA-512):
#   U=https://storage.googleapis.com/gvisor/releases/release/latest/x86_64
#   curl -fsSLO $U/gvisor.tar.bz2 && curl -fsSLO $U/gvisor.tar.bz2.sha512 && sha512sum -c gvisor.tar.bz2.sha512
#   tar -xjf gvisor.tar.bz2 -C /usr/local/bin        # runsc, gvisor-bin/ must stay next to it
#
# From Docker Desktop on Windows/macOS this ran inside `docker run --privileged --cpus 4 node:22 ...`.
# Calibration (2026-11-20): production times were about 1.6x this sandbox's, except that busy-polling,
# which was faster here, was much slower in production (CPU-crowded nodes). Do not trust CPU-burning
# tricks measured here.
set -e
KIT=${1:?kit directory}
KIND=${2:-rust}
shift 2 2>/dev/null || shift $#
REPO=$(cd "$(dirname "$0")/../.." && pwd)
INNER=$(mktemp)
cat > "$INNER" <<EOF
cd "$REPO"
if [ "$KIND" = node ]; then
  env MAKEFUN_SHOP_TOKEN=tk $* node tests/serve.ts 3000 127.0.0.1 >/dev/null 2>&1 &
else
  env MAKEFUN_SHOP_TOKEN=tk $* "$REPO/bin/forge-linux-amd64" 3000 >/dev/null 2>&1 &
fi
sleep 2
cd "$KIT" && node sdk/run.ts --url http://127.0.0.1:3000 --token tk --perf 2>&1 | grep -E '^checks|^benchmark|perf_p50'
EOF
runsc --network=none --ignore-cgroups do sh "$INNER" 2>&1 | grep -v '^\['
rm -f "$INNER"
