//! The saved-query library (`saved-queries.json`) and user snippets
//! (`snippets.json`) in the data folder. Never holds a password: a saved
//! query only remembers the id of its connection. Pure logic: load / save,
//! search, import / export, snippet expansion.

use crate::settings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// Connection the query was saved from; `None` = any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub sql: String,
    #[serde(default)]
    pub updated: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct File<T> {
    #[serde(default = "one")]
    version: u32,
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

fn one() -> u32 {
    1
}

fn read<T: for<'de> Deserialize<'de>>(name: &str) -> Vec<T> {
    crate::datafile::load_named::<File<T>>(name)
        .map(|f| f.items)
        .unwrap_or_default()
}

fn write<T: Serialize + Clone>(name: &str, items: &[T]) -> std::io::Result<()> {
    let d = settings::app_dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
    std::fs::create_dir_all(&d)?;
    let f = File {
        version: 1,
        items: items.to_vec(),
    };
    settings::write_atomic(&d.join(name), &serde_json::to_vec_pretty(&f)?)
}

pub fn load_queries() -> Vec<SavedQuery> {
    read("saved-queries.json")
}

pub fn save_queries(q: &[SavedQuery]) -> std::io::Result<()> {
    write("saved-queries.json", q)
}

/// Rows whose name, folder, description or SQL contain every word of
/// `text` (case-insensitive), as indexes in library order.
pub fn search(list: &[SavedQuery], text: &str) -> Vec<usize> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    list.iter()
        .enumerate()
        .filter(|(_, q)| {
            let hay =
                format!("{}\n{}\n{}\n{}", q.name, q.folder, q.description, q.sql).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .map(|(i, _)| i)
        .collect()
}

/// The library as one JSON file (same shape as `saved-queries.json`).
pub fn export_json(list: &[SavedQuery]) -> String {
    serde_json::to_string_pretty(&File {
        version: 1,
        items: list.to_vec(),
    })
    .unwrap_or_default()
}

/// Adds the queries of an exported file to `list`. An id that exists is
/// replaced when its content differs only by being newer; otherwise (same
/// id, different query) the import gets a fresh id. Returns how many were
/// added or replaced.
pub fn import_json(list: &mut Vec<SavedQuery>, json: &str) -> Result<usize, String> {
    // A bare array is accepted too.
    let items: Vec<SavedQuery> = match serde_json::from_str::<File<SavedQuery>>(json) {
        Ok(f) => f.items,
        Err(_) => serde_json::from_str(json).map_err(|e| e.to_string())?,
    };
    let mut n = 0;
    for mut q in items {
        if q.sql.trim().is_empty() && q.name.trim().is_empty() {
            continue;
        }
        if q.id.is_empty() {
            q.id = crate::store::new_id();
        }
        match list.iter().position(|x| x.id == q.id) {
            Some(i) if list[i] == q => {}
            Some(i) if q.updated > list[i].updated => {
                list[i] = q;
                n += 1;
            }
            Some(_) => {
                q.id = crate::store::new_id();
                list.push(q);
                n += 1;
            }
            None => {
                list.push(q);
                n += 1;
            }
        }
    }
    Ok(n)
}

/// Distinct non-empty folders, sorted.
pub fn folders(list: &[SavedQuery]) -> Vec<String> {
    let mut f: Vec<String> = list
        .iter()
        .map(|q| q.folder.clone())
        .filter(|f| !f.is_empty())
        .collect();
    f.sort_by_key(|s| s.to_lowercase());
    f.dedup();
    f
}

// ---- snippets ----------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// Typed in the editor to find it (`sel`).
    pub prefix: String,
    #[serde(default)]
    pub description: String,
    /// Text with `${1:default}`, `$1` and `$0` tab stops.
    pub body: String,
}

pub fn builtin_snippets() -> Vec<Snippet> {
    let s = |p: &str, d: &str, b: &str| Snippet {
        prefix: p.into(),
        description: d.into(),
        body: b.into(),
    };
    vec![
        s(
            "sel",
            "SELECT ... FROM",
            "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:1 = 1}",
        ),
        s(
            "ins",
            "INSERT INTO ... VALUES",
            "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values})",
        ),
        s(
            "upd",
            "UPDATE ... SET ... WHERE",
            "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition}",
        ),
        s(
            "cte",
            "WITH ... AS",
            "WITH ${1:name} AS (\n  SELECT ${2:*}\n  FROM ${3:table}\n)\nSELECT *\nFROM ${1:name}",
        ),
    ]
}

pub fn load_snippets() -> Vec<Snippet> {
    read("snippets.json")
}

pub fn save_snippets(s: &[Snippet]) -> std::io::Result<()> {
    write("snippets.json", s)
}

/// Built-ins first, then the user's; a user snippet with the same prefix
/// replaces the built-in.
pub fn all_snippets(user: &[Snippet]) -> Vec<Snippet> {
    let mut v: Vec<Snippet> = builtin_snippets()
        .into_iter()
        .filter(|b| {
            !user
                .iter()
                .any(|u| u.prefix.eq_ignore_ascii_case(&b.prefix))
        })
        .collect();
    v.extend(user.iter().filter(|u| !u.prefix.trim().is_empty()).cloned());
    v
}

/// Snippets whose prefix starts with `typed` (case-insensitive); an empty
/// `typed` matches none (snippets only show once a letter is typed).
pub fn matching<'a>(all: &'a [Snippet], typed: &str) -> Vec<&'a Snippet> {
    if typed.is_empty() {
        return Vec::new();
    }
    let t = typed.to_lowercase();
    all.iter()
        .filter(|s| s.prefix.to_lowercase().starts_with(&t))
        .collect()
}

/// A snippet body as plain text: every `${n:default}` becomes its default,
/// a bare `$n` / `${n}` reuses the default of the same number given
/// elsewhere (or nothing), `$0` disappears, `\$` is a dollar sign. Returns
/// the text and the byte range of each tab stop in order (1, 2, ... then 0).
pub fn expand(body: &str) -> (String, Vec<std::ops::Range<usize>>) {
    // First pass: default text per number.
    let mut defaults: std::collections::HashMap<u32, String> = Default::default();
    let b: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$'
            && b.get(i + 1) == Some(&'{')
            && let Some((n, def, end)) = braced(&b, i)
        {
            if !def.is_empty() {
                defaults.entry(n).or_insert(def);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    let mut out = String::new();
    let mut stops: Vec<(u32, std::ops::Range<usize>)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '\\' if b.get(i + 1) == Some(&'$') => {
                out.push('$');
                i += 2;
            }
            '$' if b.get(i + 1) == Some(&'{') => match braced(&b, i) {
                Some((n, def, end)) => {
                    let text = if def.is_empty() {
                        defaults.get(&n).cloned().unwrap_or_default()
                    } else {
                        def
                    };
                    let s = out.len();
                    out.push_str(&text);
                    stops.push((n, s..out.len()));
                    i = end;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            },
            '$' if b.get(i + 1).is_some_and(char::is_ascii_digit) => {
                let mut j = i + 1;
                let mut n = 0u32;
                while j < b.len() && b[j].is_ascii_digit() {
                    n = n
                        .saturating_mul(10)
                        .saturating_add(b[j].to_digit(10).unwrap());
                    j += 1;
                }
                let text = defaults.get(&n).cloned().unwrap_or_default();
                let s = out.len();
                out.push_str(&text);
                stops.push((n, s..out.len()));
                i = j;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Stops in order 1, 2, ...; 0 (the end) last.
    stops.sort_by_key(|(n, r)| (if *n == 0 { u32::MAX } else { *n }, r.start));
    (out, stops.into_iter().map(|(_, r)| r).collect())
}

/// `${n:default}` or `${n}` at `b[i..]`: number, default text, index after `}`.
fn braced(b: &[char], i: usize) -> Option<(u32, String, usize)> {
    let mut j = i + 2;
    let mut n = 0u32;
    let s = j;
    while j < b.len() && b[j].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(b[j].to_digit(10)?);
        j += 1;
    }
    if j == s {
        return None;
    }
    match b.get(j) {
        Some('}') => Some((n, String::new(), j + 1)),
        Some(':') => {
            let mut def = String::new();
            let mut k = j + 1;
            while k < b.len() && b[k] != '}' {
                if b[k] == '\\' && k + 1 < b.len() {
                    k += 1;
                }
                def.push(b[k]);
                k += 1;
            }
            (k < b.len()).then_some((n, def, k + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(id: &str, name: &str, sql: &str, updated: u64) -> SavedQuery {
        SavedQuery {
            id: id.into(),
            name: name.into(),
            sql: sql.into(),
            updated,
            ..Default::default()
        }
    }

    #[test]
    fn search_words_anywhere() {
        let mut a = q("1", "Top users", "select * from users", 1);
        a.folder = "Reports".into();
        a.description = "monthly".into();
        let b = q("2", "Orders", "select * from orders", 1);
        let l = vec![a, b];
        assert_eq!(search(&l, ""), vec![0, 1]);
        assert_eq!(search(&l, "USERS report"), vec![0]);
        assert_eq!(search(&l, "orders monthly"), Vec::<usize>::new());
        assert_eq!(search(&l, "monthly"), vec![0]);
    }

    #[test]
    fn json_roundtrip_has_no_secret_fields() {
        let mut a = q("1", "n", "select 1", 5);
        a.conn_id = Some("c1".into());
        let j = export_json(&[a.clone()]);
        assert!(!j.to_lowercase().contains("password"));
        let mut l = Vec::new();
        assert_eq!(import_json(&mut l, &j), Ok(1));
        assert_eq!(l, vec![a]);
        // Importing the same file again changes nothing.
        assert_eq!(import_json(&mut l, &j), Ok(0));
    }

    #[test]
    fn import_conflicts() {
        let mut l = vec![q("1", "a", "select 1", 10)];
        let newer = export_json(&[q("1", "a2", "select 2", 20)]);
        assert_eq!(import_json(&mut l, &newer), Ok(1));
        assert_eq!(l[0].name, "a2");
        let older = export_json(&[q("1", "a3", "select 3", 5)]);
        assert_eq!(import_json(&mut l, &older), Ok(1));
        assert_eq!(l.len(), 2);
        assert_ne!(l[0].id, l[1].id);
    }

    #[test]
    fn import_array_and_garbage() {
        let mut l = Vec::new();
        assert_eq!(
            import_json(&mut l, r#"[{"id":"x","name":"n","sql":"select 1"}]"#),
            Ok(1)
        );
        assert!(import_json(&mut l, "nope").is_err());
        assert_eq!(
            import_json(&mut l, r#"{"items":[{"id":"","name":"","sql":""}]}"#),
            Ok(0)
        );
    }

    #[test]
    fn folders_sorted_distinct() {
        let mut a = q("1", "a", "s", 0);
        a.folder = "b".into();
        let mut b = q("2", "b", "s", 0);
        b.folder = "A".into();
        let mut c = q("3", "c", "s", 0);
        c.folder = "b".into();
        assert_eq!(folders(&[a, b, c, q("4", "d", "s", 0)]), ["A", "b"]);
    }

    #[test]
    fn ids_unique() {
        assert_ne!(crate::store::new_id(), crate::store::new_id());
    }

    #[test]
    fn expand_defaults_and_stops() {
        let (t, s) = expand("SELECT ${1:*}\nFROM ${2:table}\nWHERE $0");
        assert_eq!(t, "SELECT *\nFROM table\nWHERE ");
        assert_eq!(s.len(), 3);
        assert_eq!(&t[s[0].clone()], "*");
        assert_eq!(&t[s[1].clone()], "table");
        assert_eq!(s[2], t.len()..t.len());
    }

    #[test]
    fn expand_mirrors_and_escapes() {
        let (t, _) = expand("WITH ${1:name} AS (SELECT 1) SELECT * FROM $1 \\$5 ${2} $");
        assert_eq!(t, "WITH name AS (SELECT 1) SELECT * FROM name $5  $");
        let (t, _) = expand("${1:a\\}b}");
        assert_eq!(t, "a}b");
        let (t, _) = expand("${x");
        assert_eq!(t, "${x");
    }

    #[test]
    fn builtins_and_matching() {
        let all = all_snippets(&[
            Snippet {
                prefix: "sel".into(),
                description: "mine".into(),
                body: "SELECT 1".into(),
            },
            Snippet {
                prefix: "zz".into(),
                description: String::new(),
                body: "z".into(),
            },
        ]);
        assert_eq!(all.iter().filter(|s| s.prefix == "sel").count(), 1);
        assert_eq!(
            all.iter().find(|s| s.prefix == "sel").unwrap().body,
            "SELECT 1"
        );
        for p in ["ins", "upd", "cte", "zz"] {
            assert!(all.iter().any(|s| s.prefix == p), "{p}");
        }
        assert!(matching(&all, "").is_empty());
        assert_eq!(matching(&all, "S").len(), 1);
        assert_eq!(matching(&all, "u").len(), 1);
    }
}
