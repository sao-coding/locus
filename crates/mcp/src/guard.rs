//! HTTP checks in front of the MCP endpoint: path, `Host`, `Origin` (DNS
//! rebinding protection, as the MCP spec asks of local servers), bearer
//! token and declared body size. Pure: headers in, a status out.

use hyper::StatusCode;
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, HOST, HeaderMap, ORIGIN};

/// Largest request body accepted (also enforced while streaming by rmcp).
pub const MAX_BODY: usize = 1 << 20;

/// The endpoint path.
pub const PATH: &str = "/mcp";

/// Why a request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reject {
    pub status: StatusCode,
    pub message: &'static str,
}

const fn reject(status: StatusCode, message: &'static str) -> Reject {
    Reject { status, message }
}

/// Checks one request. `token` is the current bearer token.
pub fn check(path: &str, headers: &HeaderMap, port: u16, token: &str) -> Result<(), Reject> {
    if path != PATH {
        return Err(reject(
            StatusCode::NOT_FOUND,
            "not found; the endpoint is /mcp",
        ));
    }
    let host = headers.get(HOST).and_then(|v| v.to_str().ok());
    if !host.is_some_and(|h| host_ok(h, port)) {
        return Err(reject(
            StatusCode::FORBIDDEN,
            "Host must be 127.0.0.1 or localhost",
        ));
    }
    if let Some(o) = headers.get(ORIGIN)
        && !o.to_str().is_ok_and(origin_ok)
    {
        return Err(reject(StatusCode::FORBIDDEN, "Origin not allowed"));
    }
    let auth = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    let given = auth.and_then(|a| a.strip_prefix("Bearer ")).map(str::trim);
    if !given.is_some_and(|g| !token.is_empty() && same(g.as_bytes(), token.as_bytes())) {
        return Err(reject(
            StatusCode::UNAUTHORIZED,
            "missing or wrong bearer token",
        ));
    }
    let len = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if len.is_some_and(|n| n > MAX_BODY as u64) {
        return Err(reject(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }
    Ok(())
}

/// `127.0.0.1:<port>`, `localhost:<port>` or `[::1]:<port>` (case-insensitive).
fn host_ok(h: &str, port: u16) -> bool {
    let h = h.trim().to_ascii_lowercase();
    let Some((name, p)) = h.rsplit_once(':') else {
        return false;
    };
    p.parse::<u16>().ok() == Some(port) && matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

/// `http(s)://localhost`, `127.0.0.1` or `[::1]`, any port. Anything else,
/// including `null`, is refused.
fn origin_ok(o: &str) -> bool {
    let o = o.trim().to_ascii_lowercase();
    let Some(rest) = o
        .strip_prefix("http://")
        .or_else(|| o.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.contains(['/', '@', '?', '#']) {
        return false;
    }
    let name = match authority.rsplit_once(':') {
        Some((n, p)) if !n.ends_with(':') && p.parse::<u16>().is_ok() => n,
        // `[::1]` has colons inside the brackets.
        _ => authority,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// Constant-time comparison (no early exit on the first different byte).
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A new token: 32 random bytes, base64url without padding (43 chars).
pub fn new_token() -> Result<String, String> {
    use base64::Engine as _;
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| format!("no random bytes: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    const TOKEN: &str = "s3cret-token";

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        m
    }

    fn ok_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("host", "127.0.0.1:7781"),
            ("authorization", "Bearer s3cret-token"),
        ]
    }

    fn status(path: &str, pairs: &[(&'static str, &str)]) -> Option<StatusCode> {
        check(path, &h(pairs), 7781, TOKEN).err().map(|r| r.status)
    }

    #[test]
    fn a_good_request_passes() {
        assert_eq!(status("/mcp", &ok_headers()), None);
        let mut v = ok_headers();
        v[0] = ("host", "LOCALHOST:7781");
        v.push(("origin", "http://localhost:6274"));
        assert_eq!(status("/mcp", &v), None);
        v[0] = ("host", "[::1]:7781");
        assert_eq!(status("/mcp", &v), None);
    }

    #[test]
    fn wrong_path_is_404() {
        assert_eq!(status("/", &ok_headers()), Some(StatusCode::NOT_FOUND));
        assert_eq!(status("/mcp/x", &ok_headers()), Some(StatusCode::NOT_FOUND));
    }

    #[test]
    fn token_rejections_are_401() {
        let host = ("host", "127.0.0.1:7781");
        for auth in [
            None,
            Some("Bearer wrong"),
            Some("Bearer s3cret-token-longer"),
            Some("Bearer s3cret-toke"),
            Some("Basic czNjcmV0LXRva2Vu"),
            Some("s3cret-token"),
            Some("Bearer "),
        ] {
            let mut v = vec![host];
            if let Some(a) = auth {
                v.push(("authorization", a));
            }
            assert_eq!(
                status("/mcp", &v),
                Some(StatusCode::UNAUTHORIZED),
                "{auth:?}"
            );
        }
        // An empty configured token never matches.
        let v = h(&[("host", "127.0.0.1:7781"), ("authorization", "Bearer ")]);
        assert!(check("/mcp", &v, 7781, "").is_err());
    }

    #[test]
    fn origin_rejections_are_403() {
        for origin in [
            "http://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.nip.io:7781",
            "https://attacker.test:7781",
            "null",
            "file://",
            "http://user@localhost",
            "http://localhost/path",
            "localhost",
        ] {
            let mut v = ok_headers();
            v.push(("origin", origin));
            assert_eq!(status("/mcp", &v), Some(StatusCode::FORBIDDEN), "{origin}");
        }
        for origin in [
            "http://localhost",
            "http://127.0.0.1:7781",
            "https://localhost:443",
            "http://[::1]:3000",
        ] {
            let mut v = ok_headers();
            v.push(("origin", origin));
            assert_eq!(status("/mcp", &v), None, "{origin}");
        }
    }

    #[test]
    fn host_rejections_are_403() {
        for host in [
            None,
            Some("evil.example:7781"),
            Some("127.0.0.1"),
            Some("127.0.0.1:7782"),
            Some("0.0.0.0:7781"),
            Some("192.168.1.5:7781"),
            Some("localhost.evil.example:7781"),
        ] {
            let mut v = vec![("authorization", "Bearer s3cret-token")];
            if let Some(x) = host {
                v.push(("host", x));
            }
            assert_eq!(status("/mcp", &v), Some(StatusCode::FORBIDDEN), "{host:?}");
        }
    }

    #[test]
    fn origin_and_host_are_checked_before_the_token() {
        // A rebinding page never learns whether its guess of the token was right.
        let v = [
            ("host", "evil.example:7781"),
            ("authorization", "Bearer s3cret-token"),
        ];
        assert_eq!(status("/mcp", &v), Some(StatusCode::FORBIDDEN));
    }

    #[test]
    fn declared_body_size_is_limited() {
        let mut v = ok_headers();
        let big = (MAX_BODY + 1).to_string();
        v.push(("content-length", big.as_str()));
        assert_eq!(status("/mcp", &v), Some(StatusCode::PAYLOAD_TOO_LARGE));
        let mut v = ok_headers();
        v.push(("content-length", "1024"));
        assert_eq!(status("/mcp", &v), None);
    }

    #[test]
    fn tokens_are_random_base64url() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }
}
