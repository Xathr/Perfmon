//! perfmon: performance, debug and memory monitor for Pumpkin.
//!
//! It times every server tick through the tick-end event, keeps 15 minutes of history, flags lag
//! spikes, counts entities per world, reports system and plugin memory, and appends a metrics
//! line to `plugins/data/perfmon/metrics.csv` on a fixed interval. Everything is reachable with
//! the `/perf` command (aliases `/tps`, `/lag`).

pub mod config;
pub mod stats;

#[cfg(target_arch = "wasm32")]
mod plugin {
    use crate::config::Config;
    use crate::stats::{Stats, human_bytes, mspt_colour, tps_colour};
    use pumpkin_plugin_api::command::{Command, CommandError, CommandNode, CommandSender, ConsumedArgs};
    use pumpkin_plugin_api::commands::CommandHandler;
    use pumpkin_plugin_api::events::{EventHandler, EventPriority, ServerTickEndEvent};
    use pumpkin_plugin_api::permission::{Permission, PermissionDefault};
    use pumpkin_plugin_api::scheduler::SchedulerExt;
    use pumpkin_plugin_api::text::TextComponent;
    use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, Server, permissions, register_plugin};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static STATS: Mutex<Option<Stats>> = Mutex::new(None);
    static CONFIG: Mutex<Option<Config>> = Mutex::new(None);
    static LAST_WARN: Mutex<u64> = Mutex::new(0);
    static FOLDER: Mutex<String> = Mutex::new(String::new());

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }
    fn log(level: pumpkin_plugin_api::logging::LogLevel, msg: &str) {
        pumpkin_plugin_api::logging::log(level, &format!("[perfmon] {msg}"));
    }
    fn cfg() -> Config {
        CONFIG.lock().unwrap().clone().unwrap_or_default()
    }
    fn with_stats<T>(f: impl FnOnce(&mut Stats) -> T) -> T {
        let mut g = STATS.lock().unwrap();
        f(g.get_or_insert_with(|| Stats::new(cfg().spike_threshold_ms * 1000)))
    }
    fn say(sender: &CommandSender, text: &str) {
        sender.send_message(TextComponent::from_legacy_string(text));
    }
    /// Linear memory of this plugin's own sandbox (its "heap").
    fn plugin_heap_bytes() -> u64 {
        core::arch::wasm32::memory_size(0) as u64 * 65_536
    }

    struct TickTimer;
    impl EventHandler<ServerTickEndEvent> for TickTimer {
        fn handle(&self, _server: Server, e: <ServerTickEndEvent as pumpkin_plugin_api::FromIntoEvent>::Data) -> <ServerTickEndEvent as pumpkin_plugin_api::FromIntoEvent>::Data {
            let us = (e.duration_nanos.max(0) as u64) / 1000;
            let t = now();
            if let Some(spike) = with_stats(|s| s.record(e.tick, us, t)) {
                let c = cfg();
                if c.warn_on_spike {
                    let mut last = LAST_WARN.lock().unwrap();
                    if t >= *last + c.spike_warn_cooldown_secs {
                        *last = t;
                        log(
                            pumpkin_plugin_api::logging::LogLevel::Warn,
                            &format!("lag spike: tick {} took {:.1} ms (threshold {} ms). Run /perf spikes for history.", spike.tick, spike.duration_us as f64 / 1000.0, c.spike_threshold_ms),
                        );
                    }
                }
            }
            e
        }
    }

    struct EntityCounts {
        per_world: Vec<(String, usize, u32)>,
        by_type: Vec<(String, usize)>,
        total: usize,
    }
    fn count_entities(server: &Server) -> EntityCounts {
        let mut per_world = Vec::new();
        let mut types: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0;
        for w in server.get_all_worlds() {
            let ents = w.get_entities();
            for e in &ents {
                *types.entry(format!("{:?}", e.get_type()).to_lowercase()).or_default() += 1;
            }
            total += ents.len();
            let name = w.get_dimension();
            let players = server.get_player_count_in_world(w);
            per_world.push((name, ents.len(), players));
        }
        let mut by_type: Vec<(String, usize)> = types.into_iter().collect();
        by_type.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        EntityCounts { per_world, by_type, total }
    }

    fn memory_lines(server: &Server) -> Vec<String> {
        let info = server.get_sys_info();
        let mut out = Vec::new();
        match (info.used_memory, info.total_memory) {
            (Some(used), Some(total)) if total > 0 => out.push(format!(
                "§7System memory: §f{} §7of §f{} §7({:.0}% used)",
                human_bytes(used),
                human_bytes(total),
                used as f64 * 100.0 / total as f64
            )),
            _ => out.push("§7System memory: §cunavailable (grant the sys.info permission)".into()),
        }
        if let Some(cpus) = info.cpu_count {
            out.push(format!("§7CPU threads: §f{cpus}"));
        }
        out.push(format!("§7perfmon sandbox heap: §f{}", human_bytes(plugin_heap_bytes())));
        out
    }

    fn summary_lines(server: &Server) -> Vec<String> {
        let (s5, m1, m5, m15, total_ticks, total_spikes) = with_stats(|s| (s.summary(100), s.summary(1200), s.summary(6000), s.summary(18000), s.total_ticks, s.total_spikes));
        let host_mspt = server.get_mspt();
        let t = |x: f64| format!("{}{x:.2}", tps_colour(x));
        vec![
            "§6§l== Server performance ==".into(),
            format!("§7TPS (5s, 1m, 5m, 15m): {} §7/ {} §7/ {} §7/ {}", t(s5.tps), t(m1.tps), t(m5.tps), t(m15.tps)),
            format!(
                "§7MSPT 1m: mean {}{:.1} §7p50 {:.1} p95 {}{:.1} §7p99 {:.1} max {}{:.1} ms   §8(server avg {:.1})",
                mspt_colour(m1.mean_ms), m1.mean_ms, m1.p50_ms, mspt_colour(m1.p95_ms), m1.p95_ms, m1.p99_ms, mspt_colour(m1.max_ms), m1.max_ms, host_mspt
            ),
            format!("§7Ticks over 50 ms in the last minute: §f{} §7of {}", m1.over_budget, m1.ticks),
            format!("§7Lag spikes since start/reset: §f{total_spikes} §7over {total_ticks} ticks"),
            format!("§7Players: §f{} §7/ {}", server.get_player_count(), server.get_max_players()),
        ]
    }

    fn write_report(server: &Server) -> Result<String, String> {
        let folder = FOLDER.lock().unwrap().clone();
        let t = now();
        let path = format!("{folder}/report-{t}.txt");
        let mut text = String::new();
        let strip = |l: &str| {
            let mut o = String::new();
            let mut skip = false;
            for c in l.chars() {
                if skip { skip = false; continue; }
                if c == '§' { skip = true; continue; }
                o.push(c);
            }
            o
        };
        for l in summary_lines(server) { text.push_str(&strip(&l)); text.push('\n'); }
        for l in memory_lines(server) { text.push_str(&strip(&l)); text.push('\n'); }
        let ec = count_entities(server);
        text.push_str(&format!("\nEntities: {}\n", ec.total));
        for (w, n, p) in &ec.per_world { text.push_str(&format!("  world {w}: {n} entities, {p} players\n")); }
        for (ty, n) in ec.by_type.iter().take(25) { text.push_str(&format!("  {ty}: {n}\n")); }
        text.push_str("\nRecent lag spikes (newest first):\n");
        for s in with_stats(|s| s.recent_spikes(64)) {
            text.push_str(&format!("  unix {} tick {}: {:.1} ms\n", s.unix_secs, s.tick, s.duration_us as f64 / 1000.0));
        }
        let info = server.get_sys_info();
        text.push_str(&format!("\nPumpkin {} on {} {}\n", info.pumpkin_version, info.os_name.unwrap_or_default(), info.os_version.unwrap_or_default()));
        text.push_str("\nLast 1200 tick durations in microseconds, oldest first:\n");
        let ticks = with_stats(|s| s.recent(1200));
        text.push_str(&ticks.iter().map(u64::to_string).collect::<Vec<_>>().join(","));
        text.push('\n');
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(format!("plugins/data/perfmon/report-{t}.txt"))
    }

    fn append_csv(server: &Server) {
        let folder = FOLDER.lock().unwrap().clone();
        let path = format!("{folder}/metrics.csv");
        let new = std::fs::metadata(&path).is_err();
        let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
        if new {
            let _ = writeln!(f, "unix,tps_1m,mspt_mean,mspt_p95,mspt_max,ticks_over_50ms,spikes_total,players,entities,used_memory_bytes,plugin_heap_bytes");
        }
        let (m1, spikes) = with_stats(|s| (s.summary(1200), s.total_spikes));
        let ec = if cfg().csv_count_entities { count_entities(server).total.to_string() } else { String::new() };
        let mem = server.get_sys_info().used_memory.map(|m| m.to_string()).unwrap_or_default();
        let _ = writeln!(f, "{},{:.3},{:.3},{:.3},{:.3},{},{},{},{},{},{}", now(), m1.tps, m1.mean_ms, m1.p95_ms, m1.max_ms, m1.over_budget, spikes, server.get_player_count(), ec, mem, plugin_heap_bytes());
    }

    #[derive(Clone, Copy)]
    enum Action { Summary, Entities, Heap, Spikes, Report, Reset, Help }
    struct Perf(Action);
    impl CommandHandler for Perf {
        fn handle(&self, sender: CommandSender, server: Server, _args: ConsumedArgs) -> Result<i32, CommandError> {
            match self.0 {
                Action::Summary => {
                    for l in summary_lines(&server) { say(&sender, &l); }
                    for l in memory_lines(&server) { say(&sender, &l); }
                    say(&sender, "§8/perf entities | heap | spikes | report | reset");
                }
                Action::Entities => {
                    let ec = count_entities(&server);
                    say(&sender, &format!("§6§l== Entities: {} ==", ec.total));
                    for (w, n, p) in &ec.per_world { say(&sender, &format!("§7{w}: §f{n} §7entities, §f{p} §7players")); }
                    for (ty, n) in ec.by_type.iter().take(10) { say(&sender, &format!("§7  {ty}: §f{n}")); }
                }
                Action::Heap => {
                    say(&sender, "§6§l== Memory ==");
                    for l in memory_lines(&server) { say(&sender, &l); }
                    say(&sender, "§8System memory is the whole machine's; Pumpkin does not expose its own heap to plugins.");
                }
                Action::Spikes => {
                    let list = with_stats(|s| s.recent_spikes(10));
                    say(&sender, &format!("§6§l== Lag spikes (threshold {} ms) ==", cfg().spike_threshold_ms));
                    if list.is_empty() { say(&sender, "§aNone recorded."); }
                    let t = now();
                    for s in list {
                        say(&sender, &format!("§7{}s ago: tick §f{} §7took {}{:.1} ms", t.saturating_sub(s.unix_secs), s.tick, mspt_colour(s.duration_us as f64 / 1000.0), s.duration_us as f64 / 1000.0));
                    }
                }
                Action::Report => match write_report(&server) {
                    Ok(path) => say(&sender, &format!("§aReport written to {path}")),
                    Err(e) => say(&sender, &format!("§cCould not write the report: {e}")),
                },
                Action::Reset => {
                    with_stats(Stats::reset);
                    say(&sender, "§aTick history and spike list cleared.");
                }
                Action::Help => {
                    for l in ["§6/perf §7summary: TPS, MSPT percentiles, players, memory", "§6/perf entities §7entity counts per world and by type", "§6/perf heap §7memory", "§6/perf spikes §7recent lag spikes", "§6/perf report §7write a full report file", "§6/perf reset §7clear history"] {
                        say(&sender, l);
                    }
                }
            }
            Ok(1)
        }
    }

    pub struct Perfmon;
    impl Plugin for Perfmon {
        fn new() -> Self { Self }

        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "perfmon".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                authors: vec!["Mineversed".into()],
                description: "Tick timing, lag spikes, entity counts and memory, with /perf and a CSV log.".into(),
                dependencies: vec![],
                permissions: vec![
                    permissions::SYS_INFO.into(),      // memory, CPU and OS for /perf heap
                    permissions::FS_READ_DATA.into(),  // config.toml
                    permissions::FS_WRITE_DATA.into(), // config.toml, metrics.csv, reports
                ],
            }
        }

        fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
            let folder = context.get_data_folder();
            let config = Config::load_or_create(&format!("{folder}/config.toml")).map_err(|e| format!("perfmon: {e}"))?;
            *FOLDER.lock().unwrap() = folder;
            *STATS.lock().unwrap() = Some(Stats::new(config.spike_threshold_ms * 1000));
            *CONFIG.lock().unwrap() = Some(config.clone());

            context.register_event_handler::<ServerTickEndEvent, _>(TickTimer, EventPriority::Lowest, false)?;

            let _ = context.register_permission(&Permission {
                node: "perfmon:perf".into(),
                description: "Use /perf".into(),
                default: PermissionDefault::Op(pumpkin_plugin_api::permission::PermissionLevel::Two),
                children: vec![],
            });
            let mut cmd = Command::new(&["perf".to_string(), "tps".to_string(), "lag".to_string()], "Server performance and debug info");
            for (name, action) in [("entities", Action::Entities), ("heap", Action::Heap), ("memory", Action::Heap), ("spikes", Action::Spikes), ("report", Action::Report), ("reset", Action::Reset), ("help", Action::Help)] {
                cmd = cmd.then(CommandNode::literal(name).execute(Perf(action)));
            }
            context.register_command(cmd.execute(Perf(Action::Summary)), "perf");

            if config.csv_interval_secs > 0 {
                let period = config.csv_interval_secs * 20;
                context.schedule_repeating_task(period, period, |server| append_csv(&server));
            }
            log(pumpkin_plugin_api::logging::LogLevel::Info, &format!("watching tick times (spike threshold {} ms); use /perf", config.spike_threshold_ms));
            Ok(())
        }
    }

    register_plugin!(Perfmon);
}
