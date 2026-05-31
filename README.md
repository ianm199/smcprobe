# hwtwin — a live hardware digital twin

A self-contained tool that reads a machine's raw sensors and renders a **live digital
twin** of the silicon — per-core temperature heatmaps, a power-delivery tree, fans,
battery, and energy — in the browser. Plus a small **reverse-engineering harness** that
maps undocumented sensors to physical subsystems by correlating them against controlled
workloads.

Today it speaks to the **Apple SMC** (System Management Controller) on Apple Silicon via
IOKit, with no external dependencies. The architecture is built to grow other hardware
backends (see [Roadmap](#roadmap)).

> Status: working on Apple M3 Max (MacBook Pro, `Mac15,11`). Sensor *labels* are
> empirical/correlational, not vendor ground truth — see [Honesty](#honesty).

---

## What it does

- **Live twin** (`serve`) — a browser visualization streamed over local SSE at 2 Hz:
  - **Per-core heatmap** of all 96 P-core, 10 E-core, and 32 GPU thermal sensors —
    watch heat move across the cluster as the scheduler shifts work.
  - **Power tree** — 46 voltage/current/power rails, each cross-checked against
    **P = V × I** live (✓ when within 15%).
  - DRAM, SSD, battery, both fans (spinning at true RPM), system power, integrated
    energy, and a throttle indicator.
- **Terminal dashboard** (no args) — the same data as a 1 Hz TUI.
- **Raw access** — `scan` (dump every decodable key), `json` (one snapshot),
  `schema` (key → type), `once` (single twin frame).

## How it works

The tool is a **driver client**: it opens a user-client connection to the `AppleSMC`
IOKit service and issues struct calls (`KERNEL_INDEX_SMC`) to read four-character keys.
Each key carries a type tag (`flt`, `ui8/16/32`, `sp78`, `fpe2`); the decoder honors the
per-type endianness (floats little-endian, integers big-endian — the classic trap). See
[`src/main.rs`](src/main.rs); the embedded UI is [`src/twin.html`](src/twin.html).

## Mapping methodology (the interesting part)

Apple documents none of the ~2,770 SMC keys. We recover meaning by **differential
stimulus-response correlation** — the same technique `iSMC`'s `guess` command uses:

1. **Baseline** — sample all keys at idle.
2. **Apply an isolated stimulus** — a workload that exercises exactly one subsystem
   (`gpu_stress.swift` for the GPU; busy loops for CPU; `dd` for memory/disk).
3. **Diff & attribute** — a key that rises *specifically* under one stimulus (and not
   others) is attributed to that subsystem.

Run it yourself:

```bash
bash probe.sh          # CPU / E-core / memory / disk stimulus matrix → samples.jsonl
bash probe_gpu.sh      # adds a Metal GPU stimulus (needs gpu_stress)
python3 analyze.py smc_mapping     # ranked per-stimulus specificity table
python3 counters.py smc_mapping    # separates real sensors from monotonic clocks
```

Outputs land in [`smc_mapping/`](smc_mapping/): `schema.json`, `analysis.md/json`,
`experiment_log.json` (the stimulus hypotheses). Raw `samples.jsonl` is regenerable and
git-ignored.

## Findings so far (Apple M3 Max)

- **CPU/SoC temps:** the `Tp*` family (96 keys) rises +20–30 °C specifically under CPU
  load; `Te*` (10) tracks E-cores; `TCMb/TCMz` are the die hotspot.
- **GPU temps:** the `Tg*` family (32 keys) rises +8–10 °C specifically under Metal load.
- **DRAM:** `TRD*`; **SSD:** `TH0*` (specific to disk I/O).
- **Power rails:** 46 rails expose voltage + current + power for the same suffix, and
  **P = V × I holds live** — independent proof the decode is correct. Correlational
  attribution: `C0x/C4x/E0b/SVR` → CPU/SoC, `C1x/C2x/b0f` → GPU, `C32/P2b/R*` → DRAM,
  `R8b` → SSD.
- **Counters:** 73 keys are monotonic free-running clocks (constant rate, not
  load-coupled) — not energy meters; excluded from sensor analysis.

### What's new here vs. existing tools

Community tools (`exelban/stats`, VirtualSMC, archived key lists) document Intel keys
well and ship a *sparse curated subset* of Apple Silicon temp keys — and **no Apple
Silicon power/voltage/current rails at all**. This project's deltas:

1. A **Watt's-law-verified power-rail map** for Apple Silicon (undocumented elsewhere).
2. **Full-family** temperature coverage with empirical per-stimulus attribution.
3. A **reproducible method + dataset**, not hand-curated guesses.

Findings will be contributed upstream (e.g. `exelban/stats#1703`, VirtualSMC) once the
peripheral-stimulus sweep (`o*`/`D*` families) is complete.

## Quickstart

```bash
cargo run --release -- serve     # live twin at http://127.0.0.1:8077
cargo run --release              # terminal dashboard
cargo run --release -- scan      # dump all ~2,770 keys
```

No dependencies — links Apple's IOKit/CoreFoundation frameworks directly.

## Architecture & Roadmap

The goal is a **simple, powerful, portable** tool: one static binary, an embedded UI, and
a clean split between the privileged OS-specific *sensor source* and the portable *model
+ visualization*.

```
[ OS sensor backend ]  →  [ portable core: snapshot model ]  →  [ twin UI · analysis ]
   AppleSMC (IOKit) ✅           subsystem aggregation              SSE + SVG (done)
   Linux hwmon/lm-sensors ◻        + per-key grids                  stimulus correlation
   Windows LHM/WMI ◻               + rail P=V×I                     (done)
```

Planned: factor sensor reading behind a `SensorSource` trait so the twin and analysis
become hardware-agnostic, then add Linux (`/sys/class/hwmon`) and Windows
(LibreHardwareMonitor) backends. The browser UI is already portable; only the data source
is OS-specific. A WASM build of the *analysis* layer (operating on uploaded
`samples.jsonl`) is a natural shareable companion — capture stays native, insight goes to
the web.

## Honesty

- **Reads only** — the SMC interface here has no write path.
- Sensor **labels are correlational hypotheses**, not vendor ground truth; the *physics*
  (P = V × I, thermal response) is verified, the *names* are inferred.
- Spatial layout in the twin is stylized, not the real die floorplan.

## License

MIT — see [LICENSE](LICENSE).
