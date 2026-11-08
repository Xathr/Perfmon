//! The startup banner printed to the console when the plugin loads.

/// The perfmon logo, three rows tall. Each row is split after "perf" so the halves can be
/// coloured differently.
const LOGO: [(&str, &str); 3] = [
    ("┌─┐┌─┐┬─┐┌─┐", "┌┬┐┌─┐┌┐┌"),
    ("├─┘├┤ ├┬┘├┤ ", "││││ ││││"),
    ("┴  └─┘┴└─└  ", "┴ ┴└─┘┘└┘"),
];

/// A tick-time graph drawn next to the logo: mostly quick ticks with one spike.
const GRAPH: [(&str, &str); 10] = [
    ("▂", "92"), ("▃", "92"), ("▂", "92"), ("▅", "93"), ("▃", "92"),
    ("▂", "92"), ("▇", "91"), ("▂", "92"), ("▃", "92"), ("▂", "92"),
];

/// What the banner reports about the current settings.
pub struct Info<'a> {
    pub version: &'a str,
    pub spike_threshold_ms: u64,
    pub warn_on_spike: bool,
    pub csv_interval_secs: u64,
}

fn paint(colors: bool, code: &str, s: &str) -> String {
    if colors { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
}

/// The banner, one console line per entry.
pub fn lines(info: &Info, colors: bool) -> Vec<String> {
    let p = |code: &str, s: &str| paint(colors, code, s);
    let graph: String = GRAPH.iter().map(|(bar, code)| p(code, bar)).collect();
    let beside = [
        graph,
        format!("{} {}", p("1;97", "perfmon"), p("90", &format!("v{}", info.version))),
        p("90", "tick, lag and memory monitor"),
    ];
    let mut out: Vec<String> = LOGO
        .iter()
        .zip(beside)
        .map(|((perf, mon), side)| format!("  {}{}   {side}", p("95", perf), p("35", mon)))
        .collect();

    let threshold = format!("{} ms", info.spike_threshold_ms);
    let warnings = if info.warn_on_spike { p("92", "on") } else { p("93", "off") };
    let csv = if info.csv_interval_secs > 0 {
        format!("every {} s {}", info.csv_interval_secs, p("90", "(plugins/data/perfmon/metrics.csv)"))
    } else {
        p("93", "off")
    };
    let commands = [p("96", "/perf"), p("96", "/tps"), p("96", "/lag")].join("  ");
    for (key, value) in [
        ("spike threshold", format!("{} {}", p("97", &threshold), p("90", "(warnings ") + &warnings + &p("90", ")"))),
        ("metrics log", csv),
        ("commands", commands),
    ] {
        out.push(format!("  {} {}", p("90", &format!("{key:<16}")), value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info<'static> {
        Info { version: "1.2.3", spike_threshold_ms: 100, warn_on_spike: true, csv_interval_secs: 60 }
    }

    #[test]
    fn logo_rows_line_up() {
        let widths: Vec<usize> = LOGO.iter().map(|(a, b)| a.chars().count() + b.chars().count()).collect();
        assert!(widths.iter().all(|w| *w == widths[0]), "{widths:?}");
    }

    #[test]
    fn plain_banner_has_no_colour_codes() {
        let lines = lines(&info(), false);
        assert!(lines.iter().all(|l| !l.contains('\x1b')));
        assert!(lines[1].ends_with("perfmon v1.2.3"));
        assert!(lines.iter().any(|l| l.contains("100 ms (warnings on)")));
        assert!(lines.iter().any(|l| l.contains("every 60 s")));
    }

    #[test]
    fn coloured_banner_resets_every_colour() {
        for line in lines(&info(), true) {
            assert_eq!(line.matches("\x1b[0m").count(), line.matches("\x1b[").count() / 2, "{line:?}");
        }
    }

    #[test]
    fn csv_off_is_shown() {
        let lines = lines(&Info { csv_interval_secs: 0, ..info() }, false);
        assert!(lines.iter().any(|l| l.contains("metrics log") && l.ends_with("off")));
    }
}
