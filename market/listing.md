# Pumpkin Market listing: perfmon

Copy each field into the Store Listing form at market.pumpkinmc.org.

| Field | Value |
|---|---|
| Plugin name | perfmon |
| Category | Utilities (your draft already uses it; Admin Tools also fits) |
| Pricing | Free |
| Release channel | Beta |
| Version | 0.3.2 |
| Binary | `Perfmon.wasm` from the latest release of this repository |
| Icon | `logo.png` (512×512) |
| Screenshots | `screenshot-perf.png`, `screenshot-graph.png`, `screenshot-console.png` (1600×900 each) |
| Source link | https://github.com/Xathr/Perfmon |
| Keywords | performance, tps, lag, mspt, profiler, monitor, debug, memory, entities, admin |
| Description | contents of `description.md` |

## Commands

| Name | Aliases | Permission | Description |
|---|---|---|---|
| `perf` | `tps, lag` | `perfmon:perf` | Shows TPS, tick-time percentiles, lag spikes, players and memory |
| `perf graph` | | `perfmon:perf` | Shows tick times as bar graphs for the last minute and the last 15 minutes |
| `perf ping` | | `perfmon:perf` | Lists every player's ping and the average |
| `perf entities` | | `perfmon:perf` | Shows entity and player counts per dimension and the top mob types |
| `perf heap` | `perf memory` | `perfmon:perf` | Shows machine memory, CPU threads and perfmon's own memory |
| `perf spikes` | | `perfmon:perf` | Lists the 10 most recent lag spikes |
| `perf report` | | `perfmon:perf` | Writes a full performance report to plugins/data/perfmon |
| `perf update` | | `perfmon:perf` | Shows whether a newer perfmon version is out |
| `perf update check` | | `perfmon:perf` | Asks the Pumpkin Market for the latest version now |
| `perf update download` | | `perfmon:perf` | Downloads the Market-signed Perfmon.wasm into plugins/data/perfmon/update |
| `perf reset` | | `perfmon:perf` | Clears the tick history and spike list |

## Short tagline

Real TPS, tick-time percentiles and graphs, lag spike alerts, entity counts, ping and memory, with a CSV log.
