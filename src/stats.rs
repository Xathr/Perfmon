//! Tick timing statistics. Pure code with no host calls, so it is unit-tested natively.

use std::collections::VecDeque;

/// Ticks kept in memory: 15 minutes at 20 TPS.
pub const WINDOW: usize = 20 * 60 * 15;
/// One server tick at full speed, in microseconds.
pub const TICK_BUDGET_US: u64 = 50_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spike {
    pub tick: i32,
    pub duration_us: u64,
    pub unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub ticks: usize,
    pub tps: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub over_budget: usize,
}

pub struct Stats {
    durations: VecDeque<u64>,
    spikes: VecDeque<Spike>,
    pub spike_threshold_us: u64,
    pub total_ticks: u64,
    pub total_spikes: u64,
    pub last_tick: i32,
}

impl Stats {
    pub fn new(spike_threshold_us: u64) -> Self {
        Self {
            durations: VecDeque::with_capacity(WINDOW),
            spikes: VecDeque::with_capacity(64),
            spike_threshold_us,
            total_ticks: 0,
            total_spikes: 0,
            last_tick: 0,
        }
    }

    /// Records one finished tick. Returns the spike if the tick was over the threshold.
    pub fn record(&mut self, tick: i32, duration_us: u64, unix_secs: u64) -> Option<Spike> {
        if self.durations.len() == WINDOW {
            self.durations.pop_front();
        }
        self.durations.push_back(duration_us);
        self.total_ticks += 1;
        self.last_tick = tick;
        if duration_us >= self.spike_threshold_us {
            let spike = Spike { tick, duration_us, unix_secs };
            if self.spikes.len() == 64 {
                self.spikes.pop_front();
            }
            self.spikes.push_back(spike);
            self.total_spikes += 1;
            return Some(spike);
        }
        None
    }

    pub fn reset(&mut self) {
        self.durations.clear();
        self.spikes.clear();
        self.total_spikes = 0;
    }

    /// Most recent spikes, newest first.
    pub fn recent_spikes(&self, n: usize) -> Vec<Spike> {
        self.spikes.iter().rev().take(n).copied().collect()
    }

    /// The last `n` tick durations in microseconds, oldest first.
    pub fn recent(&self, n: usize) -> Vec<u64> {
        let skip = self.durations.len().saturating_sub(n);
        self.durations.iter().skip(skip).copied().collect()
    }

    /// Statistics over the last `ticks` ticks (fewer if not that many were recorded).
    pub fn summary(&self, ticks: usize) -> Summary {
        let mut v = self.recent(ticks);
        if v.is_empty() {
            return Summary { ticks: 0, tps: 20.0, mean_ms: 0.0, p50_ms: 0.0, p95_ms: 0.0, p99_ms: 0.0, max_ms: 0.0, over_budget: 0 };
        }
        let n = v.len();
        let sum: u64 = v.iter().sum();
        // A tick never takes less than the 50 ms budget in wall time: the server sleeps the rest.
        let wall: u64 = v.iter().map(|d| (*d).max(TICK_BUDGET_US)).sum();
        let tps = (n as f64 * 1_000_000.0 / wall as f64).min(20.0);
        let over_budget = v.iter().filter(|d| **d > TICK_BUDGET_US).count();
        v.sort_unstable();
        let pct = |p: f64| -> f64 {
            let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
            v[idx.min(n - 1)] as f64 / 1000.0
        };
        Summary {
            ticks: n,
            tps,
            mean_ms: sum as f64 / n as f64 / 1000.0,
            p50_ms: pct(50.0),
            p95_ms: pct(95.0),
            p99_ms: pct(99.0),
            max_ms: v[n - 1] as f64 / 1000.0,
            over_budget,
        }
    }
}

/// Formats bytes as KiB/MiB/GiB.
pub fn human_bytes(b: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

/// Legacy colour code for a TPS value: green, yellow, red.
pub fn tps_colour(tps: f64) -> &'static str {
    if tps >= 19.5 { "§a" } else if tps >= 15.0 { "§e" } else { "§c" }
}

/// Legacy colour code for a tick duration in ms.
pub fn mspt_colour(ms: f64) -> &'static str {
    if ms <= 40.0 { "§a" } else if ms <= 50.0 { "§e" } else { "§c" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_ticks_are_20_tps() {
        let mut s = Stats::new(100_000);
        for t in 0..100 {
            assert!(s.record(t, 10_000, 0).is_none());
        }
        let m = s.summary(100);
        assert_eq!(m.ticks, 100);
        assert!((m.tps - 20.0).abs() < 1e-9);
        assert!((m.mean_ms - 10.0).abs() < 1e-9);
        assert_eq!(m.over_budget, 0);
    }

    #[test]
    fn slow_ticks_lower_tps_and_count_spikes() {
        let mut s = Stats::new(100_000);
        for t in 0..10 {
            s.record(t, 100_000, 7);
        }
        let m = s.summary(10);
        assert!((m.tps - 10.0).abs() < 1e-9);
        assert_eq!(m.over_budget, 10);
        assert_eq!(s.total_spikes, 10);
        assert_eq!(s.recent_spikes(3)[0].tick, 9);
    }

    #[test]
    fn percentiles_and_window() {
        let mut s = Stats::new(u64::MAX);
        for t in 0..(WINDOW as i32 + 50) {
            s.record(t, (t as u64 % 100) * 1000, 0);
        }
        assert_eq!(s.recent(usize::MAX).len(), WINDOW);
        let m = s.summary(100);
        assert!((m.max_ms - 99.0).abs() < 1e-9);
        assert!(m.p50_ms > 45.0 && m.p50_ms < 55.0);
        assert!(m.p99_ms >= 97.0);
    }

    #[test]
    fn empty_summary() {
        assert_eq!(Stats::new(1).summary(20).ticks, 0);
    }

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
