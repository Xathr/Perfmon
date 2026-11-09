//! Update check against the Pumpkin Market, which signs the plugin files it hands out. Pure code
//! with no host calls, so it is unit-tested natively; the HTTP requests themselves are in `http.rs`.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// The Pumpkin Market.
pub const MARKET: &str = "https://market.pumpkinmc.org";
/// This plugin's name on the Market (it is case sensitive there).
pub const MARKET_NAME: &str = "Perfmon";
/// This plugin's page on the Market, for people to open.
pub const PAGE_URL: &str = "https://market.pumpkinmc.org/plugin/ID0WVaaw";
/// The signed `Perfmon.wasm` of the latest version.
pub const DOWNLOAD_URL: &str = "https://market.pumpkinmc.org/api/v1/rest/plugins/ID0WVaaw/download";
/// The key the Market signs plugin files with; Pumpkin checks plugins against the same one.
pub const PUBLIC_KEY_URL: &str = "https://market.pumpkinmc.org/api/v1/rest/public-key";
/// The file name of the plugin.
pub const ASSET: &str = "Perfmon.wasm";

const METADATA_SECTION: &str = "pumpkin.metadata";
const SIGNATURE_SECTIONS: [&str; 2] = ["wasm_signature", "pumpkin.signature"];

/// What the last update check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No check has run: it is turned off in the config.
    Off,
    /// The running version is the latest on the Market (or newer).
    Current { latest: String },
    /// A newer version is on the Market. `downloaded` is where its `Perfmon.wasm` was saved, if it was.
    Available { latest: String, downloaded: Option<String> },
    /// The check could not be completed.
    Failed(String),
}

/// The Market's "is there an update" request for the running version.
pub fn check_url(current: &str) -> String {
    let version: String = current.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    format!("{MARKET}/api/v1/rest/check-update?plugin_name={MARKET_NAME}&current_version={version}")
}

/// Parses `1.2.3` or `v1.2.3`. Anything after the patch number (`-beta`, `+build`) is ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || parts.next().map(str::parse::<u64>);
    let v = (next()?.ok()?, next().unwrap_or(Ok(0)).ok()?, next().unwrap_or(Ok(0)).ok()?);
    parts.next().is_none().then_some(v)
}

/// True when `latest` is a higher version than `current`. Unparseable versions are never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The latest version in the Market's reply to [`check_url`]. Its `update_available` flag is not
/// used: it is also set when the Market's version is older than the running one.
pub fn parse_check(json: &[u8]) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|_| match std::str::from_utf8(json) {
        // Errors come back as a short line of text, such as "Plugin not found on Pumpkin Market".
        Ok(text) if !text.trim().is_empty() && text.len() < 200 && !text.contains('<') => format!("the Market says: {}", text.trim()),
        _ => "unreadable reply from the Market".to_string(),
    })?;
    let latest = v["latest_version"].as_str().ok_or("the Market lists no version of perfmon")?;
    if parse_version(latest).is_none() {
        return Err(format!("the Market lists an unreadable version: {latest:?}"));
    }
    Ok(latest.to_string())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// The Market's Ed25519 public key from its reply to [`PUBLIC_KEY_URL`].
pub fn parse_public_key(body: &[u8]) -> Result<[u8; 32], String> {
    let text = std::str::from_utf8(body).map_err(|_| "unreadable Market key")?;
    let hex = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(v) => v["public_key_hex"].as_str().ok_or("the Market sent no signing key")?.to_string(),
        Err(_) => text.trim().to_string(),
    };
    unhex(&hex).and_then(|k| <[u8; 32]>::try_from(k).ok()).ok_or_else(|| "the Market's signing key is not 32 bytes of hex".to_string())
}

fn leb128(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0u64;
    for (i, b) in bytes.iter().take(5).enumerate() {
        value |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value as usize, i + 1));
        }
    }
    None
}

/// What the Market recorded in a plugin file it signed.
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub plugin_name: String,
    pub version: String,
}

/// Checks a plugin file the way Pumpkin does: the Market appends a `pumpkin.metadata` section and a
/// `wasm_signature` section, and signs everything before them plus the metadata. Returns what the
/// metadata says, or why the file cannot be trusted.
pub fn verify_signed(wasm: &[u8], market_key: &[u8; 32]) -> Result<Signed, String> {
    if wasm.len() < 8 || &wasm[..4] != b"\0asm" {
        return Err("the download is not a WebAssembly file".into());
    }
    // Walk the top-level sections: an id byte, a length, then that many bytes.
    let (mut pos, mut signed_end) = (8, None);
    let (mut metadata, mut signature) = (None, None);
    while pos < wasm.len() {
        let start = pos;
        let (len, n) = leb128(&wasm[pos + 1..]).ok_or("the download is damaged")?;
        let body_start = pos + 1 + n;
        pos = body_start.checked_add(len).filter(|end| *end <= wasm.len()).ok_or("the download is cut short")?;
        let body = &wasm[body_start..pos];
        let custom = (wasm[start] == 0).then(|| leb128(body)).flatten().and_then(|(name_len, n)| {
            let name = std::str::from_utf8(body.get(n..n + name_len)?).ok()?;
            Some((name, &body[n + name_len..]))
        });
        match custom {
            Some((METADATA_SECTION, data)) if metadata.is_none() && signature.is_none() => {
                signed_end = Some(start);
                metadata = Some(data);
            }
            Some((name, data)) if SIGNATURE_SECTIONS.contains(&name) && metadata.is_some() && signature.is_none() => signature = Some(data),
            // The Market's two sections are the last thing in the file; anything else there is not covered by the signature.
            _ if signed_end.is_some() => return Err("the download has data after its signature".into()),
            _ => {}
        }
    }
    let (Some(end), Some(metadata), Some(signature)) = (signed_end, metadata, signature) else {
        return Err("the download is not signed by the Market".into());
    };
    let envelope: serde_json::Value = serde_json::from_slice(signature).map_err(|_| "unreadable signature")?;
    let signature = envelope["signature_hex"].as_str().and_then(unhex).and_then(|s| Signature::from_slice(&s).ok()).ok_or("unreadable signature")?;
    let key = VerifyingKey::from_bytes(market_key).map_err(|_| "the Market's signing key is not valid")?;
    let mut message = wasm[..end].to_vec();
    message.extend_from_slice(metadata);
    key.verify(&message, &signature).map_err(|_| "the signature does not match the Market's key")?;

    let meta: serde_json::Value = serde_json::from_slice(metadata).map_err(|_| "unreadable Market metadata")?;
    match (meta["plugin_name"].as_str(), meta["version"].as_str()) {
        (Some(name), Some(version)) => Ok(Signed { plugin_name: name.to_string(), version: version.to_string() }),
        _ => Err("the Market metadata names no plugin or version".into()),
    }
}

/// Verifies a download and makes sure it is the perfmon version that was announced.
pub fn verify_download(wasm: &[u8], market_key: &[u8; 32], expect_version: &str) -> Result<(), String> {
    let signed = verify_signed(wasm, market_key)?;
    if signed.plugin_name != MARKET_NAME {
        return Err(format!("the download is {:?}, not {MARKET_NAME}", signed.plugin_name));
    }
    if parse_version(&signed.version) != parse_version(expect_version) {
        return Err(format!("the download is version {}, not {expect_version}", signed.version));
    }
    Ok(())
}

/// A URL split into the pieces an HTTP request needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub https: bool,
    /// Host, with the port if one was given.
    pub authority: String,
    /// Path and query, always starting with `/`.
    pub path: String,
}

pub fn parse_url(url: &str) -> Result<Url, String> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("not an http(s) URL: {url}"));
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace) {
        return Err(format!("bad host in URL: {url}"));
    }
    Ok(Url { https, authority: authority.to_string(), path })
}

/// Where a redirect from `from` with the given `Location` header leads. A redirect may not leave
/// HTTPS for plain HTTP.
pub fn redirect_target(from: &Url, location: &str) -> Result<String, String> {
    let location = location.trim();
    let scheme = if from.https { "https" } else { "http" };
    let target = if location.starts_with("https://") || location.starts_with("http://") {
        location.to_string()
    } else if let Some(rest) = location.strip_prefix("//") {
        format!("{scheme}://{rest}")
    } else if location.starts_with('/') {
        format!("{scheme}://{}{location}", from.authority)
    } else {
        return Err(format!("unsupported redirect to {location:?}"));
    };
    if from.https && !target.starts_with("https://") {
        return Err("refused a redirect from https to http".into());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.5"), Some((0, 2, 5)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("2.0.0-beta.1"), Some((2, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v0.2.10", "0.2.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.2.5", "0.2.5"));
        assert!(!is_newer("v0.2.4", "0.2.5"));
        assert!(!is_newer("nightly", "0.2.5"));
    }

    #[test]
    fn check_request_and_reply() {
        assert_eq!(check_url("0.3.1"), "https://market.pumpkinmc.org/api/v1/rest/check-update?plugin_name=Perfmon&current_version=0.3.1");
        assert!(check_url("1.0&x=y z").ends_with("current_version=1.0xyz"));
        // The Market reports an "update" to an older version; only the version itself is used.
        assert_eq!(parse_check(br#"{"update_available":true,"latest_version":"0.3.0","api_version":"0.1.0"}"#).unwrap(), "0.3.0");
        assert_eq!(parse_check(b"Plugin not found on Pumpkin Market").unwrap_err(), "the Market says: Plugin not found on Pumpkin Market");
        assert_eq!(parse_check(b"<html>busy</html>").unwrap_err(), "unreadable reply from the Market");
        assert!(parse_check(br#"{"update_available":false,"latest_version":null}"#).is_err());
        assert!(parse_check(br#"{"latest_version":"soon"}"#).is_err());
    }

    #[test]
    fn public_keys() {
        let hex = "714d50b42cc7c56dabf1fa88735974bc333fe1b5b692e9896222928e09710ff4";
        let key = parse_public_key(format!(r#"{{"algorithm":"Ed25519","public_key_hex":"{hex}"}}"#).as_bytes()).unwrap();
        assert_eq!((key[0], key[31]), (0x71, 0xf4));
        assert_eq!(parse_public_key(format!("\"{hex}\"").as_bytes()).unwrap(), key);
        assert_eq!(parse_public_key(format!("{hex}\n").as_bytes()).unwrap(), key);
        assert!(parse_public_key(b"{}").is_err());
        assert!(parse_public_key(b"abcd").is_err());
    }

    fn section(id: u8, body: &[u8]) -> Vec<u8> {
        assert!(body.len() < 128);
        [&[id, body.len() as u8][..], body].concat()
    }
    fn custom(name: &str, data: &[u8]) -> Vec<u8> {
        section(0, &[&[name.len() as u8][..], name.as_bytes(), data].concat())
    }
    /// A tiny module signed the way the Market signs plugins.
    fn signed(key: &SigningKey, meta: &str) -> Vec<u8> {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend(section(1, &[0]));
        wasm.extend(custom("name", b"x"));
        let signature = key.sign(&[&wasm[..], meta.as_bytes()].concat());
        let hex: String = signature.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        wasm.extend(custom("pumpkin.metadata", meta.as_bytes()));
        // Too long for a one-byte length, like the real section.
        let envelope = format!(r#"{{"version":1,"algorithm":"Ed25519","signature_hex":"{hex}"}}"#);
        let body = [&[14u8][..], b"wasm_signature", envelope.as_bytes()].concat();
        wasm.push(0);
        wasm.extend([(body.len() & 0x7f) as u8 | 0x80, (body.len() >> 7) as u8]);
        wasm.extend(body);
        wasm
    }
    const META: &str = r#"{"plugin_name":"Perfmon","version":"0.4.0","user_id":0}"#;

    #[test]
    fn accepts_a_market_signed_file() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = key.verifying_key().to_bytes();
        let wasm = signed(&key, META);
        assert_eq!(verify_signed(&wasm, &public).unwrap(), Signed { plugin_name: "Perfmon".into(), version: "0.4.0".into() });
        assert!(verify_download(&wasm, &public, "0.4.0").is_ok());
        assert!(verify_download(&wasm, &public, "v0.4").is_ok());
        assert!(verify_download(&wasm, &public, "0.4.1").unwrap_err().contains("version 0.4.0"));
        let other = signed(&key, r#"{"plugin_name":"Other","version":"0.4.0"}"#);
        assert!(verify_download(&other, &public, "0.4.0").unwrap_err().contains("not Perfmon"));
    }

    #[test]
    fn rejects_tampered_and_unsigned_files() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = key.verifying_key().to_bytes();
        let wasm = signed(&key, META);

        let mut changed_code = wasm.clone();
        changed_code[10] ^= 1;
        assert!(verify_signed(&changed_code, &public).unwrap_err().contains("does not match"));

        let mut changed_meta = wasm.clone();
        let at = wasm.windows(5).position(|w| w == b"0.4.0").unwrap();
        changed_meta[at] = b'9';
        assert!(verify_signed(&changed_meta, &public).unwrap_err().contains("does not match"));

        let other_key = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(verify_signed(&wasm, &other_key).unwrap_err().contains("does not match"));

        let mut appended = wasm.clone();
        appended.extend(custom("extra", b"code"));
        assert!(verify_signed(&appended, &public).unwrap_err().contains("after its signature"));

        let unsigned = [&b"\0asm\x01\0\0\0"[..], &section(1, &[0])].concat();
        assert!(verify_signed(&unsigned, &public).unwrap_err().contains("not signed"));
        assert!(verify_signed(&wasm[..wasm.len() - 3], &public).unwrap_err().contains("cut short"));
        assert!(verify_signed(b"<html>not wasm</html>", &public).unwrap_err().contains("not a WebAssembly"));
    }

    #[test]
    fn urls() {
        let u = parse_url("https://api.github.com/repos/a/b?x=1#frag").unwrap();
        assert_eq!(u, Url { https: true, authority: "api.github.com".into(), path: "/repos/a/b?x=1".into() });
        assert_eq!(parse_url("http://localhost:8080").unwrap().path, "/");
        assert_eq!(parse_url("https://h?q=1").unwrap().path, "/?q=1");
        assert!(parse_url("ftp://h/x").is_err());
        assert!(parse_url("https://user@h/x").is_err());
        assert!(parse_url("https:///x").is_err());
    }

    #[test]
    fn redirects() {
        let from = parse_url("https://market.pumpkinmc.org/a/b").unwrap();
        assert_eq!(redirect_target(&from, "https://assets.example/x").unwrap(), "https://assets.example/x");
        assert_eq!(redirect_target(&from, "/api/v1/rest/x").unwrap(), "https://market.pumpkinmc.org/api/v1/rest/x");
        assert_eq!(redirect_target(&from, "//cdn.example/x").unwrap(), "https://cdn.example/x");
        assert!(redirect_target(&from, "http://plain.example/x").is_err());
        assert!(redirect_target(&from, "relative/path").is_err());
    }
}
