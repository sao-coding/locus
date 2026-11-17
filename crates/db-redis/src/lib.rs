//! Redis driver: one multiplexed connection per (saved connection, DB index),
//! raw commands as `Reply` trees, and the paged readers the key views use
//! (`SCAN`, `HSCAN`, `LRANGE`, `SSCAN`, `ZRANGE`, `XRANGE`: never `KEYS *`,
//! never a whole big key). Not a `db_core::Driver`: Redis is not SQL.
use redis::aio::MultiplexedConnection;

mod reply;
pub use reply::Reply;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub db: u8,
    pub tls: bool,
    /// TLS without certificate verification.
    pub insecure: bool,
}

impl Config {
    fn info(&self) -> redis::ConnectionInfo {
        let addr = if self.tls {
            redis::ConnectionAddr::TcpTls {
                host: self.host.clone(),
                port: self.port,
                insecure: self.insecure,
                tls_params: None,
            }
        } else {
            redis::ConnectionAddr::Tcp(self.host.clone(), self.port)
        };
        let mut ri = redis::RedisConnectionInfo::default().set_db(self.db as i64);
        if let Some(u) = &self.user {
            ri = ri.set_username(u.clone());
        }
        if let Some(p) = &self.password {
            ri = ri.set_password(p.clone());
        }
        use std::str::FromStr as _;
        redis::ConnectionInfo::from_str("redis://127.0.0.1")
            .expect("static url")
            .set_addr(addr)
            .set_redis_settings(ri)
    }
}

pub struct RedisDriver {
    conn: MultiplexedConnection,
    pub cfg: Config,
}

/// What `TYPE` + `PTTL` + the size command say about a key.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMeta {
    /// `string`, `hash`, `list`, `set`, `zset`, `stream`, or `none` (missing).
    pub ty: String,
    /// Milliseconds; `None` = no expiry.
    pub ttl_ms: Option<i64>,
    /// STRLEN / HLEN / LLEN / SCARD / ZCARD / XLEN.
    pub len: u64,
}

impl RedisDriver {
    pub async fn connect(cfg: Config) -> Result<Self, String> {
        let client = redis::Client::open(cfg.info()).map_err(|e| e.to_string())?;
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| "connect timeout".to_string())?
        .map_err(|e| e.to_string())?;
        Ok(Self { conn, cfg })
    }

    /// One raw command.
    pub async fn cmd<A: AsRef<[u8]>>(&self, args: &[A]) -> Result<Reply, String> {
        let name = std::str::from_utf8(args.first().ok_or("empty command")?.as_ref())
            .map_err(|e| e.to_string())?
            .to_string();
        let mut c = redis::cmd(&name);
        for a in &args[1..] {
            c.arg(a.as_ref());
        }
        let mut conn = self.conn.clone();
        let v: redis::Value = c.query_async(&mut conn).await.map_err(|e| e.to_string())?;
        Ok(Reply::from_value(v))
    }

    /// `PING` + `INFO server`: the version for the connection test.
    pub async fn test(&self) -> Result<String, String> {
        self.cmd(&["PING"]).await?;
        let info = self.info("server").await?;
        Ok(info_field(&info, "redis_version").unwrap_or_default())
    }

    pub async fn info(&self, section: &str) -> Result<String, String> {
        Ok(self.cmd(&["INFO", section]).await?.text())
    }

    pub async fn dbsize(&self) -> Result<u64, String> {
        Ok(self.cmd(&["DBSIZE"]).await?.int().max(0) as u64)
    }

    /// One `SCAN` step. `ty` filters server-side (`SCAN ... TYPE x`).
    pub async fn scan(
        &self,
        cursor: u64,
        pattern: &str,
        ty: Option<&str>,
        count: usize,
    ) -> Result<(u64, Vec<String>), String> {
        let mut a: Vec<String> = vec![
            "SCAN".into(),
            cursor.to_string(),
            "COUNT".into(),
            count.to_string(),
        ];
        if !pattern.is_empty() && pattern != "*" {
            a.extend(["MATCH".into(), pattern.into()]);
        }
        if let Some(t) = ty.filter(|t| !t.is_empty()) {
            a.extend(["TYPE".into(), t.into()]);
        }
        Ok(parse_scan(&self.cmd(&a).await?))
    }

    pub async fn key_meta(&self, key: &str) -> Result<KeyMeta, String> {
        let ty = self.cmd(&["TYPE", key]).await?.text();
        let ttl = self.cmd(&["PTTL", key]).await?.int();
        let len_cmd = match ty.as_str() {
            "string" => "STRLEN",
            "hash" => "HLEN",
            "list" => "LLEN",
            "set" => "SCARD",
            "zset" => "ZCARD",
            "stream" => "XLEN",
            _ => "",
        };
        let len = if len_cmd.is_empty() {
            0
        } else {
            self.cmd(&[len_cmd, key]).await?.int().max(0) as u64
        };
        Ok(KeyMeta {
            ty,
            ttl_ms: (ttl >= 0).then_some(ttl),
            len,
        })
    }

    /// `TYPE` + `PTTL` of many keys in one pipelined round trip (for the key list badges).
    pub async fn type_ttl(&self, keys: &[String]) -> Result<Vec<(String, Option<i64>)>, String> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut p = redis::pipe();
        for k in keys {
            p.cmd("TYPE")
                .arg(k.as_bytes())
                .cmd("PTTL")
                .arg(k.as_bytes());
        }
        let mut conn = self.conn.clone();
        let vals: Vec<redis::Value> = p.query_async(&mut conn).await.map_err(|e| e.to_string())?;
        Ok(vals
            .chunks(2)
            .map(|c| {
                let ty = Reply::from_value(c[0].clone()).text();
                let ttl = c
                    .get(1)
                    .map(|v| Reply::from_value(v.clone()).int())
                    .unwrap_or(-1);
                (ty, (ttl >= 0).then_some(ttl))
            })
            .collect())
    }

    /// `MEMORY USAGE key` in bytes (`None` when the key is gone or the command is refused).
    pub async fn memory_usage(&self, key: &str) -> Option<u64> {
        match self.cmd(&["MEMORY", "USAGE", key]).await.ok()? {
            Reply::Int(n) if n >= 0 => Some(n as u64),
            _ => None,
        }
    }

    /// First `max` bytes of a string value.
    pub async fn get_prefix(&self, key: &str, max: usize) -> Result<Vec<u8>, String> {
        Ok(self
            .cmd(&["GETRANGE", key, "0", &(max.max(1) - 1).to_string()])
            .await?
            .bytes())
    }

    pub async fn hscan(
        &self,
        key: &str,
        cursor: u64,
        count: usize,
    ) -> Result<(u64, Vec<(Vec<u8>, Vec<u8>)>), String> {
        let r = self
            .cmd(&[
                "HSCAN",
                key,
                &cursor.to_string(),
                "COUNT",
                &count.to_string(),
            ])
            .await?;
        let (c, items) = parse_scan_items(&r);
        Ok((c, pairs(&items)))
    }

    pub async fn sscan(
        &self,
        key: &str,
        cursor: u64,
        count: usize,
    ) -> Result<(u64, Vec<Vec<u8>>), String> {
        let r = self
            .cmd(&[
                "SSCAN",
                key,
                &cursor.to_string(),
                "COUNT",
                &count.to_string(),
            ])
            .await?;
        let (c, items) = parse_scan_items(&r);
        Ok((c, items.iter().map(Reply::bytes).collect()))
    }

    pub async fn lrange(&self, key: &str, start: u64, count: u64) -> Result<Vec<Vec<u8>>, String> {
        let r = self
            .cmd(&[
                "LRANGE",
                key,
                &start.to_string(),
                &(start + count.max(1) - 1).to_string(),
            ])
            .await?;
        Ok(r.array().iter().map(Reply::bytes).collect())
    }

    pub async fn zrange(
        &self,
        key: &str,
        start: u64,
        count: u64,
    ) -> Result<Vec<(Vec<u8>, f64)>, String> {
        let r = self
            .cmd(&[
                "ZRANGE",
                key,
                &start.to_string(),
                &(start + count.max(1) - 1).to_string(),
                "WITHSCORES",
            ])
            .await?;
        Ok(parse_zrange(&r))
    }

    /// `XRANGE key <from> + COUNT n`; `from` is `-` or `(<id>` for the next page.
    pub async fn xrange(
        &self,
        key: &str,
        from: &str,
        count: u64,
    ) -> Result<Vec<StreamEntry>, String> {
        let r = self
            .cmd(&["XRANGE", key, from, "+", "COUNT", &count.to_string()])
            .await?;
        Ok(parse_xrange(&r))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, String)>,
}

/// `field:value` line of an `INFO` text.
pub fn info_field(info: &str, name: &str) -> Option<String> {
    info.lines().find_map(|l| {
        l.trim()
            .strip_prefix(name)?
            .strip_prefix(':')
            .map(|v| v.trim().to_string())
    })
}

/// `INFO` text as sections of `(key, value)` rows.
pub fn parse_info(info: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for l in info.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(h) = l.strip_prefix('#') {
            out.push((h.trim().to_string(), Vec::new()));
        } else if let Some((k, v)) = l.split_once(':') {
            if out.is_empty() {
                out.push((String::new(), Vec::new()));
            }
            out.last_mut()
                .unwrap()
                .1
                .push((k.to_string(), v.to_string()));
        }
    }
    out
}

pub fn parse_scan(r: &Reply) -> (u64, Vec<String>) {
    let (c, items) = parse_scan_items(r);
    (c, items.iter().map(Reply::text).collect())
}

fn parse_scan_items(r: &Reply) -> (u64, Vec<Reply>) {
    let a = r.array();
    let cursor = a
        .first()
        .map(|c| c.text().parse().unwrap_or(0))
        .unwrap_or(0);
    (
        cursor,
        a.get(1).map(|x| x.array().to_vec()).unwrap_or_default(),
    )
}

fn pairs(items: &[Reply]) -> Vec<(Vec<u8>, Vec<u8>)> {
    items
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0].bytes(), c[1].bytes()))
        .collect()
}

pub fn parse_zrange(r: &Reply) -> Vec<(Vec<u8>, f64)> {
    let a = r.array();
    // RESP2: flat member, score, ...; RESP3 may nest [member, score] pairs.
    if a.first().is_some_and(|x| matches!(x, Reply::Array(_))) {
        return a
            .iter()
            .filter_map(|p| {
                p.array()
                    .get(..2)
                    .map(|p| (p[0].bytes(), p[1].text().parse().unwrap_or(0.0)))
            })
            .collect();
    }
    a.chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0].bytes(), c[1].text().parse().unwrap_or(0.0)))
        .collect()
}

pub fn parse_xrange(r: &Reply) -> Vec<StreamEntry> {
    r.array()
        .iter()
        .filter_map(|e| {
            let e = e.array();
            let id = e.first()?.text();
            let f = e.get(1)?.array();
            Some(StreamEntry {
                id,
                fields: f
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| (c[0].text(), c[1].text()))
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn b(s: &str) -> Reply {
        Reply::Bulk(s.as_bytes().to_vec())
    }

    #[test]
    fn scan_reply() {
        let r = Reply::Array(vec![b("17"), Reply::Array(vec![b("a:1"), b("a:2")])]);
        assert_eq!(parse_scan(&r), (17, vec!["a:1".into(), "a:2".into()]));
        assert_eq!(parse_scan(&Reply::Nil), (0, vec![]));
    }

    #[test]
    fn zrange_flat_and_nested() {
        let flat = Reply::Array(vec![b("x"), b("1.5"), b("y"), b("2")]);
        assert_eq!(
            parse_zrange(&flat),
            vec![(b"x".to_vec(), 1.5), (b"y".to_vec(), 2.0)]
        );
        let nested = Reply::Array(vec![Reply::Array(vec![b("x"), Reply::Double(1.5)])]);
        assert_eq!(parse_zrange(&nested), vec![(b"x".to_vec(), 1.5)]);
    }

    #[test]
    fn xrange_entries() {
        let r = Reply::Array(vec![Reply::Array(vec![
            b("1-0"),
            Reply::Array(vec![b("type"), b("login"), b("user"), b("1")]),
        ])]);
        let e = parse_xrange(&r);
        assert_eq!(
            (
                e[0].id.as_str(),
                e[0].fields.len(),
                e[0].fields[1].1.as_str()
            ),
            ("1-0", 2, "1")
        );
    }

    #[test]
    fn info_text() {
        let t = "# Server\r\nredis_version:8.10.3\r\nos:Windows\r\n\r\n# Keyspace\r\ndb0:keys=5,expires=1,avg_ttl=0\r\n";
        assert_eq!(info_field(t, "redis_version").as_deref(), Some("8.10.3"));
        let s = parse_info(t);
        assert_eq!(
            (s.len(), s[0].0.as_str(), s[1].1[0].0.as_str()),
            (2, "Server", "db0")
        );
    }
}
