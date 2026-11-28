//! In-place edits of Redis values: what to send for a hash / list / zset / set cell edit, how a
//! string is saved without losing its expiry, and the slow log order. Pure, so it is unit-tested
//! without a server; the views are `crates/app/src/redis`.

/// One command, as raw argument bytes.
pub type Cmd = Vec<Vec<u8>>;

/// An edit of one cell of a collection value.
#[derive(Clone, Debug, PartialEq)]
pub enum CellEdit {
    /// Hash: new value of an existing field.
    HashValue { field: Vec<u8>, value: Vec<u8> },
    /// Hash: rename a field, keeping its value.
    HashField {
        old: Vec<u8>,
        new: Vec<u8>,
        value: Vec<u8>,
    },
    /// List: new element at an index.
    ListElem { index: u64, value: Vec<u8> },
    /// Sorted set: new score of a member (text, parsed as f64).
    ZsetScore { member: Vec<u8>, score: String },
    /// Sorted set: rename a member, keeping its score.
    ZsetMember {
        old: Vec<u8>,
        new: Vec<u8>,
        score: f64,
    },
    /// Set: replace a member.
    SetMember { old: Vec<u8>, new: Vec<u8> },
}

/// What to send for an edit. `atomic`: wrap the commands in MULTI / EXEC.
#[derive(Clone, Debug, PartialEq)]
pub struct EditPlan {
    pub cmds: Vec<Cmd>,
    pub atomic: bool,
}

impl EditPlan {
    fn one(cmd: Cmd) -> Self {
        Self {
            cmds: vec![cmd],
            atomic: false,
        }
    }

    fn multi(cmds: Vec<Cmd>) -> Self {
        Self { cmds, atomic: true }
    }

    fn nothing() -> Self {
        Self {
            cmds: vec![],
            atomic: false,
        }
    }
}

fn c(parts: &[&[u8]]) -> Cmd {
    parts.iter().map(|p| p.to_vec()).collect()
}

/// A score the way ZADD takes it: a number or an infinity (`inf`, `-inf`, `+inf`), never NaN.
pub fn parse_score(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    (!v.is_nan()).then_some(v)
}

/// A score as ZADD wants it: the shortest text that parses back to the same f64.
pub fn fmt_score(v: f64) -> String {
    if v.is_infinite() {
        if v > 0.0 { "+inf" } else { "-inf" }.to_string()
    } else {
        v.to_string()
    }
}

/// Commands for a cell edit. `Err` = the score is not a number.
pub fn plan_edit(key: &[u8], e: &CellEdit) -> Result<EditPlan, String> {
    Ok(match e {
        CellEdit::HashValue { field, value } => EditPlan::one(c(&[b"HSET", key, field, value])),
        CellEdit::HashField { old, new, value } => {
            if old == new {
                EditPlan::one(c(&[b"HSET", key, old, value]))
            } else {
                EditPlan::multi(vec![
                    c(&[b"HSET", key, new, value]),
                    c(&[b"HDEL", key, old]),
                ])
            }
        }
        CellEdit::ListElem { index, value } => {
            EditPlan::one(c(&[b"LSET", key, index.to_string().as_bytes(), value]))
        }
        CellEdit::ZsetScore { member, score } => {
            let v = parse_score(score).ok_or_else(|| format!("not a number: {score}"))?;
            // XX: only update an existing member, never re-create one removed meanwhile.
            EditPlan::one(c(&[b"ZADD", key, b"XX", fmt_score(v).as_bytes(), member]))
        }
        CellEdit::ZsetMember { old, new, score } => {
            if old == new {
                return Ok(EditPlan::nothing());
            }
            EditPlan::multi(vec![
                c(&[b"ZREM", key, old]),
                c(&[b"ZADD", key, fmt_score(*score).as_bytes(), new]),
            ])
        }
        CellEdit::SetMember { old, new } => {
            if old == new {
                return Ok(EditPlan::nothing());
            }
            EditPlan::multi(vec![c(&[b"SREM", key, old]), c(&[b"SADD", key, new])])
        }
    })
}

/// Redis 6.0 added `SET ... KEEPTTL`; without a known version assume a current server.
pub fn supports_keepttl(version: Option<&str>) -> bool {
    version
        .and_then(|v| v.split('.').next())
        .and_then(|m| m.trim().parse::<u32>().ok())
        .is_none_or(|major| major >= 6)
}

/// `SET` of a string value that keeps the key's expiry: `KEEPTTL`, or on an old server a
/// `PEXPIRE` with the TTL the key had when it was loaded (in one MULTI / EXEC).
pub fn plan_set_string(key: &[u8], value: &[u8], ttl_ms: Option<i64>, keepttl: bool) -> EditPlan {
    match ttl_ms {
        _ if keepttl => EditPlan::one(c(&[b"SET", key, value, b"KEEPTTL"])),
        // No expiry to keep: a plain SET already leaves none.
        None => EditPlan::one(c(&[b"SET", key, value])),
        Some(ms) => EditPlan::multi(vec![
            c(&[b"SET", key, value]),
            c(&[b"PEXPIRE", key, ms.max(1).to_string().as_bytes()]),
        ]),
    }
}

/// One `SLOWLOG GET` entry: (id, unix seconds, microseconds, command line).
pub type SlowEntry = (i64, i64, i64, String);

/// Newest first: highest id first (ids only grow); the timestamp breaks a tie after a restart.
pub fn sort_slow_newest_first(rows: &mut [SlowEntry]) {
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(c: &Cmd) -> Vec<String> {
        c.iter()
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect()
    }

    #[test]
    fn hash_value_and_rename() {
        let p = plan_edit(
            b"h",
            &CellEdit::HashValue {
                field: b"f".to_vec(),
                value: b"v".to_vec(),
            },
        )
        .unwrap();
        assert!(!p.atomic);
        assert_eq!(s(&p.cmds[0]), ["HSET", "h", "f", "v"]);
        let rename = |new: &[u8]| {
            plan_edit(
                b"h",
                &CellEdit::HashField {
                    old: b"a".to_vec(),
                    new: new.to_vec(),
                    value: b"v".to_vec(),
                },
            )
            .unwrap()
        };
        let p = rename(b"b");
        assert!(p.atomic);
        assert_eq!(s(&p.cmds[0]), ["HSET", "h", "b", "v"]);
        assert_eq!(s(&p.cmds[1]), ["HDEL", "h", "a"]);
        // Same name: a plain HSET, never HSET + HDEL of the same field.
        assert_eq!(rename(b"a").cmds.len(), 1);
    }

    #[test]
    fn list_by_index() {
        let p = plan_edit(
            b"l",
            &CellEdit::ListElem {
                index: 205,
                value: b"x y".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(s(&p.cmds[0]), ["LSET", "l", "205", "x y"]);
    }

    #[test]
    fn zset_score_is_validated() {
        let plan = |sc: &str| {
            plan_edit(
                b"z",
                &CellEdit::ZsetScore {
                    member: b"m".to_vec(),
                    score: sc.into(),
                },
            )
        };
        assert_eq!(
            s(&plan(" 1.5 ").unwrap().cmds[0]),
            ["ZADD", "z", "XX", "1.5", "m"]
        );
        assert_eq!(s(&plan("-inf").unwrap().cmds[0])[3], "-inf");
        assert_eq!(s(&plan("3").unwrap().cmds[0])[3], "3");
        assert!(plan("abc").is_err());
        assert!(plan("").is_err());
        assert!(plan("NaN").is_err());
    }

    #[test]
    fn zset_and_set_member_rename_is_one_transaction() {
        let p = plan_edit(
            b"z",
            &CellEdit::ZsetMember {
                old: b"a".to_vec(),
                new: b"b".to_vec(),
                score: 2.5,
            },
        )
        .unwrap();
        assert!(p.atomic);
        assert_eq!(s(&p.cmds[0]), ["ZREM", "z", "a"]);
        assert_eq!(s(&p.cmds[1]), ["ZADD", "z", "2.5", "b"]);
        let set = |new: &[u8]| {
            plan_edit(
                b"s",
                &CellEdit::SetMember {
                    old: b"a".to_vec(),
                    new: new.to_vec(),
                },
            )
            .unwrap()
        };
        let p = set(b"b");
        assert!(p.atomic);
        assert_eq!(s(&p.cmds[0]), ["SREM", "s", "a"]);
        assert_eq!(s(&p.cmds[1]), ["SADD", "s", "b"]);
        // Unchanged member: nothing to send.
        assert!(set(b"a").cmds.is_empty());
    }

    #[test]
    fn string_set_keeps_ttl() {
        let p = plan_set_string(b"k", b"v", Some(5000), true);
        assert!(!p.atomic);
        assert_eq!(s(&p.cmds[0]), ["SET", "k", "v", "KEEPTTL"]);
        let p = plan_set_string(b"k", b"v", Some(5000), false);
        assert!(p.atomic);
        assert_eq!(s(&p.cmds[0]), ["SET", "k", "v"]);
        assert_eq!(s(&p.cmds[1]), ["PEXPIRE", "k", "5000"]);
        assert_eq!(plan_set_string(b"k", b"v", None, false).cmds.len(), 1);
        assert!(supports_keepttl(Some("7.2.4")));
        assert!(supports_keepttl(Some("6.0.0")));
        assert!(!supports_keepttl(Some("5.0.14")));
        assert!(supports_keepttl(None));
    }

    #[test]
    fn slow_log_newest_first() {
        let mut rows = vec![
            (3, 100, 10, "a".to_string()),
            (7, 300, 10, "c".to_string()),
            (5, 200, 10, "b".to_string()),
        ];
        sort_slow_newest_first(&mut rows);
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [7, 5, 3]);
        // The server already answers newest first: sorting keeps that order.
        sort_slow_newest_first(&mut rows);
        assert_eq!(rows[0].0, 7);
        // After a restart ids start over: the later timestamp wins a tie.
        let mut rows = vec![(1, 10, 0, "x".to_string()), (1, 20, 0, "y".to_string())];
        sort_slow_newest_first(&mut rows);
        assert_eq!(rows[0].3, "y");
    }
}
