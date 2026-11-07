# perfmon — know why your server lags

**perfmon** watches every single server tick and tells you, in game or from the console, exactly how healthy your Pumpkin server is. No guessing from a single TPS number: see tick-time percentiles, catch lag spikes the moment they happen, find which mobs are piling up, and keep a CSV history you can graph.

![perfmon](https://raw.githubusercontent.com/jayclrk358/Perfmon/main/market/screenshot-perf.png)

## Features

- ⏱️ **Real TPS over 5 s, 1 m, 5 m and 15 m**, worked out from measured tick times
- 📊 **Tick-time percentiles**: mean, p50, p95, p99 and worst tick in the last minute
- ⚠️ **Lag spike alerts**: a console warning when a tick passes your threshold (100 ms by default), with a spike history
- 🐄 **Entity counts** per dimension, plus the 10 most common mob types, to find farms and mob pile-ups
- 🧠 **Memory**: machine memory, CPU threads, and the plugin's own footprint
- 📝 **One-command reports**: `/perf report` writes everything, including the last 1,200 tick times, to a text file you can share
- 📈 **CSV metrics log**: one line a minute in `metrics.csv`, ready to graph in any spreadsheet
- 🎨 Colour coded: green is healthy, yellow is close to the limit, red is lagging
- 🪶 Lightweight: one small event per tick, no extra threads, about 1 MB of memory

## Commands

| Command | What it shows |
|---|---|
| `/perf` | TPS, tick-time percentiles, spikes, players and memory |
| `/perf entities` | Entities and players per dimension, top mob types |
| `/perf heap` | Memory and CPU threads |
| `/perf spikes` | The 10 most recent lag spikes |
| `/perf report` | Writes a full report file |
| `/perf reset` | Clears the history |

`/tps` and `/lag` work as aliases. Commands need operator level 2 or the `perfmon:perf` permission.

## Configuration

`plugins/data/perfmon/config.toml` is created on first start:

```toml
spike_threshold_ms = 100        # a tick at least this long counts as a lag spike
warn_on_spike = true            # log a console warning on spikes
spike_warn_cooldown_secs = 10   # at most one warning per this many seconds
csv_interval_secs = 60          # 0 turns the CSV log off
csv_count_entities = true       # include the entity count in each CSV line
```

## Permissions it asks for

| Permission | Why |
|---|---|
| `sys.info` | Machine memory and CPU thread count |
| `fs.read.data` | Reading its config |
| `fs.write.data` | Its config, `metrics.csv` and reports, only inside its own data folder |

perfmon makes no network connections.

To skip the permission prompt on start, add this to `pumpkin.toml`:

```toml
[plugins.overrides.perfmon]
allowed_permissions = ["sys.info", "fs.read.data", "fs.write.data"]
```

## Good to know

Pumpkin gives plugins the machine's memory, not the server's own heap. On Linux, `ps -o rss= -C pumpkin` shows the server's own memory.

Open source under GPL-3.0. Issues and ideas are welcome on GitHub.
