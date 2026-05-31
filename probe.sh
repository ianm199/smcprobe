#!/bin/bash
# Autonomous SMC sensor-mapping probe harness.
# Runs a serial stimulus matrix, sampling all numeric SMC keys before/during/
# after each load, and logs structured JSONL for later triangulation. Read-only
# on the SMC; every load is timeout-bounded and children are killed on exit.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/smc_reader"
OUT="$HERE/smc_mapping"
mkdir -p "$OUT"
SAMPLES="$OUT/samples.jsonl"
LOG="$OUT/run.log"
: > "$SAMPLES"
: > "$LOG"

cleanup() {
  pkill -P $$ 2>/dev/null
  rm -f "$OUT/diskbench.tmp" 2>/dev/null
}
trap cleanup EXIT INT TERM

log() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$LOG"; }

DUR=35       # seconds of sustained load per rep
COOL=50      # seconds of cooldown sampling per rep
PRE=10       # seconds of per-rep idle baseline
BASE=20      # seconds of global baseline
IV=3         # seconds between samples
REPS=2

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

stress_cpu_all() { for _ in $(seq 14); do timeout "$1" sh -c 'while :; do :; done' & done; }
stress_cpu_e()   { for _ in $(seq 8);  do timeout "$1" taskpolicy -b sh -c 'while :; do :; done' & done; }
stress_mem()     { for _ in $(seq 4);  do timeout "$1" dd if=/dev/zero of=/dev/null bs=64m & done; }
stress_disk() {
  local dur="$1" f="$OUT/diskbench.tmp"
  ( local end=$(( $(date +%s) + dur ))
    while [ "$(date +%s)" -lt "$end" ]; do
      dd if=/dev/zero of="$f" bs=1m count=256 conv=fsync 2>/dev/null
    done
    rm -f "$f" ) &
}

run_stim() {
  local name="$1" fn="$2"
  for rep in $(seq 1 "$REPS"); do
    log "$name rep $rep: idle pre"
    sample "$name" pre "$rep" "$PRE"
    log "$name rep $rep: LOAD"
    "$fn" "$DUR"
    sample "$name" load "$rep" "$DUR"
    pkill -P $$ 2>/dev/null
    log "$name rep $rep: cooldown"
    sample "$name" cooldown "$rep" "$COOL"
  done
}

log "writing schema"
"$BIN" schema > "$OUT/schema.json"

log "global baseline"
sample baseline baseline 0 "$BASE"

run_stim cpu_all stress_cpu_all
run_stim cpu_e   stress_cpu_e
run_stim memory  stress_mem

FREE_GB=$(df -g "$OUT" | awk 'NR==2{print $4}')
if [ "${FREE_GB:-0}" -gt 5 ]; then
  run_stim disk stress_disk
else
  log "SKIP disk: only ${FREE_GB}GB free"
fi

log "final baseline"
sample baseline baseline 99 "$BASE"

log "running analysis"
python3 "$HERE/analyze.py" "$OUT" >> "$LOG" 2>&1

log "DONE — see $OUT/analysis.md and analysis.json"
