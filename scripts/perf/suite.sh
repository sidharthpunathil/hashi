#!/bin/sh
# Runs every scenario of bench.py against one release binary.
# usage: scripts/perf/suite.sh path/to/hashi
set -e
B=$1
HERE=$(cd "$(dirname "$0")" && pwd)
P="python3 -I $HERE/bench.py"
echo "size_bytes $(wc -c < "$B" | tr -d ' ') gzip_bytes $(gzip -9c "$B" | wc -c | tr -d ' ')"
echo "startup $(BENCH_PREEXEC=1 $P "$B" startup 15)"
echo "startup_100ms_probes $(BENCH_PREEXEC=1 BENCH_VDELAY=0.1 $P "$B" startup 5)"
echo "load $($P "$B" load 1000)"
echo "held $($P "$B" held)"
echo "held_30k_prompts $(BENCH_PROMPT_BYTES=30000 $P "$B" held)"
echo "big $($P "$B" big)"
echo "leak $($P "$B" leak 10000)"
