//! perfmon: performance, debug and memory monitor for Pumpkin.
//!
//! It times every server tick through the tick-end event, keeps 15 minutes of history, flags lag
//! spikes, counts entities per world, reports system and plugin memory, and appends a metrics
//! line to `plugins/data/perfmon/metrics.csv` on a fixed interval. Everything is reachable with
//! the `/perf` command (aliases `/tps`, `/lag`). At startup it asks the Pumpkin Market whether a newer
//! version is out.

#![allow(non_snake_case)] // the library is named Perfmon so the plugin file is Perfmon.wasm

pub mod banner;
pub mod chat;
pub mod config;
#[cfg(target_arch = "wasm32")]
mod http;
pub mod plugins;
pub mod stats;
pub mod update;

#[cfg(target_arch = "wasm32")]
mod plugin {
    use crate::chat::{self, Part, button, text};
    use crate::config::Config;
    use crate::http;
    use crate::plugins::{self, Answer, Kind, Limits, Registry, State, Watched};
    use crate::stats::{Bucket, Stats, bar, human_bytes, mspt_colour, tps_colour};
    use crate::update::{self, Status};
    use pumpkin_plugin_api::command::{Command, CommandError, CommandNode, CommandSender, ConsumedArgs};
    use pumpkin_plugin_api::commands::CommandHandler;
    use pumpkin_plugin_api::events::{EventHandler, EventPriority, PlayerJoinEvent, ServerTickEndEvent};
    use pumpkin_plugin_api::permission::{Permission, PermissionDefault, PermissionLevel};
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
    static UPDATE: Mutex<Status> = Mutex::new(Status::Off);
    /// The plugins perfmon asks for reports.
    static PLUGINS: Mutex<Option<Registry>> = Mutex::new(None);
    /// The machine's used memory over time, watched for steady growth like a plugin's.
    static MACHINE: Mutex<Option<Watched>> = Mutex::new(None);
    /// Problems already logged, as "plugin kind": each is logged when it starts, not every second.
    static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    const VERSION: &str = env!("CARGO_PKG_VERSION");
    /// Where a downloaded update is saved, as the server admin sees it.
    const UPDATE_FILE: &str = "plugins/data/perfmon/update/Perfmon.wasm";
    /// Ticks to wait after a player joins before telling them about an update.
    const JOIN_NOTICE_DELAY_TICKS: u64 = 60;

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }
    fn tag() -> &'static str {
        if cfg().console_colors { "\x1b[95m[Perfmon]\x1b[0m" } else { "[Perfmon]" }
    }
    fn log(level: pumpkin_plugin_api::logging::LogLevel, msg: &str) {
        pumpkin_plugin_api::logging::log(level, &format!("{} {msg}", tag()));
    }
    /// Puts the [Perfmon] tag in front of every line of a reply shown in the console.
    fn tag_lines(text: &str) -> String {
        let tag = tag();
        text.lines().map(|l| format!("{tag} {l}")).collect::<Vec<_>>().join("\n")
    }
    fn cfg() -> Config {
        CONFIG.lock().unwrap().clone().unwrap_or_default()
    }
    fn with_stats<T>(f: impl FnOnce(&mut Stats) -> T) -> T {
        let mut g = STATS.lock().unwrap();
        f(g.get_or_insert_with(|| Stats::new(cfg().spike_threshold_ms * 1000)))
    }
    fn say(sender: &CommandSender, text: &str) {
        if sender.is_console() {
            // The console prints chat text uncoloured, so send it ANSI-coloured instead.
            let t = if cfg().console_colors { chat::legacy_to_ansi(text) } else { strip_codes(text) };
            sender.send_message(TextComponent::text(&tag_lines(&t)));
        } else {
            sender.send_message(TextComponent::from_legacy_string(text));
        }
    }
    /// Sends a line with click and hover actions; falls back to plain text if the server rejects it.
    fn say_parts(sender: &CommandSender, parts: &[Part]) {
        if sender.is_console() {
            let t = if cfg().console_colors { chat::ansi(parts) } else { chat::plain(parts) };
            sender.send_message(TextComponent::text(&tag_lines(&t)));
            return;
        }
        match TextComponent::from_json(&chat::line(parts)) {
            Ok(c) => sender.send_message(c),
            Err(_) => say(sender, &chat::plain(parts)),
        }
    }
    /// The clickable bar of subcommands shown under most outputs.
    fn nav(sender: &CommandSender) {
        say_parts(sender, &[
            button("Refresh", "/perf", "TPS, tick times, players and memory"),
            text(" ", "gray"),
            button("Graph", "/perf graph", "Tick times over the last minute and 15 minutes"),
            text(" ", "gray"),
            button("Spikes", "/perf spikes", "The most recent lag spikes"),
            text(" ", "gray"),
            button("Entities", "/perf entities", "Entity and player counts per dimension"),
            text(" ", "gray"),
            button("Ping", "/perf ping", "Every player's ping"),
        ]);
        say_parts(sender, &[
            button("Plugins", "/perf plugins", "Memory, tick time and long runs of the plugins that report to perfmon"),
            text(" ", "gray"),
            button("Memory", "/perf heap", "Machine memory and CPU threads"),
            text(" ", "gray"),
            button("Report", "/perf report", "Write everything to a report file"),
            text(" ", "gray"),
            button("Update", "/perf update", "Whether a newer perfmon version is out"),
            text(" ", "gray"),
            text("[Reset]", "red").suggest("/perf reset").hover("Clear the tick history and spike list\nClick to put /perf reset in the chat box"),
        ]);
    }
    fn with_plugins<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
        f(PLUGINS.lock().unwrap().get_or_insert_with(Registry::default))
    }
    fn limits() -> Limits {
        let c = cfg();
        Limits { lag_ms: c.plugin_lag_ms.max(0.1), spike_ms: c.plugin_spike_ms.max(1), leak_bytes: c.plugin_leak_mb.max(1) * 1024 * 1024 }
    }
    /// Steady growth of the whole machine's memory is only worth a word when it is large.
    fn machine_limits() -> Limits {
        Limits { leak_bytes: 256 * 1024 * 1024, ..limits() }
    }

    /// Asks one plugin for its report.
    fn ask(name: &str) -> Answer {
        match pumpkin_plugin_api::ipc::send_ipc_message(name, &plugins::request()) {
            Err(()) => Answer::NotLoaded,
            Ok(Err(_)) => Answer::Silent,
            Ok(Ok(bytes)) => plugins::parse_report(&bytes).map_or(Answer::Silent, Answer::Report),
        }
    }

    /// Logs a problem when it starts and forgets it when it is over, so the console hears of
    /// each once.
    fn warn_once(key: String, active: bool, message: impl FnOnce() -> String) {
        let mut warned = WARNED.lock().unwrap();
        let known = warned.iter().position(|k| *k == key);
        match (active, known) {
            (true, None) => {
                warned.push(key);
                drop(warned);
                if cfg().warn_on_plugin_problem {
                    log(pumpkin_plugin_api::logging::LogLevel::Warn, &message());
                }
            }
            (false, Some(at)) => {
                warned.remove(at);
            }
            _ => {}
        }
    }

    /// Runs every `plugin_poll_secs`: collects every watched plugin's report and the machine's
    /// memory, and logs what is new.
    fn poll_plugins(server: &Server) {
        let t = now();
        let limits = limits();
        let colours = cfg().console_colors;
        let paint = |code: &str, text: &str| if colours { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() };
        for name in with_plugins(|r| r.names()) {
            // asked outside the lock: the plugin may answer by talking to perfmon
            let answer = ask(&name);
            let (spike, findings) = with_plugins(|r| {
                let Some(plugin) = r.get_mut(&name) else { return (None, Vec::new()) };
                let spike = plugin.record(t, answer, &limits);
                (spike, plugin.findings(t, &limits))
            });
            if let Some(spike) = spike
                && cfg().warn_on_plugin_problem
            {
                let mut last = LAST_WARN.lock().unwrap();
                if t >= *last + cfg().spike_warn_cooldown_secs {
                    *last = t;
                    log(
                        pumpkin_plugin_api::logging::LogLevel::Warn,
                        &format!("{} {} ran for {} in one go. Run {} for more.", paint("93", "plugin spike:"), paint("97", &name), paint("91", &format!("{:.1} ms", spike.duration_us as f64 / 1000.0)), paint("96", "/perf plugins")),
                    );
                }
            }
            for kind in [Kind::Lag, Kind::Leak] {
                let finding = findings.iter().find(|f| f.kind == kind);
                warn_once(format!("{name} {kind:?}"), finding.is_some(), || {
                    let what = if kind == Kind::Lag { "plugin lag:" } else { "plugin memory:" };
                    format!("{} {} {}. Run {} for more.", paint("93", what), paint("97", &name), finding.map(|f| f.text.clone()).unwrap_or_default(), paint("96", "/perf plugins"))
                });
            }
        }
        if let Some(used) = server.get_sys_info().used_memory {
            let leak = {
                let mut machine = MACHINE.lock().unwrap();
                let machine = machine.get_or_insert_with(|| Watched::new("machine"));
                machine.record(t, Answer::Report(plugins::Report { perfmon: plugins::PROTOCOL, memory_bytes: Some(used), ..plugins::Report::default() }), &machine_limits());
                machine.findings(t, &machine_limits()).into_iter().find(|f| f.kind == Kind::Leak)
            };
            warn_once("machine Leak".into(), leak.is_some(), || format!("{} the machine's {}. Run {} for more.", paint("93", "memory:"), leak.map(|f| f.text).unwrap_or_default(), paint("96", "/perf heap")));
        }
    }

    /// One line per problem, for the summary and the report: "regions: uses 6.0 ms of every tick ...".
    fn problem_lines() -> Vec<(bool, String)> {
        let t = now();
        let mut out: Vec<(bool, String)> = with_plugins(|r| r.all().iter().flat_map(|p| p.findings(t, &limits()).into_iter().map(|f| (f.severe, format!("{}: {}", p.name, f.text))).collect::<Vec<_>>()).collect());
        if let Some(machine) = MACHINE.lock().unwrap().as_ref() {
            out.extend(machine.findings(t, &machine_limits()).into_iter().filter(|f| f.kind == Kind::Leak).map(|f| (f.severe, format!("machine: {}", f.text))));
        }
        out
    }

    /// `/perf plugins` as chat lines (§ colour codes).
    fn plugin_lines() -> Vec<String> {
        let t = now();
        let limits = limits();
        let mut out = vec!["§6§l== Plugins ==".to_string()];
        if cfg().plugin_poll_secs == 0 {
            out.push("§7Plugin watching is off (plugin_poll_secs = 0 in config.toml).".into());
            return out;
        }
        let lines = with_plugins(|r| {
            let mut lines = Vec::new();
            for p in r.all() {
                match p.state {
                    State::Unknown => lines.push(format!("§f{} §8not asked yet", p.name)),
                    State::NotLoaded => lines.push(format!("§f{} §8is not loaded", p.name)),
                    State::Silent => lines.push(format!("§f{} §7is loaded, but gives perfmon no report", p.name)),
                    State::Reporting => {
                        let mut line = format!("§f{}", p.name);
                        if let Some(version) = &p.version {
                            line.push_str(&format!(" §8v{version}"));
                        }
                        if let Some(memory) = p.memory_bytes() {
                            line.push_str(&format!("  §7memory §f{}", human_bytes(memory)));
                            if let Some(growth) = p.memory_growth(plugins::LEAK_WINDOW_SECS).filter(|g| g.bytes() > 0 && g.secs >= 60) {
                                line.push_str(&format!(" §8(+{} in {} min)", human_bytes(growth.bytes()), growth.secs / 60));
                            }
                        }
                        if let Some(ms) = p.work_ms_per_tick(plugins::LAG_WINDOW_SECS) {
                            let colour = if ms >= limits.lag_ms * 2.0 { "§c" } else if ms >= limits.lag_ms { "§e" } else { "§a" };
                            let share = if p.off_tick { format!("{:.1}% of a CPU core, beside the tick", ms * 2.0) } else { format!("{:.1}% of a tick", ms * 2.0) };
                            line.push_str(&format!("  §7work {colour}{ms:.2} ms/tick §8({share})"));
                        }
                        if let Some(calls) = p.calls_per_sec(plugins::LAG_WINDOW_SECS) {
                            line.push_str(&format!("  §7runs §f{calls:.0}§7/s"));
                        }
                        lines.push(line);
                        if !p.gauges.is_empty() {
                            let gauges: Vec<String> = p.gauges.iter().map(|g| format!("§7{} §f{}{}", g.name, trim_number(g.value), g.unit.as_deref().map(|u| format!(" {u}")).unwrap_or_default())).collect();
                            lines.push(format!("  {}", gauges.join("§8 · ")));
                        }
                        for finding in p.findings(t, &limits) {
                            lines.push(format!("  {}⚠ {}", if finding.severe { "§c" } else { "§e" }, finding.text));
                        }
                        let spikes = p.recent_spikes(3);
                        if !spikes.is_empty() {
                            let list: Vec<String> = spikes.iter().map(|s| format!("{:.1} ms {}s ago", s.duration_us as f64 / 1000.0, t.saturating_sub(s.unix_secs))).collect();
                            lines.push(format!("  §8long runs ({} in all): {}", p.total_spikes, list.join(", ")));
                        }
                    }
                }
            }
            lines
        });
        if lines.is_empty() {
            out.push("§7No plugin is being watched yet.".into());
            out.push("§8Pumpkin does not let one plugin measure another, so a plugin shows up here when it".into());
            out.push("§8reports to perfmon (see the README). To look for one by name, list it under".into());
            out.push("§8plugins = [...] in plugins/data/perfmon/config.toml.".into());
        } else {
            out.extend(lines);
            out.push(format!("§8Limits: {} ms of a tick, runs of {} ms, {} MiB of steady growth in 10 min.", cfg().plugin_lag_ms, cfg().plugin_spike_ms, cfg().plugin_leak_mb));
        }
        out
    }

    /// 12 → "12", 1.50 → "1.5".
    fn trim_number(value: f64) -> String {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
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
                            &if c.console_colors {
                                format!("\x1b[93mlag spike:\x1b[0m tick {} took \x1b[91m{:.1} ms\x1b[0m (threshold {} ms). Run \x1b[96m/perf spikes\x1b[0m for history.", spike.tick, spike.duration_us as f64 / 1000.0, c.spike_threshold_ms)
                            } else {
                                format!("lag spike: tick {} took {:.1} ms (threshold {} ms). Run /perf spikes for history.", spike.tick, spike.duration_us as f64 / 1000.0, c.spike_threshold_ms)
                            },
                        );
                    }
                }
            }
            e
        }
    }

    /// Asks the Pumpkin Market for the latest version and compares it with the running one.
    fn check_update() -> Status {
        match http::get(&update::check_url(VERSION), 64 * 1024).and_then(|body| update::parse_check(&body)) {
            Ok(latest) if update::is_newer(&latest, VERSION) => Status::Available { latest, downloaded: None },
            Ok(latest) => Status::Current { latest },
            Err(e) => Status::Failed(e),
        }
    }

    /// Saves the Market's signed Perfmon.wasm in the data folder, after checking its signature
    /// against the Market's key. A plugin cannot write to plugins/, so the admin copies it over.
    fn download_update(latest: &str) -> Result<(), String> {
        let key = update::parse_public_key(&http::get(update::PUBLIC_KEY_URL, 4096)?)?;
        let dir = format!("{}/update", FOLDER.lock().unwrap());
        let path = format!("{dir}/{}", update::ASSET);
        if std::fs::read(&path).is_ok_and(|have| update::verify_download(&have, &key, latest).is_ok()) {
            return Ok(()); // already downloaded
        }
        let data = http::get(update::DOWNLOAD_URL, 32 * 1024 * 1024)?;
        update::verify_download(&data, &key, latest).map_err(|e| format!("{e}; nothing was saved"))?;
        std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, data)).map_err(|e| format!("{UPDATE_FILE}: {e}"))
    }

    /// Downloads the available update, if there is one, and records where it went.
    fn download_available() -> Result<(), String> {
        let Status::Available { latest, .. } = UPDATE.lock().unwrap().clone() else {
            return Err("there is no newer version to download".into());
        };
        download_update(&latest)?;
        *UPDATE.lock().unwrap() = Status::Available { latest, downloaded: Some(UPDATE_FILE.into()) };
        Ok(())
    }

    /// The update status as chat lines; shown by /perf update and logged at startup.
    fn update_lines(status: &Status) -> Vec<Vec<Part>> {
        let check = button("Check now", "/perf update check", "Ask the Pumpkin Market for the latest version.\nThe server may pause for a moment while it answers.");
        match status {
            Status::Off => vec![vec![text("Update check is off (update_check = false in config.toml). ", "gray"), check]],
            Status::Current { latest } => vec![vec![text(format!("perfmon v{VERSION} is up to date (latest on the Pumpkin Market: v{latest}). "), "green"), check]],
            Status::Failed(why) => vec![vec![text(format!("Update check failed: {why}. "), "red"), check]],
            Status::Available { latest, downloaded } => {
                let mut lines = vec![
                    vec![text(format!("perfmon v{latest} is out"), "yellow").bold(), text(format!(" (this server runs v{VERSION})."), "gray")],
                    vec![text("Pumpkin Market: ", "gray"), text(update::PAGE_URL, "aqua").open(update::PAGE_URL).hover("Click to open perfmon on the Pumpkin Market")],
                ];
                lines.push(match downloaded {
                    Some(path) => vec![
                        text("Downloaded, signed by the Market: ", "green"),
                        text(path.clone(), "white").copy(path.clone()).hover("Click to copy the path"),
                        text(". Stop the server and copy it over plugins/Perfmon.wasm.", "gray"),
                    ],
                    None => vec![
                        button("Download", "/perf update download", "Save the Market's signed Perfmon.wasm in plugins/data/perfmon/update/\nafter checking its signature.\nThe server may pause for a moment while it downloads."),
                        text(" ", "gray"),
                        check,
                    ],
                });
                lines
            }
        }
    }

    fn may_use_perf(player: &pumpkin_plugin_api::Player) -> bool {
        player.has_permission("perfmon:perf") || matches!(player.get_permission_level(), PermissionLevel::Two | PermissionLevel::Three | PermissionLevel::Four)
    }

    /// Tells operators about a newer version shortly after they join.
    struct UpdateNotice;
    impl EventHandler<PlayerJoinEvent> for UpdateNotice {
        fn handle(&self, _server: Server, e: <PlayerJoinEvent as pumpkin_plugin_api::FromIntoEvent>::Data) -> <PlayerJoinEvent as pumpkin_plugin_api::FromIntoEvent>::Data {
            let available = matches!(*UPDATE.lock().unwrap(), Status::Available { .. });
            if available && cfg().update_notify_ops && may_use_perf(&e.player) {
                let id = e.player.get_id();
                pumpkin_plugin_api::scheduler::schedule_delayed_task(JOIN_NOTICE_DELAY_TICKS, move |server| {
                    let Status::Available { latest, .. } = UPDATE.lock().unwrap().clone() else { return };
                    let Some(player) = server.get_player_by_uuid(id) else { return };
                    let line = chat::line(&[
                        text("[Perfmon] ", "light_purple"),
                        text(format!("v{latest} is out"), "yellow"),
                        text(format!(" (this server runs v{VERSION}). "), "gray"),
                        text("[Pumpkin Market]", "aqua").open(update::PAGE_URL).hover(update::PAGE_URL),
                        text(" ", "gray"),
                        button("Details", "/perf update", "Update status and download"),
                    ]);
                    if let Ok(component) = TextComponent::from_json(&line) {
                        player.send_system_message(component, false);
                    }
                });
            }
            e
        }
    }

    /// One row of /perf graph: a bar per bucket, coloured and sized by its slowest tick.
    fn graph_row(sender: &CommandSender, title: &str, buckets: &[Option<Bucket>], secs_per_bar: usize) {
        let top = buckets.iter().flatten().map(|b| b.max_ms).fold(50.0, f64::max);
        say(sender, &format!("§7{title}: {secs_per_bar} s per bar, full height = §f{top:.0} ms"));
        let n = buckets.len();
        let bars: Vec<Part> = buckets
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let (from, to) = ((n - i) * secs_per_bar, (n - i - 1) * secs_per_bar);
                match b {
                    Some(b) => text(bar(b.max_ms, top).to_string(), chat::mspt_color(b.max_ms))
                        .hover(format!("{from} to {to} s ago\nSlowest tick: {:.1} ms\nAverage: {:.1} ms\n{} ticks", b.max_ms, b.mean_ms, b.ticks)),
                    None => text(bar(0.0, top).to_string(), "dark_gray").hover(format!("{from} to {to} s ago\nNo ticks recorded yet")),
                }
            })
            .collect();
        say_parts(sender, &bars);
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
        if let Some(machine) = MACHINE.lock().unwrap().as_ref() {
            if let Some(growth) = machine.memory_growth(plugins::LEAK_WINDOW_SECS).filter(|g| g.secs >= 60) {
                let change = growth.to_bytes as i64 - growth.from_bytes as i64;
                out.push(format!("§7System memory over the last {} min: §f{}{}", growth.secs / 60, if change < 0 { "-" } else { "+" }, human_bytes(change.unsigned_abs())));
            }
            for finding in machine.findings(now(), &machine_limits()).into_iter().filter(|f| f.kind == Kind::Leak) {
                out.push(format!("{}⚠ The machine's {}", if finding.severe { "§c" } else { "§e" }, finding.text));
            }
        }
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

    fn strip_codes(l: &str) -> String {
        let mut o = String::new();
        let mut skip = false;
        for c in l.chars() {
            if skip { skip = false; continue; }
            if c == '§' { skip = true; continue; }
            o.push(c);
        }
        o
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
            let suspect = with_plugins(|r| r.suspect(s.unix_secs, 2).map(|(name, run)| format!(" ({name} ran for {:.1} ms then)", run.duration_us as f64 / 1000.0))).unwrap_or_default();
            text.push_str(&format!("  unix {} tick {}: {:.1} ms{suspect}\n", s.unix_secs, s.tick, s.duration_us as f64 / 1000.0));
        }
        text.push('\n');
        for l in plugin_lines() { text.push_str(&strip(&l)); text.push('\n'); }
        let problems = problem_lines();
        if !problems.is_empty() {
            text.push_str("\nProblems right now:\n");
            for (_, problem) in problems { text.push_str(&format!("  {problem}\n")); }
        }
        let info = server.get_sys_info();
        text.push_str(&format!("\nperfmon {VERSION}, Pumpkin {} on {} {}\n", info.pumpkin_version, info.os_name.unwrap_or_default(), info.os_version.unwrap_or_default()));
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

    /// Checks for a newer version while the server is still starting, so no tick waits for the Market.
    fn startup_update_check(config: &Config) {
        use pumpkin_plugin_api::logging::LogLevel;
        *UPDATE.lock().unwrap() = check_update();
        if config.update_download
            && matches!(*UPDATE.lock().unwrap(), Status::Available { .. })
            && let Err(e) = download_available()
        {
            log(LogLevel::Warn, &format!("could not download the update: {e}"));
        }
        let status = UPDATE.lock().unwrap().clone();
        let level = match status {
            Status::Available { .. } => LogLevel::Warn,
            _ => LogLevel::Info,
        };
        for line in update_lines(&status) {
            // Without the chat buttons, which mean nothing in the log.
            let parts: Vec<Part> = line.into_iter().filter(|p| !p.is_button()).collect();
            if chat::plain(&parts).trim().is_empty() {
                continue;
            }
            log(level, &if config.console_colors { chat::ansi(&parts) } else { chat::plain(&parts) });
        }
    }

    #[derive(Clone, Copy)]
    enum Action { Summary, Graph, Ping, Entities, Heap, Plugins, Spikes, Report, Reset, Update, UpdateCheck, UpdateDownload, Help }
    struct Perf(Action);
    impl CommandHandler for Perf {
        fn handle(&self, sender: CommandSender, server: Server, _args: ConsumedArgs) -> Result<i32, CommandError> {
            match self.0 {
                Action::Summary => {
                    let (s5, m1, m5, m15) = with_stats(|s| (s.summary(100), s.summary(1200), s.summary(6000), s.summary(18000)));
                    let lines = summary_lines(&server);
                    say_parts(&sender, &[text("== Server performance ==", "gold").bold().run("/perf").hover("Click to refresh")]);
                    let tps = |label: &str, v: f64, span: &str| text(format!("{v:.2}"), chat::tps_color(v)).hover(format!("{label}: average TPS over the last {span}\n20.00 is full speed"));
                    say_parts(&sender, &[
                        text("TPS (5s, 1m, 5m, 15m): ", "gray"),
                        tps("5 seconds", s5.tps, "5 seconds"), text(" / ", "gray"),
                        tps("1 minute", m1.tps, "minute"), text(" / ", "gray"),
                        tps("5 minutes", m5.tps, "5 minutes"), text(" / ", "gray"),
                        tps("15 minutes", m15.tps, "15 minutes"),
                    ]);
                    say_parts(&sender, &[
                        text("MSPT 1m: ", "gray").hover("Milliseconds per tick over the last minute.\nUnder 50 ms keeps 20 TPS."),
                        text("mean ", "gray"), text(format!("{:.1}", m1.mean_ms), chat::mspt_color(m1.mean_ms)).hover("Average tick time"),
                        text(" p50 ", "gray"), text(format!("{:.1}", m1.p50_ms), "white").hover("Half of all ticks were faster than this"),
                        text(" p95 ", "gray"), text(format!("{:.1}", m1.p95_ms), chat::mspt_color(m1.p95_ms)).hover("95% of ticks were faster than this"),
                        text(" p99 ", "gray"), text(format!("{:.1}", m1.p99_ms), "white").hover("99% of ticks were faster than this"),
                        text(" max ", "gray"), text(format!("{:.1}", m1.max_ms), chat::mspt_color(m1.max_ms)).hover("The slowest tick in the last minute"),
                        text(" ms", "gray"),
                    ]);
                    say_parts(&sender, &[
                        text(format!("Ticks over 50 ms in the last minute: {} of {}", m1.over_budget, m1.ticks), "gray"),
                    ]);
                    // Spike count links to the spike list.
                    if let Some(l) = lines.get(4) { say_parts(&sender, &[text(strip_codes(l), "gray").run("/perf spikes").hover("Click to list recent lag spikes")]); }
                    if let Some(l) = lines.get(5) { say(&sender, l); }
                    for l in memory_lines(&server) { say(&sender, &l); }
                    for (severe, problem) in problem_lines() {
                        say_parts(&sender, &[text(format!("⚠ {problem}"), if severe { "red" } else { "yellow" }).run("/perf plugins").hover("Click for every watched plugin")]);
                    }
                    nav(&sender);
                }
                Action::Graph => {
                    let (minute, quarter) = with_stats(|s| (s.buckets(30, 40), s.buckets(30, 600)));
                    say(&sender, "§6§l== Tick times ==");
                    graph_row(&sender, "Last minute", &minute, 2);
                    graph_row(&sender, "Last 15 minutes", &quarter, 30);
                    say(&sender, "§8Oldest on the left. A bar is the slowest tick in its time span: green up to 40 ms, yellow up to 50, red over.");
                    nav(&sender);
                }
                Action::Ping => {
                    let mut pings: Vec<(String, u32)> = server.get_all_players().iter().map(|p| (p.get_name(), p.get_ping())).collect();
                    pings.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    if pings.is_empty() {
                        say(&sender, "§6§l== Ping ==");
                        say(&sender, "§7No players online.");
                    } else {
                        let mean = pings.iter().map(|p| p.1 as f64).sum::<f64>() / pings.len() as f64;
                        say(&sender, &format!("§6§l== Ping: {} players, average {mean:.0} ms ==", pings.len()));
                        for (name, ms) in pings.iter().take(15) {
                            say_parts(&sender, &[text(format!("{name}: "), "gray"), text(format!("{ms} ms"), chat::ping_color(*ms))]);
                        }
                        if pings.len() > 15 {
                            say(&sender, &format!("§8and {} more with lower ping", pings.len() - 15));
                        }
                    }
                    nav(&sender);
                }
                Action::Update | Action::UpdateCheck | Action::UpdateDownload => {
                    if matches!(self.0, Action::UpdateCheck) {
                        *UPDATE.lock().unwrap() = check_update();
                    }
                    if matches!(self.0, Action::UpdateDownload) && let Err(e) = download_available() {
                        say(&sender, &format!("§cDownload failed: {e}"));
                    }
                    let status = UPDATE.lock().unwrap().clone();
                    for mut line in update_lines(&status) {
                        if sender.is_console() {
                            // Buttons cannot be clicked in the console.
                            line.retain(|p| !p.is_button());
                            if chat::plain(&line).trim().is_empty() { continue; }
                        }
                        say_parts(&sender, &line);
                    }
                    if sender.is_console() {
                        say(&sender, "§8Commands: /perf update check, /perf update download");
                    }
                }
                Action::Entities => {
                    let ec = count_entities(&server);
                    say(&sender, &format!("§6§l== Entities: {} ==", ec.total));
                    for (w, n, p) in &ec.per_world { say(&sender, &format!("§7{w}: §f{n} §7entities, §f{p} §7players")); }
                    for (ty, n) in ec.by_type.iter().take(10) { say(&sender, &format!("§7  {ty}: §f{n}")); }
                    nav(&sender);
                }
                Action::Heap => {
                    say(&sender, "§6§l== Memory ==");
                    for l in memory_lines(&server) { say(&sender, &l); }
                    say(&sender, "§8System memory is the whole machine's; Pumpkin does not expose its own heap to plugins.");
                    nav(&sender);
                }
                Action::Plugins => {
                    for l in plugin_lines() { say(&sender, &l); }
                    nav(&sender);
                }
                Action::Spikes => {
                    let list = with_stats(|s| s.recent_spikes(10));
                    say(&sender, &format!("§6§l== Lag spikes (threshold {} ms) ==", cfg().spike_threshold_ms));
                    if list.is_empty() { say(&sender, "§aNone recorded."); }
                    let t = now();
                    for s in list {
                        let ms = s.duration_us as f64 / 1000.0;
                        say_parts(&sender, &[
                            text(format!("{}s ago: tick ", t.saturating_sub(s.unix_secs)), "gray"),
                            text(s.tick.to_string(), "white").copy(s.tick.to_string()).hover(format!("Tick {}\nUnix time {}\nClick to copy the tick number", s.tick, s.unix_secs)),
                            text(" took ", "gray"),
                            text(format!("{ms:.1} ms"), chat::mspt_color(ms)).hover(format!("{:.1}x the 50 ms tick budget", ms / 50.0)),
                            // a plugin that reported a long run in the same couple of seconds
                            match with_plugins(|r| r.suspect(s.unix_secs, 2).map(|(name, run)| (name.to_string(), run.duration_us))) {
                                Some((name, us)) => text(format!("  {name} ran for {:.1} ms then", us as f64 / 1000.0), "yellow").run("/perf plugins").hover("A watched plugin reported a long run of its own code\nwithin two seconds of this tick.\nClick for every watched plugin"),
                                None => text("", "gray"),
                            },
                        ]);
                    }
                    nav(&sender);
                }
                Action::Report => match write_report(&server) {
                    Ok(path) => say_parts(&sender, &[
                        text("Report written to ", "green"),
                        text(path.clone(), "white").copy(path).hover("Click to copy the path"),
                    ]),
                    Err(e) => say(&sender, &format!("§cCould not write the report: {e}")),
                },
                Action::Reset => {
                    with_stats(Stats::reset);
                    with_plugins(Registry::reset);
                    WARNED.lock().unwrap().clear();
                    say_parts(&sender, &[text("Tick history and spike list cleared. ", "green"), button("Refresh", "/perf", "Show the summary")]);
                }
                Action::Help => {
                    for (cmd, what) in [("/perf", "summary: TPS, MSPT percentiles, players, memory"), ("/perf graph", "tick times as a bar graph"), ("/perf spikes", "recent lag spikes"), ("/perf entities", "entity counts per world and by type"), ("/perf ping", "every player's ping"), ("/perf heap", "memory"), ("/perf plugins", "memory, tick time and long runs of plugins that report to perfmon"), ("/perf report", "write a full report file"), ("/perf update", "check for a newer perfmon version"), ("/perf reset", "clear history")] {
                        let c = text(cmd, "gold");
                        let c = if cmd == "/perf reset" { c.suggest(cmd) } else { c.run(cmd) };
                        say_parts(&sender, &[c.hover(format!("Click for {cmd}")), text(format!(" {what}"), "gray")]);
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
                authors: vec!["Xathr".into()],
                description: "Tick timing, lag spikes, entity counts and memory, with /perf and a CSV log.".into(),
                dependencies: vec![],
                permissions: vec![
                    permissions::SYS_INFO.into(),      // memory, CPU and OS for /perf heap
                    permissions::FS_READ_DATA.into(),  // config.toml
                    permissions::FS_WRITE_DATA.into(), // config.toml, metrics.csv, reports, downloaded updates
                    permissions::HTTP_OUTBOUND.into(), // update check against the Pumpkin Market
                ],
            }
        }

        /// A plugin introducing itself: it is watched from now on.
        fn handle_ipc_message(&self, _sender: String, message: Vec<u8>) -> Result<Vec<u8>, String> {
            let name = plugins::parse_hello(&message).ok_or("perfmon only takes {\"perfmon\":1,\"hello\":\"<plugin name>\"}")?;
            with_plugins(|r| r.add(&name));
            Ok(plugins::welcome())
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
                default: PermissionDefault::Op(PermissionLevel::Two),
                children: vec![],
            });
            let mut cmd = Command::new(&["perf".to_string(), "tps".to_string(), "lag".to_string()], "Server performance and debug info");
            context.register_event_handler::<PlayerJoinEvent, _>(UpdateNotice, EventPriority::Lowest, false)?;
            cmd = cmd.then(
                CommandNode::literal("update")
                    .then(CommandNode::literal("check").execute(Perf(Action::UpdateCheck)))
                    .then(CommandNode::literal("download").execute(Perf(Action::UpdateDownload)))
                    .execute(Perf(Action::Update)),
            );
            for (name, action) in [("graph", Action::Graph), ("ping", Action::Ping), ("entities", Action::Entities), ("heap", Action::Heap), ("memory", Action::Heap), ("plugins", Action::Plugins), ("spikes", Action::Spikes), ("report", Action::Report), ("reset", Action::Reset), ("help", Action::Help)] {
                cmd = cmd.then(CommandNode::literal(name).execute(Perf(action)));
            }
            context.register_command(cmd.execute(Perf(Action::Summary)), "perf");

            {
                let mut registry = Registry::default();
                for name in &config.plugins {
                    if !registry.add(name) {
                        log(pumpkin_plugin_api::logging::LogLevel::Warn, &format!("plugins in config.toml: \"{name}\" is not a plugin name perfmon can watch"));
                    }
                }
                *PLUGINS.lock().unwrap() = Some(registry);
            }
            if config.plugin_poll_secs > 0 {
                let period = config.plugin_poll_secs * 20;
                context.schedule_repeating_task(period, period, |server| poll_plugins(&server));
            }
            if config.csv_interval_secs > 0 {
                let period = config.csv_interval_secs * 20;
                context.schedule_repeating_task(period, period, |server| append_csv(&server));
            }
            let info = crate::banner::Info {
                version: VERSION,
                spike_threshold_ms: config.spike_threshold_ms,
                warn_on_spike: config.warn_on_spike,
                csv_interval_secs: config.csv_interval_secs,
                update_check: config.update_check,
                update_download: config.update_download,
            };
            for line in crate::banner::lines(&info, config.console_colors) {
                log(pumpkin_plugin_api::logging::LogLevel::Info, &line);
            }
            if config.update_check {
                startup_update_check(&config);
            }
            Ok(())
        }
    }

    register_plugin!(Perfmon);
}
