#!/bin/bash
# Guided "Simon-says" physical-stimulus recorder. Run it in a real Terminal and
# follow the on-screen prompts: it tells you what to do, counts you in, then
# records a window labeled with that stimulus. Appends to the accumulating
# dataset. Read-only on the SMC; the only state it touches is opening/closing
# Photo Booth for the camera step.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/smc_reader"
OUT="$HERE/smc_mapping"
SAMPLES="$OUT/samples.jsonl"

B=$(tput bold 2>/dev/null || true)
RST=$(tput sgr0 2>/dev/null || true)

banner() {
  echo
  echo "${B}════════════════════════════════════════════════════════════${RST}"
  echo "${B}  $*${RST}"
  echo "${B}════════════════════════════════════════════════════════════${RST}"
}

countdown() {
  local n="$1"
  shift
  while [ "$n" -gt 0 ]; do
    printf "\r  ▶ %s  in %d…   " "$*" "$n"
    [ "$n" -le 3 ] && command -v afplay >/dev/null && (afplay /System/Library/Sounds/Tink.aiff >/dev/null 2>&1 &)
    sleep 1
    n=$((n - 1))
  done
  printf "\r  ${B}▶ %s — NOW${RST}                                  \n" "$*"
  command -v say >/dev/null && (say "$*, now" >/dev/null 2>&1 &)
}

sample() {
  local stim="$1" phase="$2" dur="$3"
  local end=$(( $(date +%s) + dur ))
  while [ "$(date +%s)" -lt "$end" ]; do
    printf "\r  ● recording  %-12s %-4s   %2ds left   " "$stim" "$phase" "$(( end - $(date +%s) ))"
    printf '{"t":%s,"stim":"%s","phase":"%s","rep":1,"sensors":%s}\n' \
      "$(date +%s)" "$stim" "$phase" "$("$BIN" json)" >> "$SAMPLES"
    sleep 2
  done
  printf "\r  ✓ recorded   %-12s %-4s                       \n" "$stim" "$phase"
}

clear
echo "${B}hwtwin · Simon-says physical sensor mapping${RST}"
echo
echo "I'll prompt you for a physical action, count you in, then record a window."
echo "Start with the ${B}charger plugged in${RST}. Have it within reach."
echo
printf "Press ${B}Enter${RST} when you're ready… "
read -r _

banner "BASELINE — charger PLUGGED IN, hands off"
sample baseline baseline 18

banner "STEP 1 of 3 · CHARGER  (the big one)"
echo "  Recording the plugged-in state first…"
sample charger_out pre 10
countdown 6 "UNPLUG the charger"
sample charger_out load 28
echo "  Now the reverse — recording the unplugged state…"
sample charger_in pre 10
countdown 6 "PLUG the charger back IN"
sample charger_in load 28

banner "STEP 2 of 3 · DISPLAY BRIGHTNESS"
countdown 6 "Hold the brightness-DOWN key (🔅) to MINIMUM"
sample display pre 10
countdown 6 "Hold the brightness-UP key (🔆) to MAXIMUM"
sample display load 28
echo "  (set brightness back to comfortable whenever you like)"

banner "STEP 3 of 3 · CAMERA"
echo "  Recording with the camera OFF…"
sample camera pre 10
echo "  Opening Photo Booth to power the camera on…"
open -a "Photo Booth" 2>/dev/null
sleep 3
sample camera load 24
osascript -e 'tell application "Photo Booth" to quit' 2>/dev/null

banner "DONE — recording complete"
echo "  Appended to smc_mapping/samples.jsonl."
echo "  Switch back to Claude and say you're done — it'll analyze what lit up."
