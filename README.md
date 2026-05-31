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

## Findings (Apple M3 Max, `Mac15,11`)

| Subsystem | Keys | Stimulus that mapped it | Confidence |
|---|---|---|---|
| CPU P-cores | `Tp*` (96) | all-core compute | high |
| CPU E-cores | `Te*` (10) | compute | medium |
| GPU | `Tg*` (32) | Metal compute | high |
| Die hotspot | `TCMb` `TCMz` | — | high |
| DRAM temp / power | `TRD*` / `PMVC` | memory bandwidth | med-high |
| SSD temp | `TH0*` | disk I/O | high |
| **Power rails (46)** | `V/I/P<suffix>` | — | **high (P = V × I verified live)** |
| Battery internals | `B*` (cells, charge, temps) | charger transitions | medium |
| **AC adapter / charge input** | **`D3*`** (21; `D3V*`=voltage, `D3I*`=current), `AC*` | charger plug/unplug | high |
| **Display backlight** | **`PDBR` / `IDBR`** | brightness min→max | high |
| Neural Engine | shared `Ta0*`; faint candidates `Th00-02`/`Ts0h-i`/rail `C00` | CoreML conv on ANE | low — no dedicated sensor |

Rail subsystem attribution (correlational): `C0x/C4x/E0b/SVR` → CPU/SoC, `C1x/C2x/b0f` → GPU,
`C32/P2b/R*` → DRAM, `R8b` → SSD, `C00` → ANE-adjacent.

**Two negative results worth recording:**
- **73 `ui32` keys are free-running clocks** (constant rate, not load-coupled) — not energy meters; excluded from analysis.
- **The `o*` family (358 keys) is static** — max delta `0.000` across *every* stimulus (compute, GPU, memory, disk, Wi-Fi, audio, charger, display, camera, ANE). They are not load-driven sensors but config/calibration/identity values; not crackable by stimulus-response.

### What's new here vs. existing tools

Prior art: `exelban/stats` and VirtualSMC ship a sparse curated subset of Apple Silicon
temp keys (no rails). **Asahi Linux** goes furthest — `macsmc-hwmon` (kernel 6.19) exposes
the raw T/V/I/P sensors and documents a few specific keys (`D1in`/`D2??` USB-C *ports*,
`gP12` backlight gate, `TB0T`/`TCHP`/`TW0P`) — but, in their own words, you "mostly have to
guess based on the four-character name," and there is **no per-rail subsystem attribution**.

This project's additive deltas (checked against Asahi's docs):

1. **Empirical per-rail subsystem attribution** — which `C/P/R` rail is CPU / GPU / DRAM / SSD,
   by stimulus correlation. Asahi exposes the rails but doesn't attribute them.
2. **`P = V × I` verified across 46 rails** — confirms the V/I/P decode; not done elsewhere.
3. **Full-family temp attribution** (`Tp/Te/Tg` → P-core/E-core/GPU) by stimulus, vs. guess-by-name.
4. The **`D3*` per-port electrical keys** (voltage/current) — extending Asahi's `D<n>`=USB-C-port
   insight with the electrical sub-keys and plug/unplug behavior — plus the **`PDBR`/`IDBR`
   backlight power/current rail** alongside their `gP12` gate.
5. A **reproducible stimulus-correlation method + dataset + live twin**, not hand-curated guesses.

Credit: the Apple Silicon SMC interface and the `D`/`g` key families were first documented by
the [Asahi Linux](https://asahilinux.org/docs/hw/soc/smc/) project.

## Mapping a new Mac

Run the harness on any Mac and it produces a `profiles/<hw.model>.profile`:

```bash
cargo build --release
bash probe.sh            # CPU/E-core/memory/disk    bash probe_gpu.sh   # + GPU (Metal)
bash probe_peripherals.sh # Wi-Fi/audio              bash simon.sh        # guided physical: charger/brightness/camera
bash probe_ane.sh        # Neural Engine (needs ane-venv: python3.11 -m venv + pip install coremltools)
python3 analyze.py smc_mapping     # ranked per-stimulus specificity
python3 peripherals.py smc_mapping # low-signal, peripheral-specific pass
python3 counters.py smc_mapping    # separate sensors from clocks
```

Then transcribe the confident keys into `profiles/<model>.profile` and the tool auto-loads
it. Contributions of new-model profiles welcome.

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

**Done:** sensor reading is behind a `SensorSource` trait (`src/sensors.rs`); the twin and
analysis consume a hardware-independent `Snapshot` (`src/model.rs`). Sensor maps are
**data, not code** — `profiles/<hw.model>.profile` text files, auto-detected at runtime
(disk first, so a new Mac is a dropped-in file with no recompile; known profiles are also
bundled into the binary). Running on an unmapped model prints guidance to map it.

```
profiles/
└── Mac15,11.profile     # Apple M3 Max — the mapping output, as portable data
```

**Planned:** Linux (`/sys/class/hwmon`) and Windows (LibreHardwareMonitor) `SensorSource`
backends — the twin, power tree, and analysis won't change. A WASM build of the *analysis*
layer (operating on an uploaded `samples.jsonl`) is a natural shareable companion: capture
stays native, insight goes to the web.

## Honesty

- **Reads only** — the SMC interface here has no write path.
- Sensor **labels are correlational hypotheses**, not vendor ground truth; the *physics*
  (P = V × I, thermal response) is verified, the *names* are inferred.
- Spatial layout in the twin is stylized, not the real die floorplan.

## License

MIT — see [LICENSE](LICENSE).
