//! Every UI language must have exactly the keys of `en.yml`, with the same
//! `%{placeholder}` set per key. A missing, extra or mistyped key fails here.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    /// Flattens the locale file (two-space nesting, `key: value` lines, no
    /// block scalars) into `a.b.c -> set of %{placeholders}`.
    fn flatten(src: &str) -> BTreeMap<String, BTreeSet<String>> {
        let mut out = BTreeMap::new();
        let mut stack: Vec<(usize, String)> = Vec::new();
        for raw in src.lines() {
            let line = raw.trim_end();
            let body = line.trim_start();
            if body.is_empty() || body.starts_with('#') {
                continue;
            }
            let indent = line.len() - body.len();
            let (key, value) = match split_key(body) {
                Some(kv) => kv,
                None => panic!("unparsable locale line: {raw}"),
            };
            while stack.last().is_some_and(|(i, _)| *i >= indent) {
                stack.pop();
            }
            let path = stack
                .iter()
                .map(|(_, k)| k.as_str())
                .chain([key.as_str()])
                .collect::<Vec<_>>()
                .join(".");
            if value.is_empty() {
                stack.push((indent, key));
            } else {
                let dup = out.insert(path.clone(), placeholders(value));
                assert!(dup.is_none(), "duplicate key {path}");
            }
        }
        out
    }

    /// `key: value` or `"key": value`; the key never contains `: `.
    fn split_key(body: &str) -> Option<(String, &str)> {
        let (k, rest) = if let Some(r) = body.strip_prefix('"') {
            let end = r.find('"')?;
            (&r[..end], r[end + 1..].strip_prefix(':')?)
        } else {
            let i = body.find(':')?;
            (&body[..i], &body[i + 1..])
        };
        Some((k.to_string(), rest.trim()))
    }

    fn placeholders(v: &str) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        let mut rest = v;
        while let Some(i) = rest.find("%{") {
            let after = &rest[i + 2..];
            match after.find('}') {
                Some(j) => {
                    set.insert(after[..j].to_string());
                    rest = &after[j + 1..];
                }
                None => break,
            }
        }
        set
    }

    fn check(name: &str, src: &str) {
        let en = flatten(include_str!("../../app/locales/en.yml"));
        let other = flatten(src);
        let missing: Vec<_> = en.keys().filter(|k| !other.contains_key(*k)).collect();
        let extra: Vec<_> = other.keys().filter(|k| !en.contains_key(*k)).collect();
        assert!(missing.is_empty(), "{name}: missing keys {missing:?}");
        assert!(extra.is_empty(), "{name}: extra keys {extra:?}");
        for (k, ph) in &en {
            assert_eq!(&other[k], ph, "{name}: placeholders differ at {k}");
        }
    }

    #[test]
    fn zh_cn_has_every_key_and_placeholder_of_en() {
        check("zh-CN", include_str!("../../app/locales/zh-CN.yml"));
    }

    #[test]
    fn zh_tw_has_every_key_and_placeholder_of_en() {
        check("zh-TW", include_str!("../../app/locales/zh-TW.yml"));
    }

    #[test]
    fn a_missing_key_is_detected() {
        let en = flatten(include_str!("../../app/locales/en.yml"));
        let cut = include_str!("../../app/locales/zh-CN.yml").replace("  save: 保存\n", "");
        assert!(flatten(&cut).len() < en.len());
    }

    #[test]
    fn every_language_has_a_locale_file_test() {
        assert_eq!(crate::settings::LANGUAGES, &["en", "zh-TW", "zh-CN"]);
    }
}
