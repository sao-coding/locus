//! Integration tests against a real server. `#[ignore]`: run with
//! `REDIS_URL=redis://:<password>@127.0.0.1:6380/0 cargo test -p db-redis -- --ignored`
//! (a local Redis server). They only touch `wb_test:*` keys.
use db_redis::{Config, RedisDriver, Reply};

fn config() -> Option<Config> {
    let url = std::env::var("REDIS_URL").ok()?;
    let rest = url.strip_prefix("redis://")?;
    let (auth, host) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (hostport, db) = host.split_once('/').unwrap_or((host, "0"));
    let (h, p) = hostport.rsplit_once(':').unwrap_or((hostport, "6379"));
    let (user, pass) = auth.split_once(':').unwrap_or((auth, ""));
    Some(Config {
        host: h.into(),
        port: p.parse().ok()?,
        user: (!user.is_empty()).then(|| user.to_string()),
        password: (!pass.is_empty()).then(|| pass.to_string()),
        db: db.parse().unwrap_or(0),
        tls: false,
        insecure: false,
    })
}

async fn driver() -> Option<RedisDriver> {
    let d = RedisDriver::connect(config()?)
        .await
        .unwrap_or_else(|e| panic!("connect: {e}"));
    Some(d)
}

#[tokio::test]
#[ignore]
async fn ping_and_version() {
    let Some(d) = driver().await else { return };
    let v = d.test().await.unwrap();
    assert!(v.split('.').count() >= 2, "version {v}");
    assert!(d.dbsize().await.is_ok());
}

#[tokio::test]
#[ignore]
async fn every_type_roundtrip() {
    let Some(d) = driver().await else { return };
    let d = &d;
    let c = |a: &[&str]| {
        let a: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        async move { d.cmd(&a).await }
    };
    c(&[
        "DEL",
        "wb_test:s",
        "wb_test:h",
        "wb_test:l",
        "wb_test:set",
        "wb_test:z",
        "wb_test:x",
    ])
    .await
    .unwrap();
    c(&["SET", "wb_test:s", "hello", "EX", "100"])
        .await
        .unwrap();
    c(&["HSET", "wb_test:h", "a", "1", "b", "2"]).await.unwrap();
    c(&["RPUSH", "wb_test:l", "x", "y", "z"]).await.unwrap();
    c(&["SADD", "wb_test:set", "m1", "m2"]).await.unwrap();
    c(&["ZADD", "wb_test:z", "1.5", "a", "2", "b"])
        .await
        .unwrap();
    c(&["XADD", "wb_test:x", "1-1", "f", "v"]).await.unwrap();

    let m = d.key_meta("wb_test:s").await.unwrap();
    assert_eq!((m.ty.as_str(), m.len), ("string", 5));
    assert!(m.ttl_ms.is_some_and(|t| t > 0 && t <= 100_000));
    assert_eq!(d.get_prefix("wb_test:s", 3).await.unwrap(), b"hel");
    assert_eq!(d.key_meta("wb_test:nope").await.unwrap().ty, "none");

    let (cur, rows) = d.hscan("wb_test:h", 0, 100).await.unwrap();
    assert_eq!((cur, rows.len()), (0, 2));
    assert_eq!(
        d.lrange("wb_test:l", 1, 2).await.unwrap(),
        vec![b"y".to_vec(), b"z".to_vec()]
    );
    let (_, members) = d.sscan("wb_test:set", 0, 100).await.unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(
        d.zrange("wb_test:z", 0, 10).await.unwrap(),
        vec![(b"a".to_vec(), 1.5), (b"b".to_vec(), 2.0)]
    );
    let xs = d.xrange("wb_test:x", "-", 10).await.unwrap();
    assert_eq!(
        (xs[0].id.as_str(), xs[0].fields[0].0.as_str()),
        ("1-1", "f")
    );
    assert_eq!(d.key_meta("wb_test:x").await.unwrap().ty, "stream");

    // SCAN with MATCH and TYPE never needs KEYS.
    let mut cursor = 0;
    let mut found = Vec::new();
    loop {
        let (next, keys) = d
            .scan(cursor, "wb_test:*", Some("hash"), 100)
            .await
            .unwrap();
        found.extend(keys);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    assert_eq!(found, vec!["wb_test:h".to_string()]);

    // Errors come back as Err, not as a panic.
    assert!(c(&["LPUSH", "wb_test:s", "x"]).await.is_err());
    assert_eq!(c(&["PERSIST", "wb_test:s"]).await.unwrap(), Reply::Int(1));
    assert_eq!(d.key_meta("wb_test:s").await.unwrap().ttl_ms, None);
    c(&[
        "DEL",
        "wb_test:s",
        "wb_test:h",
        "wb_test:l",
        "wb_test:set",
        "wb_test:z",
        "wb_test:x",
    ])
    .await
    .unwrap();
}

#[tokio::test]
#[ignore]
async fn type_ttl_and_memory_usage() {
    let Some(d) = driver().await else { return };
    let c = |a: &[&str]| {
        let a: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        let d = &d;
        async move { d.cmd(&a).await }
    };
    c(&["DEL", "wb_test:tt1", "wb_test:tt2"]).await.unwrap();
    c(&["SET", "wb_test:tt1", "v", "EX", "100"]).await.unwrap();
    c(&["RPUSH", "wb_test:tt2", "a"]).await.unwrap();
    let keys = vec![
        "wb_test:tt1".to_string(),
        "wb_test:tt2".to_string(),
        "wb_test:missing".to_string(),
    ];
    let r = d.type_ttl(&keys).await.unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].0, "string");
    assert!(r[0].1.is_some_and(|t| t > 0 && t <= 100_000));
    assert_eq!((r[1].0.as_str(), r[1].1), ("list", None));
    assert_eq!(r[2].0, "none");
    assert!(d.memory_usage("wb_test:tt1").await.is_some_and(|n| n > 0));
    assert_eq!(d.memory_usage("wb_test:missing").await, None);
    c(&["DEL", "wb_test:tt1", "wb_test:tt2"]).await.unwrap();
}
