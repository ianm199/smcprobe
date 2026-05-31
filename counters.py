"""Classifies SMC keys as monotonic counters vs fluctuating sensors, and finds
which counters speed up under CPU load — i.e. candidate hardware energy/activity
accumulators. Operates on the samples already captured in samples.jsonl.
"""

import json
import sys
from collections import defaultdict

out_dir = sys.argv[1]
schema = json.load(open(f"{out_dir}/schema.json"))
samples = [json.loads(l) for l in open(f"{out_dir}/samples.jsonl") if l.strip()]

series = defaultdict(list)
for s in samples:
    for k, v in s["sensors"].items():
        series[k].append((s["t"], s["stim"], s["phase"], v))

counters, sensors = [], 0
for k, pts in series.items():
    vals = [p[3] for p in pts]
    if len(vals) < 10 or max(vals) == min(vals):
        continue
    rises = sum(1 for a, b in zip(vals, vals[1:]) if b >= a - 1e-6)
    frac = rises / (len(vals) - 1)
    if frac >= 0.97:
        counters.append(k)
    else:
        sensors += 1


def phase_rate(pts, stim, phase):
    rates = []
    for (t1, s1, p1, v1), (t2, s2, p2, v2) in zip(pts, pts[1:]):
        if s1 == stim and p1 == phase and s2 == stim and p2 == phase and t2 > t1:
            rates.append((v2 - v1) / (t2 - t1))
    return sum(rates) / len(rates) if rates else None


scored = []
for k in counters:
    idle = phase_rate(series[k], "cpu_all", "pre")
    load = phase_rate(series[k], "cpu_all", "load")
    if idle is not None and load is not None and idle > 0:
        scored.append((k, schema.get(k, "?"), idle, load, load / idle))

scored.sort(key=lambda r: -r[4])

print(f"Total keys analyzed : {len(series)}")
print(f"Monotonic counters  : {len(counters)}")
print(f"Fluctuating sensors : {sensors}")
print()
print("Counters that accelerate most under CPU load (candidate energy/activity meters):")
print(f"{'key':6} {'type':5} {'idle rate/s':>14} {'load rate/s':>14} {'x load/idle':>12}")
for k, t, idle, load, ratio in scored[:15]:
    print(f"{k:6} {t:5} {idle:14.1f} {load:14.1f} {ratio:12.1f}")
