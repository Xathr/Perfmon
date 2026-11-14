//! What other plugins say about themselves, and what perfmon makes of it. Pure code with no host
//! calls, so it is unit-tested natively.
//!
//! Pumpkin runs every plugin in its own sandbox and gives none of them a way to look into
//! another: perfmon cannot time another plugin's code or read its memory. What plugins can do is
//! send each other messages. So a plugin that wants to be watched answers one question, "report",
//! with a few numbers it keeps about itself:
//!
//! ```text
//! perfmon asks   {"perfmon":1,"ask":"report"}
//! plugin answers {"perfmon":1,"name":"cascade","version":"0.1.0",
//!                 "memory_bytes":4194304,       its sandbox memory, right now
//!                 "work_us":81234567,           time spent in its own code since it loaded
//!                 "calls":40211,                how many times its code ran
//!                 "max_us":1840,                its longest single run since the last report
//!                 "gauges":[{"name":"npcs","value":12}]}   anything else worth showing
//! ```
//!
//! Every field but the first is optional. A plugin whose work runs beside the server tick instead
//! of inside it (a network loop, say) adds `"off_tick":true`: its time is CPU used, not tick time
//! lost, and perfmon words it that way and never blames it for a slow tick.
//!
//! More on the above: A plugin can also introduce itself, so the server
//! admin doesn't have to list it in perfmon's config: it sends `{"perfmon":1,"hello":"<its
//! name>"}` to the plugin named `perfmon`.
//!
//! From a series of such reports perfmon works out how much of each tick a plugin uses (lag),
//! whether its memory keeps growing (a leak), and which single runs took long (spikes).

use serde::Deserialize;
use std::collections::VecDeque;

/// The version of the question and answer above.
pub const PROTOCOL: u32 = 1;
/// Reports kept per plugin: 15 minutes at one a second.
pub const HISTORY: usize = 900;
/// Spikes kept per plugin.
pub const SPIKES_KEPT: usize = 32;
/// Plugins perfmon will watch at most (names can arrive in messages from any plugin).
pub const MAX_WATCHED: usize = 64;
/// One server tick at full speed, in milliseconds.
const TICK_MS: f64 = 50.0;
const TICKS_PER_SECOND: f64 = 20.0;

/// The question perfmon sends.
pub fn request() -> Vec<u8> {
    format!("{{\"perfmon\":{PROTOCOL},\"ask\":\"report\"}}").into_bytes()
}

/// The answer perfmon gives a plugin that introduced itself.
pub fn welcome() -> Vec<u8> {
    format!("{{\"perfmon\":{PROTOCOL},\"ok\":true}}").into_bytes()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Gauge {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub unit: Option<String>,
}

/// One answer to "report".
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Report {
    pub perfmon: u32,
    pub name: Option<String>,
    pub version: Option<String>,
    pub memory_bytes: Option<u64>,
    pub work_us: Option<u64>,
    pub calls: Option<u64>,
    pub max_us: Option<u64>,
    /// The plugin's work runs beside the server tick, not inside it.
    pub off_tick: bool,
    pub gauges: Vec<Gauge>,
}

/// Reads a plugin's answer. Anything that isn't a version 1 report is refused, so a plugin that
/// answers messages for reasons of its own isn't mistaken for one that reports.
pub fn parse_report(bytes: &[u8]) -> Result<Report, String> {
    if bytes.len() > 64 * 1024 {
        return Err("the report is over 64 KiB".into());
    }
    let mut report: Report = serde_json::from_slice(bytes).map_err(|e| format!("not a report: {e}"))?;
    if report.perfmon != PROTOCOL {
        return Err(format!("not a version {PROTOCOL} report"));
    }
    report.gauges.truncate(16);
    report.gauges.retain(|g| g.value.is_finite() && !g.name.is_empty() && g.name.len() <= 32);
    for gauge in &mut report.gauges {
        gauge.name = clean(&gauge.name);
        gauge.unit = gauge.unit.as_deref().map(clean).filter(|u| u.len() <= 8);
    }
    report.version = report.version.as_deref().map(clean).filter(|v| !v.is_empty() && v.len() <= 32);
    Ok(report)
}

/// Text from another plugin, safe to show in chat and in the console: no control characters and
/// no formatting codes.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() && *c != '§').collect()
}

/// A plugin name as Pumpkin spells them: lower-case letters, digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// The name in a plugin's introduction, if that is what the message is.
pub fn parse_hello(bytes: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Hello {
        perfmon: u32,
        hello: String,
    }
    let hello: Hello = serde_json::from_slice(bytes.get(..bytes.len().min(1024))?).ok()?;
    (hello.perfmon == PROTOCOL && valid_name(&hello.hello)).then_some(hello.hello)
}

/// What came back when perfmon asked a plugin for its report.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// No plugin of that name is loaded.
    NotLoaded,
    /// The plugin is loaded and doesn't answer the question.
    Silent,
    Report(Report),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not asked yet.
    Unknown,
    NotLoaded,
    Silent,
    Reporting,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    unix_secs: u64,
    memory_bytes: Option<u64>,
    work_us: Option<u64>,
    calls: Option<u64>,
}

/// One long run of a plugin's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginSpike {
    pub unix_secs: u64,
    pub duration_us: u64,
}

/// How a plugin's memory changed over a stretch of time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Growth {
    pub secs: u64,
    pub from_bytes: u64,
    pub to_bytes: u64,
    /// Memory rose in every quarter of the stretch: it isn't a one-off jump, it keeps coming.
    pub steady: bool,
}

impl Growth {
    pub fn bytes(&self) -> u64 {
        self.to_bytes.saturating_sub(self.from_bytes)
    }
    pub fn bytes_per_hour(&self) -> f64 {
        if self.secs == 0 { 0.0 } else { self.bytes() as f64 * 3600.0 / self.secs as f64 }
    }
}

/// The limits past which perfmon speaks up about a plugin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Average time per tick in the plugin's own code, in milliseconds.
    pub lag_ms: f64,
    /// A single run of the plugin's code this long is a spike, in milliseconds.
    pub spike_ms: u64,
    /// Steady memory growth of at least this much over the leak window is a suspected leak.
    pub leak_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { lag_ms: 5.0, spike_ms: 25, leak_bytes: 8 * 1024 * 1024 }
    }
}

/// How far back the lag figure looks.
pub const LAG_WINDOW_SECS: u64 = 60;
/// How far back the leak check looks, and the least history it wants before it says anything.
pub const LEAK_WINDOW_SECS: u64 = 600;
pub const LEAK_MIN_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lag,
    Leak,
    Spike,
}

/// Something perfmon has to say about a plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    /// Past twice the limit: red rather than yellow.
    pub severe: bool,
    pub text: String,
}

/// One plugin perfmon keeps an eye on.
#[derive(Debug, Clone)]
pub struct Watched {
    pub name: String,
    pub state: State,
    pub version: Option<String>,
    /// Its work runs beside the server tick (see [`Report::off_tick`]).
    pub off_tick: bool,
    pub gauges: Vec<Gauge>,
    samples: VecDeque<Sample>,
    spikes: VecDeque<PluginSpike>,
    pub total_spikes: u64,
}

impl Watched {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), state: State::Unknown, version: None, off_tick: false, gauges: Vec::new(), samples: VecDeque::new(), spikes: VecDeque::new(), total_spikes: 0 }
    }

    /// Takes in one answer. Returns the spike, if the report carried one past `limits.spike_ms`.
    pub fn record(&mut self, now: u64, answer: Answer, limits: &Limits) -> Option<PluginSpike> {
        let report = match answer {
            Answer::NotLoaded => {
                self.forget(State::NotLoaded);
                return None;
            }
            Answer::Silent => {
                self.forget(State::Silent);
                return None;
            }
            Answer::Report(report) => report,
        };
        // A counter that went backwards means the plugin was reloaded: what was measured before
        // belongs to another run of it.
        if let Some(last) = self.samples.back()
            && (report.work_us < last.work_us || report.calls < last.calls || now < last.unix_secs)
        {
            self.samples.clear();
        }
        self.state = State::Reporting;
        self.version = report.version;
        self.off_tick = report.off_tick;
        self.gauges = report.gauges;
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample { unix_secs: now, memory_bytes: report.memory_bytes, work_us: report.work_us, calls: report.calls });
        let duration_us = report.max_us.filter(|us| *us >= limits.spike_ms * 1000)?;
        let spike = PluginSpike { unix_secs: now, duration_us };
        if self.spikes.len() == SPIKES_KEPT {
            self.spikes.pop_front();
        }
        self.spikes.push_back(spike);
        self.total_spikes += 1;
        Some(spike)
    }

    fn forget(&mut self, state: State) {
        self.state = state;
        self.version = None;
        self.gauges.clear();
        self.samples.clear();
    }

    /// The oldest sample no older than `window_secs`, and the newest.
    fn span(&self, window_secs: u64) -> Option<(&Sample, &Sample)> {
        let last = self.samples.back()?;
        let first = self.samples.iter().find(|s| s.unix_secs + window_secs >= last.unix_secs)?;
        (first.unix_secs < last.unix_secs).then_some((first, last))
    }

    pub fn memory_bytes(&self) -> Option<u64> {
        self.samples.back()?.memory_bytes
    }

    /// Seconds of reports perfmon holds for this plugin.
    pub fn watched_secs(&self) -> u64 {
        match (self.samples.front(), self.samples.back()) {
            (Some(first), Some(last)) => last.unix_secs - first.unix_secs,
            _ => 0,
        }
    }

    /// Average time per tick spent in the plugin's own code over the last `window_secs`, in
    /// milliseconds. `None` until there are two reports with a work counter.
    pub fn work_ms_per_tick(&self, window_secs: u64) -> Option<f64> {
        let (first, last) = self.span(window_secs)?;
        let work = last.work_us?.checked_sub(first.work_us?)?;
        let ticks = (last.unix_secs - first.unix_secs) as f64 * TICKS_PER_SECOND;
        Some(work as f64 / 1000.0 / ticks)
    }

    /// How many times a second the plugin's code ran over the last `window_secs`.
    pub fn calls_per_sec(&self, window_secs: u64) -> Option<f64> {
        let (first, last) = self.span(window_secs)?;
        let calls = last.calls?.checked_sub(first.calls?)?;
        Some(calls as f64 / (last.unix_secs - first.unix_secs) as f64)
    }

    /// How the plugin's memory changed over the last `window_secs`.
    pub fn memory_growth(&self, window_secs: u64) -> Option<Growth> {
        let (first, last) = self.span(window_secs)?;
        let (from_bytes, to_bytes) = (first.memory_bytes?, last.memory_bytes?);
        let secs = last.unix_secs - first.unix_secs;
        // memory at the start, at each quarter and at the end
        let mut marks = Vec::with_capacity(5);
        for quarter in 0..=4 {
            let at = first.unix_secs + secs * quarter / 4;
            marks.push(self.samples.iter().rev().find(|s| s.unix_secs <= at).and_then(|s| s.memory_bytes)?);
        }
        let steady = secs >= 4 && marks.windows(2).all(|pair| pair[1] > pair[0]);
        Some(Growth { secs, from_bytes, to_bytes, steady })
    }

    /// The longest single run reported in the last `window_secs`.
    pub fn worst_spike(&self, now: u64, window_secs: u64) -> Option<PluginSpike> {
        self.spikes.iter().filter(|s| s.unix_secs + window_secs >= now).max_by_key(|s| s.duration_us).copied()
    }

    pub fn recent_spikes(&self, n: usize) -> Vec<PluginSpike> {
        self.spikes.iter().rev().take(n).copied().collect()
    }

    /// What is worth telling the admin about this plugin right now.
    pub fn findings(&self, now: u64, limits: &Limits) -> Vec<Finding> {
        let mut out = Vec::new();
        if self.state != State::Reporting {
            return out;
        }
        if let Some(ms) = self.work_ms_per_tick(LAG_WINDOW_SECS)
            && ms >= limits.lag_ms
        {
            out.push(Finding {
                kind: Kind::Lag,
                severe: ms >= limits.lag_ms * 2.0,
                text: if self.off_tick {
                    format!("is busy {ms:.1} ms per tick beside the server tick ({:.0}% of one CPU core). This uses CPU; it does not make ticks longer", ms * 100.0 / TICK_MS)
                } else {
                    format!("uses {ms:.1} ms of every tick ({:.0}% of the 50 ms a tick has)", ms * 100.0 / TICK_MS)
                },
            });
        }
        if let Some(growth) = self.memory_growth(LEAK_WINDOW_SECS)
            && growth.secs >= LEAK_MIN_SECS
            && growth.steady
            && growth.bytes() >= limits.leak_bytes
        {
            out.push(Finding {
                kind: Kind::Leak,
                severe: growth.bytes() >= limits.leak_bytes * 2,
                text: format!(
                    "memory has grown steadily for {} min: {} to {} ({}/hour). This looks like a leak",
                    growth.secs / 60,
                    crate::stats::human_bytes(growth.from_bytes),
                    crate::stats::human_bytes(growth.to_bytes),
                    crate::stats::human_bytes(growth.bytes_per_hour() as u64)
                ),
            });
        }
        if let Some(spike) = self.worst_spike(now, LAG_WINDOW_SECS) {
            let ms = spike.duration_us as f64 / 1000.0;
            out.push(Finding {
                kind: Kind::Spike,
                severe: spike.duration_us >= limits.spike_ms * 2000,
                text: if self.off_tick {
                    format!("was busy for {ms:.1} ms in one go {}s ago, beside the server tick", now.saturating_sub(spike.unix_secs))
                } else {
                    format!("ran for {ms:.1} ms in one go {}s ago ({:.1}x a whole tick)", now.saturating_sub(spike.unix_secs), ms / TICK_MS)
                },
            });
        }
        out
    }
}

/// Every plugin perfmon watches, in the order they were added.
#[derive(Debug, Default)]
pub struct Registry {
    plugins: Vec<Watched>,
}

impl Registry {
    /// Starts watching a plugin. `false` if the name is not a plugin name, is perfmon itself, is
    /// already watched, or the list is full.
    pub fn add(&mut self, name: &str) -> bool {
        if !valid_name(name) || name == "perfmon" || self.plugins.len() >= MAX_WATCHED || self.plugins.iter().any(|p| p.name == name) {
            return false;
        }
        self.plugins.push(Watched::new(name));
        true
    }
    pub fn names(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.name.clone()).collect()
    }
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Watched> {
        self.plugins.iter_mut().find(|p| p.name == name)
    }
    pub fn all(&self) -> &[Watched] {
        &self.plugins
    }
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
    /// The plugin whose long run falls closest before `unix_secs`, within `within_secs`: the
    /// likeliest cause of a slow tick at that time.
    pub fn suspect(&self, unix_secs: u64, within_secs: u64) -> Option<(&str, PluginSpike)> {
        self.plugins
            .iter()
            // work beside the tick can't have made a tick slow
            .filter(|p| !p.off_tick)
            .flat_map(|p| p.spikes.iter().map(move |s| (p.name.as_str(), *s)))
            .filter(|(_, s)| s.unix_secs.abs_diff(unix_secs) <= within_secs)
            .max_by_key(|(_, s)| s.duration_us)
    }
    /// Forgets every measurement (the list of plugins stays).
    pub fn reset(&mut self) {
        for plugin in &mut self.plugins {
            plugin.samples.clear();
            plugin.spikes.clear();
            plugin.total_spikes = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn report(memory: u64, work_us: u64, calls: u64, max_us: u64) -> Answer {
        Answer::Report(Report { perfmon: 1, memory_bytes: Some(memory), work_us: Some(work_us), calls: Some(calls), max_us: Some(max_us), ..Report::default() })
    }

    #[test]
    fn the_question_and_a_full_answer() {
        assert_eq!(request(), br#"{"perfmon":1,"ask":"report"}"#);
        let answer = br#"{"perfmon":1,"name":"cascade","version":"0.1.0","memory_bytes":4194304,"work_us":81234567,"calls":40211,"max_us":1840,
            "gauges":[{"name":"npcs","value":12},{"name":"sent","value":1.5,"unit":"MB"}],"something_new":true}"#;
        let r = parse_report(answer).unwrap();
        assert_eq!((r.name.as_deref(), r.version.as_deref()), (Some("cascade"), Some("0.1.0")));
        assert_eq!((r.memory_bytes, r.work_us, r.calls, r.max_us), (Some(4_194_304), Some(81_234_567), Some(40_211), Some(1840)));
        assert_eq!(r.gauges, [Gauge { name: "npcs".into(), value: 12.0, unit: None }, Gauge { name: "sent".into(), value: 1.5, unit: Some("MB".into()) }]);
        // the least a plugin can say
        assert_eq!(parse_report(br#"{"perfmon":1}"#).unwrap(), Report { perfmon: 1, ..Report::default() });
    }

    #[test]
    fn answers_that_are_not_reports_are_refused() {
        for bad in [&b""[..], b"pong", b"{}", br#"{"perfmon":2,"memory_bytes":1}"#, br#"{"perfmon":1,"memory_bytes":-5}"#, br#"{"perfmon":1,"work_us":"lots"}"#, b"[1,2,3]"] {
            assert!(parse_report(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        assert!(parse_report(&vec![b' '; 70_000]).is_err());
    }

    #[test]
    fn text_from_other_plugins_is_cleaned() {
        let gauges: String = (0..40).map(|i| format!(r#"{{"name":"g{i}","value":{i}}},"#)).collect();
        let answer = format!(r#"{{"perfmon":1,"version":"1.0§c\n\u0007","gauges":[{gauges}{{"name":"§4evil\n","value":1,"unit":"§kx"}},{{"name":"","value":1}}]}}"#);
        let r = parse_report(answer.as_bytes()).unwrap();
        assert_eq!(r.version.as_deref(), Some("1.0c"));
        assert_eq!(r.gauges.len(), 16, "no more than 16 gauges are kept");
        let odd = br#"{"perfmon":1,"gauges":[{"name":"a\nb","value":2,"unit":"way too long a unit"},{"name":"nan","value":1e999}]}"#;
        // a number too large to be one is refused as a whole
        assert!(parse_report(odd).is_err() || parse_report(odd).unwrap().gauges.iter().all(|g| g.value.is_finite()));
        let r = parse_report(br#"{"perfmon":1,"gauges":[{"name":"a\nb","value":2,"unit":"way too long a unit"}]}"#).unwrap();
        assert_eq!(r.gauges, [Gauge { name: "ab".into(), value: 2.0, unit: None }]);
    }

    #[test]
    fn introductions_name_a_plugin() {
        assert_eq!(parse_hello(br#"{"perfmon":1,"hello":"cascade"}"#), Some("cascade".into()));
        for bad in [&br#"{"perfmon":1,"hello":"Not A Name"}"#[..], br#"{"perfmon":2,"hello":"cascade"}"#, br#"{"perfmon":1,"ask":"report"}"#, br#"{"perfmon":1,"hello":""}"#, b"hello"] {
            assert_eq!(parse_hello(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        assert!(valid_name("multi-version_2") && !valid_name("a b") && !valid_name("") && !valid_name(&"a".repeat(65)));
        assert_eq!(welcome(), br#"{"perfmon":1,"ok":true}"#);
    }

    #[test]
    fn the_registry_keeps_one_of_each_name() {
        let mut registry = Registry::default();
        assert!(registry.add("cascade") && registry.add("multiversion"));
        assert!(!registry.add("cascade"), "already watched");
        assert!(!registry.add("perfmon"), "perfmon doesn't watch itself");
        assert!(!registry.add("Bad Name"));
        assert_eq!(registry.names(), ["cascade", "multiversion"]);
        for i in 0..100 {
            registry.add(&format!("p{i}"));
        }
        assert_eq!(registry.all().len(), MAX_WATCHED);
    }

    #[test]
    fn work_per_tick_comes_from_the_counter() {
        let limits = Limits::default();
        let mut p = Watched::new("x");
        assert_eq!(p.work_ms_per_tick(60), None);
        p.record(100, report(MIB, 1_000_000, 500, 0), &limits);
        assert_eq!(p.work_ms_per_tick(60), None, "one report is not enough");
        // 10 seconds later it has worked 400 ms more: 200 ticks, 2 ms each
        p.record(110, report(MIB, 1_400_000, 900, 0), &limits);
        assert_eq!(p.work_ms_per_tick(60), Some(2.0));
        assert_eq!(p.calls_per_sec(60), Some(40.0));
        assert_eq!(p.watched_secs(), 10);
        assert!(p.findings(110, &limits).is_empty(), "2 ms a tick is under the 5 ms limit");
        // then it gets busy: 6 ms a tick over the last minute
        for i in 1..=60 {
            p.record(110 + i, report(MIB, 1_400_000 + i * 120_000, 900 + i, 0), &limits);
        }
        let found = p.findings(170, &limits);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].kind, found[0].severe), (Kind::Lag, false));
        assert!(found[0].text.contains("6.0 ms") && found[0].text.contains("12%"), "{}", found[0].text);
        // only the last minute counts
        assert_eq!(p.work_ms_per_tick(60), Some(6.0));
        // twice the limit is severe
        for i in 1..=60 {
            p.record(170 + i, report(MIB, 8_600_000 + i * 300_000, 2000, 0), &limits);
        }
        assert!(p.findings(230, &limits)[0].severe);
    }

    #[test]
    fn steady_memory_growth_is_a_suspected_leak() {
        let limits = Limits::default();
        // grows 64 KiB a second: 37.5 MiB in ten minutes
        let mut leaky = Watched::new("leaky");
        for t in 0..=600u64 {
            leaky.record(1000 + t, report(16 * MIB + t * 65_536, t, t, 0), &limits);
        }
        let growth = leaky.memory_growth(LEAK_WINDOW_SECS).unwrap();
        assert!(growth.steady);
        assert_eq!((growth.secs, growth.bytes()), (600, 600 * 65_536));
        let found = leaky.findings(1600, &limits);
        assert_eq!(found.iter().map(|f| f.kind).collect::<Vec<_>>(), [Kind::Leak]);
        assert!(found[0].severe && found[0].text.contains("leak"), "{}", found[0].text);

        // the same total, all at once at the start (a world loading): not a leak
        let mut loader = Watched::new("loader");
        for t in 0..=600u64 {
            loader.record(1000 + t, report(if t < 20 { 16 * MIB } else { 54 * MIB }, t, t, 0), &limits);
        }
        assert!(!loader.memory_growth(LEAK_WINDOW_SECS).unwrap().steady);
        assert!(loader.findings(1600, &limits).is_empty());

        // steady but small: under the 8 MiB limit
        let mut slow = Watched::new("slow");
        for t in 0..=600u64 {
            slow.record(1000 + t, report(16 * MIB + t * 4096, t, t, 0), &limits);
        }
        assert!(slow.memory_growth(LEAK_WINDOW_SECS).unwrap().steady);
        assert!(slow.findings(1600, &limits).is_empty());

        // fast, but perfmon has only watched for two minutes: too early to say
        let mut new = Watched::new("new");
        for t in 0..=120u64 {
            new.record(1000 + t, report(16 * MIB + t * MIB, t, t, 0), &limits);
        }
        assert!(new.findings(1120, &limits).is_empty());
    }

    #[test]
    fn long_runs_are_spikes() {
        let limits = Limits::default();
        let mut p = Watched::new("x");
        assert_eq!(p.record(100, report(MIB, 10, 1, 24_999), &limits), None, "under 25 ms");
        let spike = p.record(101, report(MIB, 20, 2, 80_000), &limits).unwrap();
        assert_eq!(spike, PluginSpike { unix_secs: 101, duration_us: 80_000 });
        p.record(102, report(MIB, 30, 3, 30_000), &limits);
        assert_eq!(p.total_spikes, 2);
        assert_eq!(p.worst_spike(110, 60).unwrap().duration_us, 80_000);
        assert_eq!(p.recent_spikes(10).iter().map(|s| s.duration_us).collect::<Vec<_>>(), [30_000, 80_000]);
        let found = p.findings(110, &limits);
        assert_eq!((found[0].kind, found[0].severe), (Kind::Spike, true));
        assert!(found[0].text.contains("80.0 ms") && found[0].text.contains("9s ago"), "{}", found[0].text);
        // old spikes drop out of the findings but stay in the list
        assert!(p.findings(400, &limits).is_empty());
        assert_eq!(p.recent_spikes(10).len(), 2);
        for i in 0..100 {
            p.record(500 + i, report(MIB, 100 + i, 10 + i, 26_000), &limits);
        }
        assert_eq!(p.recent_spikes(100).len(), SPIKES_KEPT);
    }

    #[test]
    fn the_likeliest_cause_of_a_slow_tick_is_the_longest_run_near_it() {
        let limits = Limits::default();
        let mut registry = Registry::default();
        registry.add("a");
        registry.add("b");
        registry.get_mut("a").unwrap().record(100, report(MIB, 1, 1, 40_000), &limits);
        registry.get_mut("b").unwrap().record(101, report(MIB, 1, 1, 90_000), &limits);
        registry.get_mut("b").unwrap().record(300, report(MIB, 2, 2, 500_000), &limits);
        assert_eq!(registry.suspect(100, 2).map(|(name, s)| (name, s.duration_us)), Some(("b", 90_000)));
        assert_eq!(registry.suspect(100, 0).map(|(name, _)| name), Some("a"));
        assert_eq!(registry.suspect(200, 2), None);
        registry.reset();
        assert_eq!(registry.suspect(100, 2), None);
        assert_eq!(registry.names(), ["a", "b"]);
    }

    #[test]
    fn work_beside_the_tick_is_cpu_not_lag() {
        let limits = Limits::default();
        let mut registry = Registry::default();
        registry.add("proxy");
        registry.add("mob");
        let busy = |work_us: u64, max_us: u64| Answer::Report(Report { perfmon: 1, work_us: Some(work_us), calls: Some(1), max_us: Some(max_us), off_tick: true, ..Report::default() });
        let proxy = registry.get_mut("proxy").unwrap();
        proxy.record(100, busy(0, 0), &limits);
        proxy.record(110, busy(2_000_000, 90_000), &limits);
        assert!(proxy.off_tick);
        let found = proxy.findings(110, &limits);
        assert_eq!(found.iter().map(|f| f.kind).collect::<Vec<_>>(), [Kind::Lag, Kind::Spike]);
        assert!(found[0].text.contains("10.0 ms") && found[0].text.contains("20% of one CPU core") && found[0].text.contains("does not make ticks longer"), "{}", found[0].text);
        assert!(found[1].text.contains("beside the server tick") && !found[1].text.contains("whole tick"), "{}", found[1].text);
        // a slow tick at that moment is not put down to it, but to a plugin that runs in the tick
        assert_eq!(registry.suspect(110, 2), None);
        registry.get_mut("mob").unwrap().record(110, report(MIB, 1, 1, 30_000), &limits);
        assert_eq!(registry.suspect(110, 2).map(|(name, _)| name), Some("mob"));
        assert_eq!(parse_report(br#"{"perfmon":1,"off_tick":true}"#).unwrap().off_tick, true);
        assert!(!parse_report(br#"{"perfmon":1}"#).unwrap().off_tick);
    }

    #[test]
    fn a_reloaded_or_missing_plugin_starts_over() {
        let limits = Limits::default();
        let mut p = Watched::new("x");
        assert_eq!(p.state, State::Unknown);
        p.record(100, report(MIB, 5_000_000, 100, 0), &limits);
        p.record(110, report(MIB, 6_000_000, 200, 0), &limits);
        assert_eq!(p.state, State::Reporting);
        // its counters start from zero again: the old figures say nothing about the new run
        p.record(120, report(MIB, 1000, 1, 0), &limits);
        assert_eq!(p.work_ms_per_tick(60), None);
        p.record(130, report(MIB, 201_000, 2, 0), &limits);
        assert_eq!(p.work_ms_per_tick(60), Some(1.0));
        p.record(140, Answer::Silent, &limits);
        assert_eq!((p.state, p.memory_bytes(), p.work_ms_per_tick(60)), (State::Silent, None, None));
        p.record(150, Answer::NotLoaded, &limits);
        assert_eq!(p.state, State::NotLoaded);
        assert!(p.findings(150, &limits).is_empty());
        // history is capped
        for t in 0..2000 {
            p.record(1000 + t, report(MIB, t, t, 0), &limits);
        }
        assert_eq!(p.watched_secs(), HISTORY as u64 - 1);
        // a report with no numbers at all is still a plugin that reports
        let mut bare = Watched::new("bare");
        bare.record(1, Answer::Report(Report { perfmon: 1, ..Report::default() }), &limits);
        bare.record(2, Answer::Report(Report { perfmon: 1, ..Report::default() }), &limits);
        assert_eq!((bare.state, bare.memory_bytes(), bare.work_ms_per_tick(60), bare.memory_growth(60)), (State::Reporting, None, None, None));
    }
}
