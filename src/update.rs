//! Update check against the GitHub releases of this plugin. Pure code with no host calls, so it is
//! unit-tested natively; the HTTP requests themselves are in `http.rs`.

use sha2::{Digest, Sha256};

/// The GitHub repository releases are published to.
pub const REPO: &str = "Xathr/Perfmon";
/// GitHub's "latest release" API for [`REPO`].
pub const LATEST_URL: &str = "https://api.github.com/repos/Xathr/Perfmon/releases/latest";
/// The release file that is the plugin itself.
pub const ASSET: &str = "Perfmon.wasm";

/// The parts of a GitHub release the updater needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// The release tag, for example `v0.3.0`.
    pub tag: String,
    /// The release page, for people to open.
    pub page_url: String,
    /// Download URL of `Perfmon.wasm`, if the release has one.
    pub wasm_url: Option<String>,
    /// Download URL of `Perfmon.wasm.sha256`, if the release has one.
    pub sha256_url: Option<String>,
}

/// What the last update check found.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No check has run: it is turned off in the config.
    Off,
    /// The running version is the latest release (or newer).
    Current { latest: String },
    /// A newer release exists. `downloaded` is where its `Perfmon.wasm` was saved, if it was.
    Available { release: Release, downloaded: Option<String> },
    /// The check could not be completed.
    Failed(String),
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

/// Reads the reply of GitHub's "latest release" API.
pub fn parse_release(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("unreadable release data: {e}"))?;
    let Some(tag) = v["tag_name"].as_str() else {
        // An API error (rate limit, missing repository) is an object with a message.
        return Err(v["message"].as_str().map_or_else(|| "release data has no tag".into(), |m| format!("GitHub says: {m}")));
    };
    let asset = |name: &str| {
        v["assets"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["name"] == name))
            .and_then(|a| a["browser_download_url"].as_str())
            .filter(|u| u.starts_with("https://"))
            .map(str::to_string)
    };
    Ok(Release {
        tag: tag.to_string(),
        page_url: v["html_url"]
            .as_str()
            .filter(|u| u.starts_with("https://"))
            .map_or_else(|| format!("https://github.com/{REPO}/releases/latest"), str::to_string),
        wasm_url: asset(ASSET),
        sha256_url: asset(&format!("{ASSET}.sha256")),
    })
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

/// Lower-case hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// The hash in a `sha256sum` file (`<64 hex digits>  <file name>`).
pub fn parse_sha256_file(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?.to_ascii_lowercase();
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn release_with_assets() {
        let json = br#"{"tag_name":"v0.3.0","html_url":"https://github.com/Xathr/Perfmon/releases/tag/v0.3.0","assets":[
            {"name":"Perfmon.wasm","browser_download_url":"https://github.com/Xathr/Perfmon/releases/download/v0.3.0/Perfmon.wasm"},
            {"name":"Perfmon.wasm.sha256","browser_download_url":"https://github.com/Xathr/Perfmon/releases/download/v0.3.0/Perfmon.wasm.sha256"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.tag, "v0.3.0");
        assert!(r.page_url.ends_with("/tag/v0.3.0"));
        assert!(r.wasm_url.unwrap().ends_with("/Perfmon.wasm"));
        assert!(r.sha256_url.unwrap().ends_with("/Perfmon.wasm.sha256"));
    }

    #[test]
    fn release_without_assets_and_api_errors() {
        let r = parse_release(br#"{"tag_name":"v1.0.0","assets":[{"name":"Perfmon.wasm","browser_download_url":"http://plain.example/x"}]}"#).unwrap();
        assert_eq!(r.wasm_url, None);
        assert_eq!(r.page_url, "https://github.com/Xathr/Perfmon/releases/latest");
        assert_eq!(parse_release(br#"{"message":"API rate limit exceeded"}"#).unwrap_err(), "GitHub says: API rate limit exceeded");
        assert!(parse_release(b"<html>").is_err());
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
        let from = parse_url("https://github.com/a/b").unwrap();
        assert_eq!(redirect_target(&from, "https://objects.example/x").unwrap(), "https://objects.example/x");
        assert_eq!(redirect_target(&from, "/repositories/1/releases/latest").unwrap(), "https://github.com/repositories/1/releases/latest");
        assert_eq!(redirect_target(&from, "//cdn.example/x").unwrap(), "https://cdn.example/x");
        assert!(redirect_target(&from, "http://plain.example/x").is_err());
        assert!(redirect_target(&from, "relative/path").is_err());
    }

    #[test]
    fn checksums() {
        let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), hash);
        assert_eq!(parse_sha256_file(&format!("{}  Perfmon.wasm\n", hash.to_uppercase())).as_deref(), Some(hash));
        assert_eq!(parse_sha256_file("not a hash"), None);
        assert_eq!(parse_sha256_file(""), None);
    }
}
