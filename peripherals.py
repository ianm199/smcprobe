"""Low-signal, specificity-aware analysis for the peripheral sweep.

Peripheral subsystems (Wi-Fi radio, audio amp) draw far less than compute, so
their sensor responses are small. This pass ranks keys by their response to a
peripheral stimulus, and flags ones that respond to a peripheral MORE than to
any compute load — those are candidate peripheral sensors, with special
attention to the unmapped o*/D* families.
"""

import json
import sys
from collections import defaultdict
from statistics import mean

out = sys.argv[1]
schema = json.load(open(f"{out}/schema.json"))
samples = [json.loads(l) for l in open(f"{out}/samples.jsonl") if l.strip()]

PERIPH = ["wifi", "audio", "display", "charger_out", "charger_in", "camera"]
COMPUTE = ["cpu_all", "cpu_e", "memory", "disk", "gpu"]
EXCLUDE_TYPES = {"ui32", "ui64", "si32", "si64"}  # free-running counters, not sensors


def phase_mean(stim, phase):
    b = defaultdict(list)
    for s in samples:
        if s["stim"] == stim and s["phase"] == phase:
            for k, v in s["sensors"].items():
                b[k].append(v)
    return {k: mean(v) for k, v in b.items() if v}


def delta(stim):
    load, pre = phase_mean(stim, "load"), phase_mean(stim, "pre")
    return {k: load[k] - pre[k] for k in load if k in pre}


present = {s["stim"] for s in samples}
pdel = {s: delta(s) for s in PERIPH if s in present}
cdel = {s: delta(s) for s in COMPUTE if s in present}


def family(k):
    return {"o": "o* (unmapped)", "D": "D* (unmapped)"}.get(k[0], k[0] + "*")


keys = set().union(*[d.keys() for d in pdel.values()]) if pdel else set()
keys = {k for k in keys if schema.get(k) not in EXCLUDE_TYPES}
rows = []
for k in keys:
    pd = {s: pdel[s].get(k, 0.0) for s in pdel}
    best = max(pd, key=lambda x: pd[x])
    bd = pd[best]
    ranked = sorted(pd.values(), reverse=True)
    second = ranked[1] if len(ranked) > 1 else 0.0
    cmax = max((cdel[s].get(k, 0.0) for s in cdel), default=0.0)
    specific = bd > 0.12 and bd > cmax and bd > 1.5 * max(second, 0.01)
    rows.append((k, schema.get(k, "?"), best, bd, cmax, specific, family(k)))

rows.sort(key=lambda r: -r[3])

print(f"Peripheral sweep — stimuli present: {sorted(pdel)}")
print(f"\nTop keys by peripheral response:")
print(f"{'key':6} {'type':5} {'stim':8} {'dPeriph':>8} {'dCompute':>9} {'spec':>4}  family")
for k, t, s, bd, cm, sp, f in rows[:30]:
    if bd <= 0.05:
        continue
    print(f"{k:6} {t:5} {s:8} {bd:8.3f} {cm:9.3f} {'Y' if sp else '':>4}  {f}")

spec = [r for r in rows if r[5]]
print(f"\n{len(spec)} peripheral-specific keys (respond to a peripheral > any compute load):")
for k, t, s, bd, cm, sp, f in spec:
    print(f"  {k} [{t}] -> {s}  (delta {bd:.3f}, {f})")
