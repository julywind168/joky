#!/bin/bash
# Run a flaky-runtime-test hunt under a bounded synthetic CPU load.
#
# Usage: scripts/load-campaign.sh [batches] [time_cap_seconds] [max_runs] [test_filter]
#
# Defaults: 8 batches, 155s load window per batch, 40 runs per batch, and the
# file-workflows cancellation test that observed the one confirmed leak.
#
# Resource contract (do not relax without revisiting the CPU-saturation
# incident that motivated this script):
#   - at most 3 loader processes, each self-expiring after 170s;
#   - batches never overlap: the script waits for every loader to exit and
#     re-checks the process table before starting the next batch;
#   - any loader that refuses to die aborts the whole campaign.
#
# macOS note: `timeout` is not available, so the loaders enforce their own
# deadline inside the Python loop; the wait/verify steps are the real guard.

set -u

BATCHES=${1:-8}
TIME_CAP=${2:-155}
MAX_RUNS=${3:-40}
TEST=${4:-"compiler::tests::files::file_workflows_release_handles_buffers_and_cancelled_outputs"}
TAG="JOKY_STRESS_SPIN_TAG_V1"

cd "$(dirname "$0")/.."

total_fail=0
total_runs=0
for batch in $(seq 1 "$BATCHES"); do
  leftover=$(pgrep -f "python3 -c.*$TAG" | wc -l | tr -d " ")
  if [ "$leftover" != "0" ]; then
    echo "batch $batch: $leftover loaders still alive, aborting"
    exit 1
  fi
  for _ in 1 2 3; do
    python3 -c "
import time
end = time.time() + 170
while time.time() < end: pass  # $TAG
" &
  done
  runs=0
  fails=0
  start=$(date +%s)
  while true; do
    now=$(date +%s)
    if [ $((now - start)) -ge "$TIME_CAP" ] || [ "$runs" -ge "$MAX_RUNS" ]; then break; fi
    runs=$((runs + 1))
    total_runs=$((total_runs + 1))
    log="/tmp/load-campaign_b${batch}_r${runs}.log"
    JOKY_STRESS_MIN_ITERATIONS=300 cargo test --lib "$TEST" \
      -- --ignored --exact --nocapture >"$log" 2>&1
    if grep -q FAILED "$log"; then
      fails=$((fails + 1))
      total_fail=$((total_fail + 1))
      echo "batch $batch run $runs: FAILED"
      grep -B 1 -A 8 "panicked" "$log" | head -14
    fi
  done
  wait
  sleep 2
  remaining=$(pgrep -f "python3 -c.*$TAG" | wc -l | tr -d " ")
  echo "batch $batch: $runs runs, $fails failures, leftover loaders: $remaining"
  if [ "$remaining" != "0" ]; then
    echo "loaders refused to die, aborting"
    exit 1
  fi
done
echo "CAMPAIGN TOTAL: $total_fail failures in $total_runs runs"
[ "$total_fail" -eq 0 ]
