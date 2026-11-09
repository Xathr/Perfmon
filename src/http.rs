//! A small blocking HTTP GET over `wasi:http`, for the update check. Pumpkin only lets the request
//! out when the plugin has the `http.outbound` permission.

use crate::update::{Url, parse_url, redirect_target};
use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, Method, OutgoingRequest, RequestOptions, Scheme};
use wasi::io::streams::StreamError;

const USER_AGENT: &str = concat!("perfmon/", env!("CARGO_PKG_VERSION"), " (Pumpkin plugin)");
const MAX_REDIRECTS: usize = 5;
const CONNECT_TIMEOUT_NS: u64 = 4_000_000_000;
const FIRST_BYTE_TIMEOUT_NS: u64 = 6_000_000_000;
const BETWEEN_BYTES_TIMEOUT_NS: u64 = 6_000_000_000;

enum Reply {
    Body(Vec<u8>),
    Redirect(String),
}

/// Fetches `url`, following redirects, and returns the body. Fails on any status but 200 and on
/// a body longer than `max_bytes`.
pub fn get(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        match fetch(&parse_url(&url)?, max_bytes)? {
            Reply::Body(body) => return Ok(body),
            Reply::Redirect(next) => url = next,
        }
    }
    Err("too many redirects".into())
}

fn fetch(url: &Url, max_bytes: usize) -> Result<Reply, String> {
    let headers = Fields::new();
    for (name, value) in [("user-agent", USER_AGENT), ("accept", "application/vnd.github+json, */*")] {
        headers.append(name, value.as_bytes()).map_err(|e| format!("header {name}: {e:?}"))?;
    }
    let request = OutgoingRequest::new(headers);
    let bad = |_| "could not build the request".to_string();
    request.set_method(&Method::Get).map_err(bad)?;
    request.set_scheme(Some(if url.https { &Scheme::Https } else { &Scheme::Http })).map_err(bad)?;
    request.set_authority(Some(&url.authority)).map_err(bad)?;
    request.set_path_with_query(Some(&url.path)).map_err(bad)?;
    let options = RequestOptions::new();
    let _ = options.set_connect_timeout(Some(CONNECT_TIMEOUT_NS));
    let _ = options.set_first_byte_timeout(Some(FIRST_BYTE_TIMEOUT_NS));
    let _ = options.set_between_bytes_timeout(Some(BETWEEN_BYTES_TIMEOUT_NS));

    let denied = "the http.outbound permission is not granted";
    let explain = |e: wasi::http::types::ErrorCode| match e {
        wasi::http::types::ErrorCode::HttpRequestDenied => denied.to_string(),
        e => format!("{} ({e:?})", url.authority),
    };
    let pending = outgoing_handler::handle(request, Some(options)).map_err(explain)?;
    pending.subscribe().block();
    let response = pending
        .get()
        .ok_or("no response")?
        .map_err(|()| "response already read".to_string())?
        .map_err(explain)?;

    let status = response.status();
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        let location = response.headers().get("location").into_iter().next().ok_or(format!("HTTP {status} without a Location header"))?;
        return redirect_target(url, &String::from_utf8_lossy(&location)).map(Reply::Redirect);
    }
    let incoming = response.consume().map_err(|()| "no response body".to_string())?;
    let stream = incoming.stream().map_err(|()| "no response body".to_string())?;
    let mut body = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) => {
                body.extend_from_slice(&chunk);
                if body.len() > max_bytes {
                    return Err(format!("reply from {} is larger than {max_bytes} bytes", url.authority));
                }
            }
            Err(StreamError::Closed) => break,
            Err(StreamError::LastOperationFailed(e)) => return Err(format!("{}: {}", url.authority, e.to_debug_string())),
        }
    }
    if status != 200 {
        // GitHub explains API errors (rate limit, not found) in a small JSON body.
        let why = serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| v["message"].as_str().map(str::to_string));
        return Err(match why {
            Some(why) => format!("HTTP {status} from {}: {why}", url.authority),
            None => format!("HTTP {status} from {}", url.authority),
        });
    }
    Ok(Reply::Body(body))
}
