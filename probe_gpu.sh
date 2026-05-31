#!/bin/bash
# Focused GPU mapping probe. Appends baseline + GPU-load samples to the existing
# dataset so analyze.py can identify which sensors respond to GPU work.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/smc_reader"
OUT="$HERE/smc_mapping"
SAMPLES="$OUT/samples.jsonl"
LOG="$OUT/run_gpu.log"
: > "$LOG"

cleanup() { pkill -P $$ 2>/dev/null; pkill -f gpu_stress 2>/dev/null; }
trap cleanup EXIT INT TERM
log() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$LOG"; }

DUR=35; COOL=50; PRE=10; BASE=20; IV=3; REPS=2

sample() {
  local stim="$1" phase="$2" rep="$3" dur="$4"
  local end=$(( $(date +%s) + dur ))
  while [ "$(date +%s)" -lt "$end" ]; do
    local ts sensors
    ts=$(date +%s)
    sensors=$("$BIN" json)
    printf '{"t":%s,"stim":"%s","phase":"%s","rep":%s,"sensors":%s}\n' \
      "$ts" "$stim" "$phase" "$rep" "$sensors" >> "$SAMPLES"
    sleep "$IV"
  done
}

stress_gpu() { timeout "$1" "$HERE/gpu_stress" "$1" & }

log "global baseline"
sample baseline baseline 200 "$BASE"

for rep in $(seq 1 "$REPS"); do
  log "gpu rep $rep: idle pre"
  sample gpu pre "$rep" "$PRE"
  log "gpu rep $rep: LOAD"
  stress_gpu "$DUR"
  sample gpu load "$rep" "$DUR"
  pkill -f gpu_stress 2>/dev/null
  log "gpu rep $rep: cooldown"
  sample gpu cooldown "$rep" "$COOL"
done

log "running analysis"
python3 "$HERE/analyze.py" "$OUT" >> "$LOG" 2>&1
log "DONE"
