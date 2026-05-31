<!-- DRAFT comment for github.com/exelban/stats/issues/1703 — review before posting. -->

I mapped the M3 Max (`Mac15,11`) SMC keys empirically and figured the data might help here.

Rather than guess from names, I drove **isolated workloads** (all-core CPU, Metal GPU, memory bandwidth, disk I/O, plus charger/brightness physical steps) while sampling every numeric key, and kept the keys that respond *specifically* to one subsystem. It's the thermal-correlation approach `iSMC`'s `guess` uses, taken across the full key set. Method, raw dataset, and a reproducible harness are here: **[link to repo]**.

**Temperature families (the part most relevant to this issue):**

| Subsystem | Keys |
|---|---|
| P-cores | `Tp*` — 96 keys (full list in repo); rise +20–30 °C specifically under CPU load |
| E-cores | `Te04 Te05 Te06 Te0K Te0L Te0M Te0P Te0Q Te0S Te0T` |
| GPU | `Tg*` — 32 keys; rise +8–10 °C specifically under Metal load |
| Die hotspot | `TCMb` `TCMz` |
| DRAM | `TRD0 TRD1 TRD3 TRD4 TRD8 TRDb` |
| SSD | `TH0a TH0b TH0x` |

(`Tf*` keys also warm under CPU load, so there appear to be multiple CPU-region families — worth noting if you've been mapping P-cores to `Tf*`.)

**Bonus, if useful:** the 46 `V/I/P` power rails check out against `P = V × I` live, with correlational subsystem attribution (`C0x/C4x/E0b/SVR`→CPU, `C1x/C2x/b0f`→GPU, `C32/P2b/R*`→DRAM, `R8b`→SSD); `PDBR`/`IDBR` track the display backlight; and the `D3*` keys are USB-C-port adapter voltage/current (building on Asahi's `D<n>` port docs).

**Caveats, in fairness:** this is one `Mac15,11`, and the labels are *correlational* (which key moved under which load), not vendor ground truth — the physics (`P=V×I`, thermal response) is verified, the names are inferred. Happy to share the full key lists or open a PR against `values.swift` in whatever shape is useful — didn't want to dump 96 keys unprompted.

Credit to the [Asahi Linux SMC docs](https://asahilinux.org/docs/hw/soc/smc/) for the underlying interface and key-family conventions.
