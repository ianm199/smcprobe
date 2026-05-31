"""Correlates the probe harness's sensor samples against each stimulus.

Reads schema.json and samples.jsonl from the output directory, computes for
each stimulus the mean change of every key between its idle pre-phase and its
load phase, then ranks keys by responsiveness and specificity. Output is a
structured analysis.json plus a human-readable analysis.md. Labels are
hypotheses derived from effect size, cross-stimulus specificity, and key-name
conventions; they are correlational, not ground truth.
"""

import json
import sys
from collections import defaultdict
from statistics import mean

STIMULI = ["cpu_all", "cpu_e", "memory", "disk", "gpu", "wifi", "audio",
           "display", "charger_out", "charger_in", "camera"]

EXCLUDE_TYPES = {"ui32", "ui64", "si32", "si64"}
"""Wide integer types are overwhelmingly monotonic accumulators (energy totals,
tick counters), not instantaneous sensors. Their deltas scale with elapsed time
rather than load, so they swamp the ranking with meaningless billions. The real
sensors are floats and narrow fixed-point/integer types."""

PREFIX_HINTS = {
    "Tp": "CPU performance core",
    "Te": "CPU efficiency core",
    "TP": "CPU performance cluster",
    "TPD": "CPU performance core die",
    "TCM": "CPU die hotspot",
    "Tg": "GPU",
    "Tm": "memory / DRAM",
    "TB": "battery",
    "TA": "ambient / airflow",
    "TH": "NAND / storage",
    "TS": "NAND / storage",
    "PZC": "CPU cluster power",
    "PMV": "memory power",
    "PST": "system total power",
}


def load_samples(out_dir):
    samples = []
    with open(f"{out_dir}/samples.jsonl") as fh:
        for line in fh:
            line = line.strip()
            if line:
                samples.append(json.loads(line))
    return samples


def phase_means(samples, stim, phase):
    """Mean value per key across all samples of a stimulus+phase."""
    buckets = defaultdict(list)
    for s in samples:
        if s["stim"] == stim and s["phase"] == phase:
            for key, val in s["sensors"].items():
                buckets[key].append(val)
    return {k: mean(v) for k, v in buckets.items() if v}


def name_hint(key):
    for prefix, label in sorted(PREFIX_HINTS.items(), key=lambda kv: -len(kv[0])):
        if key.startswith(prefix):
            return label
    return None


def main():
    out_dir = sys.argv[1]
    with open(f"{out_dir}/schema.json") as fh:
        schema = json.load(fh)
    samples = load_samples(out_dir)

    deltas = {}
    for stim in STIMULI:
        load = phase_means(samples, stim, "load")
        pre = phase_means(samples, stim, "pre")
        d = {}
        for key, load_val in load.items():
            if key in pre:
                d[key] = load_val - pre[key]
        deltas[stim] = d

    keys = set()
    for d in deltas.values():
        keys.update(d.keys())
    keys = {k for k in keys if schema.get(k) not in EXCLUDE_TYPES}

    records = []
    for key in keys:
        per_stim = {s: round(deltas[s].get(key, 0.0), 3) for s in STIMULI}
        ranked = sorted(per_stim.items(), key=lambda kv: -kv[1])
        top_stim, top_delta = ranked[0]
        second_delta = ranked[1][1] if len(ranked) > 1 else 0.0
        specific = top_delta > 0.5 and top_delta > 2 * max(second_delta, 0.01)
        records.append(
            {
                "key": key,
                "type": schema.get(key, "?"),
                "deltas": per_stim,
                "top_stimulus": top_stim,
                "top_delta": round(top_delta, 3),
                "specific": specific,
                "name_hint": name_hint(key),
            }
        )

    records.sort(key=lambda r: -r["top_delta"])
    with open(f"{out_dir}/analysis.json", "w") as fh:
        json.dump({"stimuli": STIMULI, "records": records}, fh, indent=2)

    lines = ["# SMC sensor mapping — correlational analysis\n"]
    lines.append(f"Samples analyzed: {len(samples)}  |  keys with deltas: {len(records)}\n")
    for stim in STIMULI:
        lines.append(f"\n## Top responders to `{stim}`\n")
        lines.append("| key | type | Δ°/ΔW | specific | name-prefix hint |")
        lines.append("|---|---|---|---|---|")
        top = sorted(records, key=lambda r: -r["deltas"][stim])[:20]
        for r in top:
            if r["deltas"][stim] <= 0.1:
                continue
            lines.append(
                f"| {r['key']} | {r['type']} | {r['deltas'][stim]:+.2f} | "
                f"{'yes' if (r['specific'] and r['top_stimulus']==stim) else ''} | "
                f"{r['name_hint'] or ''} |"
            )

    lines.append("\n## Stimulus-specific keys (strongest single-stimulus response)\n")
    lines.append("| key | type | stimulus | Δ | hint |")
    lines.append("|---|---|---|---|---|")
    for r in records:
        if r["specific"]:
            lines.append(
                f"| {r['key']} | {r['type']} | {r['top_stimulus']} | "
                f"{r['top_delta']:+.2f} | {r['name_hint'] or ''} |"
            )

    with open(f"{out_dir}/analysis.md", "w") as fh:
        fh.write("\n".join(lines) + "\n")

    print(f"Analysis written: {len(records)} keys, {len(samples)} samples.")


if __name__ == "__main__":
    main()
