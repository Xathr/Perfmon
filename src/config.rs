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
}

impl Default for Config {
    fn default() -> Self {
        Self { spike_threshold_ms: 100, warn_on_spike: true, spike_warn_cooldown_secs: 10, csv_interval_secs: 60, csv_count_entities: true }
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
    }
}
