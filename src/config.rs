//! `plugins/data/perfmon/config.toml`, written with defaults on first start.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// A tick that takes at least this long counts as a lag spike.
    pub spike_threshold_ms: u64,
    /// Log a warning to the console when a spike happens.
    pub warn_on_spike: bool,
    /// At most one spike warning per this many seconds.
    pub spike_warn_cooldown_secs: u64,
    /// Append a line to metrics.csv this often. 0 turns the CSV log off.
    pub csv_interval_secs: u64,
    /// Count entities for each CSV line (walks every world's entity list).
    pub csv_count_entities: bool,
    /// Colour console output (ANSI codes, like Pumpkin's own output).
    pub console_colors: bool,
    /// Ask the Pumpkin Market for the latest perfmon version when the server starts.
    pub update_check: bool,
    /// Tell operators in game, when they join, that a newer version is out.
    pub update_notify_ops: bool,
    /// Download a newer, Market-signed Perfmon.wasm into the data folder at startup, ready to be copied into plugins/.
    pub update_download: bool,
    /// Plugins to ask for a report, by name. Plugins that introduce themselves to perfmon are
    /// watched without being listed.
    pub plugins: Vec<String>,
    /// Ask watched plugins for their report this often. 0 turns plugin watching off.
    pub plugin_poll_secs: u64,
    /// Speak up when a plugin uses at least this many milliseconds of every tick, on average over a minute.
    pub plugin_lag_ms: f64,
    /// A single run of a plugin's code this long counts as a plugin spike.
    pub plugin_spike_ms: u64,
    /// Speak up when a plugin's memory has grown steadily by at least this many MiB over ten minutes.
    pub plugin_leak_mb: u64,
    /// Log a warning to the console when a plugin lags, spikes or looks like it leaks.
    pub warn_on_plugin_problem: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { spike_threshold_ms: 100, warn_on_spike: true, spike_warn_cooldown_secs: 10, csv_interval_secs: 60, csv_count_entities: true, console_colors: true, update_check: true, update_notify_ops: true, update_download: false, plugins: Vec::new(), plugin_poll_secs: 1, plugin_lag_ms: 5.0, plugin_spike_ms: 25, plugin_leak_mb: 8, warn_on_plugin_problem: true }
    }
}

const HEADER: &str = "# perfmon settings. Restart the server after editing.\n\n";

impl Config {
    pub fn load_or_create(path: &str) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{path}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                let body = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
                std::fs::write(path, format!("{HEADER}{body}")).map_err(|e| format!("{path}: {e}"))?;
                Ok(cfg)
            }
            Err(e) => Err(format!("{path}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn defaults_round_trip() {
        let text = toml::to_string_pretty(&super::Config::default()).unwrap();
        let back: super::Config = toml::from_str(&text).unwrap();
        assert_eq!(back.spike_threshold_ms, 100);
        let partial: super::Config = toml::from_str("spike_threshold_ms = 250").unwrap();
        assert_eq!(partial.spike_threshold_ms, 250);
        assert_eq!(partial.csv_interval_secs, 60);
        // A config written by an older version has no update settings: the defaults apply.
        assert!(partial.update_check && partial.update_notify_ops && !partial.update_download);
        // ... and no plugin settings either
        assert!(partial.plugins.is_empty() && partial.plugin_poll_secs == 1 && partial.warn_on_plugin_problem);
        assert_eq!((partial.plugin_lag_ms, partial.plugin_spike_ms, partial.plugin_leak_mb), (5.0, 25, 8));
        let listed: super::Config = toml::from_str("plugins = [\"cascade\", \"multiversion\"]\nplugin_poll_secs = 0").unwrap();
        assert_eq!((listed.plugins.len(), listed.plugin_poll_secs), (2, 0));
    }
}
