//! Clickable chat messages, built as JSON text components (Pumpkin's format, converted by the server
//! for each client version). Pure code, unit-tested natively.

use serde_json::{Value, json};

/// One piece of a chat line.
#[derive(Clone, Debug)]
pub struct Part {
    text: String,
    color: &'static str,
    bold: bool,
    click: Option<(&'static str, String)>,
    hover: Option<String>,
}

pub fn text(t: impl Into<String>, color: &'static str) -> Part {
    Part { text: t.into(), color, bold: false, click: None, hover: None }
}

impl Part {
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    /// Runs `command` when clicked.
    pub fn run(mut self, command: impl Into<String>) -> Self {
        self.click = Some(("run_command", command.into()));
        self
    }
    /// Puts `command` in the chat box when clicked, for the player to confirm.
    pub fn suggest(mut self, command: impl Into<String>) -> Self {
        self.click = Some(("suggest_command", command.into()));
        self
    }
    /// Copies `value` to the clipboard when clicked.
    pub fn copy(mut self, value: impl Into<String>) -> Self {
        self.click = Some(("copy_to_clipboard", value.into()));
        self
    }
    pub fn hover(mut self, t: impl Into<String>) -> Self {
        self.hover = Some(t.into());
        self
    }

    fn to_json(&self) -> Value {
        let mut v = json!({ "text": self.text, "color": self.color });
        if self.bold {
            v["bold"] = json!(true);
        }
        if let Some((action, value)) = &self.click {
            v["click_event"] = match *action {
                "copy_to_clipboard" => json!({ "action": action, "value": value }),
                _ => json!({ "action": action, "command": value }),
            };
        }
        if let Some(h) = &self.hover {
            v["hover_event"] = json!({ "action": "show_text", "value": [{ "text": h, "color": "gray" }] });
        }
        v
    }
}

/// A whole chat line as a JSON text component.
pub fn line(parts: &[Part]) -> String {
    json!({ "text": "", "extra": parts.iter().map(Part::to_json).collect::<Vec<_>>() }).to_string()
}

/// The same line as plain text, for the console.
pub fn plain(parts: &[Part]) -> String {
    parts.iter().map(|p| p.text.as_str()).collect()
}

/// A clickable `[label]` button that runs a command.
pub fn button(label: &str, command: &str, hover: &str) -> Part {
    text(format!("[{label}]"), "aqua").run(command).hover(format!("{hover}\nClick to run {command}"))
}

/// Colour name for a TPS value: green, yellow, red.
pub fn tps_color(tps: f64) -> &'static str {
    if tps >= 19.5 { "green" } else if tps >= 15.0 { "yellow" } else { "red" }
}

/// Colour name for a tick duration in ms.
pub fn mspt_color(ms: f64) -> &'static str {
    if ms <= 40.0 { "green" } else if ms <= 50.0 { "yellow" } else { "red" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_click_and_hover() {
        let l = line(&[text("TPS ", "gray"), button("Entities", "/perf entities", "Entity counts")]);
        let v: Value = serde_json::from_str(&l).unwrap();
        let b = &v["extra"][1];
        assert_eq!(b["text"], "[Entities]");
        assert_eq!(b["click_event"]["action"], "run_command");
        assert_eq!(b["click_event"]["command"], "/perf entities");
        assert_eq!(b["hover_event"]["action"], "show_text");
        assert!(b["hover_event"]["value"][0]["text"].as_str().unwrap().contains("Click to run"));
    }

    #[test]
    fn copy_uses_value_and_suggest_uses_command() {
        let v: Value = serde_json::from_str(&line(&[text("a", "white").copy("x"), text("b", "white").suggest("/perf reset")])).unwrap();
        assert_eq!(v["extra"][0]["click_event"], json!({"action": "copy_to_clipboard", "value": "x"}));
        assert_eq!(v["extra"][1]["click_event"], json!({"action": "suggest_command", "command": "/perf reset"}));
    }

    #[test]
    fn escapes_and_plain() {
        let parts = [text("quote \" and \\ slash", "white").bold()];
        let v: Value = serde_json::from_str(&line(&parts)).unwrap();
        assert_eq!(v["extra"][0]["text"], "quote \" and \\ slash");
        assert_eq!(plain(&parts), "quote \" and \\ slash");
    }
}

/// ANSI code for a chat colour name.
fn ansi_code(color: &str) -> &'static str {
    match color {
        "black" => "30",
        "dark_blue" => "34",
        "dark_green" => "32",
        "dark_aqua" => "36",
        "dark_red" => "31",
        "dark_purple" => "35",
        "gold" => "33",
        "gray" => "37",
        "dark_gray" => "90",
        "blue" => "94",
        "green" => "92",
        "aqua" => "96",
        "red" => "91",
        "light_purple" => "95",
        "yellow" => "93",
        _ => "97",
    }
}

/// The line as ANSI-coloured text for the server console.
pub fn ansi(parts: &[Part]) -> String {
    let mut out = String::new();
    for p in parts {
        let bold = if p.bold { "1;" } else { "" };
        out.push_str(&format!("\x1b[{bold}{}m{}\x1b[0m", ansi_code(p.color), p.text));
    }
    out
}

/// Converts legacy `§` colour codes to ANSI for the server console.
pub fn legacy_to_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    let mut used = false;
    while let Some(c) = chars.next() {
        if c != '§' {
            out.push(c);
            continue;
        }
        let Some(code) = chars.next() else { break };
        let ansi = match code.to_ascii_lowercase() {
            '0' => "30", '1' => "34", '2' => "32", '3' => "36", '4' => "31", '5' => "35", '6' => "33",
            '7' => "37", '8' => "90", '9' => "94", 'a' => "92", 'b' => "96", 'c' => "91", 'd' => "95",
            'e' => "93", 'f' => "97", 'l' => "1", 'r' => "0",
            _ => continue,
        };
        out.push_str(&format!("\x1b[{ansi}m"));
        used = true;
    }
    if used {
        out.push_str("\x1b[0m");
    }
    out
}

#[cfg(test)]
mod ansi_tests {
    use super::*;
    #[test]
    fn converts() {
        assert_eq!(legacy_to_ansi("§7a §fb"), "\x1b[37ma \x1b[97mb\x1b[0m");
        assert_eq!(legacy_to_ansi("plain"), "plain");
        assert_eq!(ansi(&[text("x", "green").bold()]), "\x1b[1;92mx\x1b[0m");
    }
}
