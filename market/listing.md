# Pumpkin Market listing: perfmon

Copy each field into the Store Listing form at market.pumpkinmc.org.

| Field | Value |
|---|---|
| Plugin name | perfmon |
| Category | Utilities (your draft already uses it; Admin Tools also fits) |
| Pricing | Free |
| Release channel | Beta |
| Version | 0.1.0 |
| Binary | `perfmon.wasm` from the latest release of this repository |
| Icon | `logo.png` (512×512) |
| Screenshot | `screenshot-perf.png` (1600×900) |
| Source link | https://github.com/jayclrk358/Perfmon |
| Keywords | performance, tps, lag, mspt, profiler, monitor, debug, memory, entities, admin |
| Description | contents of `description.md` |

## Commands

| Name | Aliases | Permission | Description |
|---|---|---|---|
| `perf` | `tps, lag` | `perfmon:perf` | Shows TPS, tick-time percentiles, lag spikes, players and memory |
| `perf entities` | | `perfmon:perf` | Shows entity and player counts per dimension and the top mob types |
| `perf heap` | `perf memory` | `perfmon:perf` | Shows machine memory, CPU threads and perfmon's own memory |
| `perf spikes` | | `perfmon:perf` | Lists the 10 most recent lag spikes |
| `perf report` | | `perfmon:perf` | Writes a full performance report to plugins/data/perfmon |
| `perf reset` | | `perfmon:perf` | Clears the tick history and spike list |

## Short tagline

Real TPS, tick-time percentiles, lag spike alerts, entity counts and memory, with a CSV log.
