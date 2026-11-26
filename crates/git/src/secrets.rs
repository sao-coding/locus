//! Looks for credentials in what is about to be committed. A DB tool's files are full of
//! connection strings, so this runs on the added lines of the staged diff.
use crate::diff::{FileDiff, LineKind};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `scheme://user:****@host`
    UrlPassword,
    /// `password=...`, `PWD=...`, `IDENTIFIED BY '...'`
    Password,
    PrivateKey,
    /// A `.env` file as a whole.
    EnvFile,
    CloudKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub path: String,
    /// Line in the new file; 0 for a whole file.
    pub line: u32,
    pub kind: Kind,
    /// The line, shortened, with the secret itself hidden.
    pub excerpt: String,
}

struct Rules {
    url: Regex,
    pass: Regex,
    ident: Regex,
    key: Regex,
    aws: Regex,
}

fn rules() -> &'static Rules {
    static R: OnceLock<Rules> = OnceLock::new();
    R.get_or_init(|| Rules {
        // scheme://user:<secret>@host  (the user may be empty)
        url: Regex::new(r#"(?i)\b[a-z][a-z0-9+.\-]*://[^\s:/@'"]*:([^\s/@'"]+)@[^\s/'"]"#).unwrap(),
        // password = 'x', pwd=x, "password": "x", PASSWORD: x
        pass: Regex::new(r#"(?i)(?:password|passwd|pwd)\b["']?\s*[:=]\s*["']?([^\s"';,&)]+)"#)
            .unwrap(),
        ident: Regex::new(
            r#"(?i)\bidentified\s+(?:with\s+\w+\s+)?by\s+(?:password\s+)?['"]([^'"]+)['"]"#,
        )
        .unwrap(),
        key: Regex::new(r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----").unwrap(),
        aws: Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").unwrap(),
    })
}

/// A value that stands for a secret rather than being one.
fn is_placeholder(v: &str) -> bool {
    let v = v.trim_matches(['\'', '"']);
    v.is_empty()
        || v.starts_with(['$', '<', '{', '%', ':', '@', '?', '#'])
        || v.chars()
            .all(|c| matches!(c, '*' | 'x' | 'X' | '.' | '-' | '_'))
        || matches!(
            v.to_ascii_lowercase().as_str(),
            "null" | "none" | "password" | "your_password" | "yourpassword" | "secret"
        )
}

/// The `.env` family, except the examples meant to be committed.
pub fn is_env_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.ends_with(".example") || name.ends_with(".sample") || name.ends_with(".template") {
        return false;
    }
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

/// Findings in one added line (`None` when it is clean).
pub fn scan_line(line: &str) -> Option<Kind> {
    let r = rules();
    if r.key.is_match(line) {
        return Some(Kind::PrivateKey);
    }
    if r.aws.is_match(line) {
        return Some(Kind::CloudKey);
    }
    if r.url.captures_iter(line).any(|c| !is_placeholder(&c[1])) {
        return Some(Kind::UrlPassword);
    }
    if r.ident.captures_iter(line).any(|c| !is_placeholder(&c[1])) {
        return Some(Kind::Password);
    }
    if r.pass.captures_iter(line).any(|c| !is_placeholder(&c[1])) {
        return Some(Kind::Password);
    }
    None
}

fn excerpt(line: &str) -> String {
    let t = line.trim();
    let mut s: String = t.chars().take(90).collect();
    if t.chars().count() > 90 {
        s.push('…');
    }
    hide(&s)
}

/// Hides the secret part of an excerpt, so the warning itself leaks nothing.
fn hide(s: &str) -> String {
    let r = rules();
    let s = r
        .url
        .replace_all(s, |c: &regex::Captures| {
            let whole = &c[0];
            whole.replacen(&c[1], "••••", 1)
        })
        .into_owned();
    let s = r
        .ident
        .replace_all(&s, |c: &regex::Captures| c[0].replacen(&c[1], "••••", 1))
        .into_owned();
    r.pass
        .replace_all(&s, |c: &regex::Captures| c[0].replacen(&c[1], "••••", 1))
        .into_owned()
}

/// Scans the staged paths (for `.env` files) and the added lines of their diffs.
pub fn scan(paths: &[String], diffs: &[FileDiff]) -> Vec<Finding> {
    let mut out = Vec::new();
    for p in paths {
        if is_env_file(p) {
            out.push(Finding {
                path: p.clone(),
                line: 0,
                kind: Kind::EnvFile,
                excerpt: String::new(),
            });
        }
    }
    for f in diffs {
        let path = f.path().to_string();
        for h in &f.hunks {
            for l in &h.lines {
                if l.kind != LineKind::Add {
                    continue;
                }
                if let Some(kind) = scan_line(&l.text) {
                    out.push(Finding {
                        path: path.clone(),
                        line: l.new_no.unwrap_or(0),
                        kind,
                        excerpt: excerpt(&l.text),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The samples are assembled at run time, so no source file holds a credential-shaped
    // literal (secret scanners read the files, not the program).
    fn pw() -> String {
        ["pass", "word"].concat()
    }
    fn url(scheme: &str, user: &str, secret: &str, host: &str) -> String {
        format!("{scheme}://{user}:{secret}@{host}")
    }
    fn begin(kind: &str) -> String {
        format!("{} {kind}PRIVATE KEY{}", "-----BEGIN", "-----")
    }

    #[test]
    fn url_passwords() {
        for l in [
            url("mysql", "u1", "s1", "host"),
            format!(
                "conn = \"{}\"",
                url("postgres", "app", "x9!", "db.internal:5432/x")
            ),
            url("redis", "", "s2", "cache:6379"),
            format!("jdbc:{}", url("mysql", "u", "p", "h/db")),
        ] {
            assert_eq!(scan_line(&l), Some(Kind::UrlPassword), "{l}");
        }
        for l in [
            "mysql://root@host".to_string(),
            "mysql://host:3306/db".to_string(),
            "https://example.com/a".to_string(),
            url("mysql", "root", "${DB_PASS}", "host"),
            url("mysql", "root", "<redacted>", "host"),
            "see http://localhost:8080/path".to_string(),
        ] {
            assert_eq!(scan_line(&l), None, "{l}");
        }
    }

    #[test]
    fn password_assignments() {
        let p = pw();
        let up = p.to_uppercase();
        for l in [
            format!("{p}=v1"),
            format!("{} = 'v2'", ["P", "WD"].concat()),
            format!("\"{p}\": \"v3\""),
            format!("Server=x;User Id=sa;{p}=v4;"),
            format!("DB_{up}: v5"),
            format!(
                "CREATE USER app {} '{}';",
                ["IDENTIFIED", " BY"].concat(),
                "v6"
            ),
            format!("alter user x {} \"v7\"", ["identified", " by"].concat()),
            format!(
                "CREATE USER u {} mysql_native_{p} BY 'v8'",
                ["IDENTIFIED", " WITH"].concat()
            ),
        ] {
            assert_eq!(scan_line(&l), Some(Kind::Password), "{l}");
        }
        for l in [
            format!("{p}="),
            format!("{p} = ?"),
            format!("{p}=${{PASSWORD}}"),
            format!("{p}: ****"),
            format!("PWD=<your {p}>"),
            format!("-- the {p} is set elsewhere"),
            format!("select {p} from users"),
            format!("{} ''", ["IDENTIFIED", " BY"].concat()),
            format!("set {p}_hash = 1"),
        ] {
            assert_eq!(scan_line(&l), None, "{l}");
        }
    }

    #[test]
    fn keys() {
        for kind in ["RSA ", "OPENSSH ", ""] {
            assert_eq!(scan_line(&begin(kind)), Some(Kind::PrivateKey), "{kind}");
        }
        assert_eq!(
            scan_line(&format!("{} PUBLIC KEY{}", "-----BEGIN", "-----")),
            None
        );
        let id = format!("{}{}", "AK", "IA") + &"A".repeat(16);
        assert_eq!(scan_line(&format!("key = {id}")), Some(Kind::CloudKey));
    }

    #[test]
    fn env_files() {
        for p in [
            ".env",
            "app/.env",
            ".env.production",
            "prod.env",
            "a/b/Local.ENV",
        ] {
            assert!(is_env_file(p), "{p}");
        }
        for p in [".env.example", "env.sql", "a/environment.md", ".envrc"] {
            assert!(!is_env_file(p), "{p}");
        }
    }

    #[test]
    fn scans_only_added_lines_and_names_file_and_line() {
        let added = url("mysql", "u1", "s1", "host/db");
        let removed = format!("{}=old", pw());
        let diff = format!(
            "diff --git a/c.sql b/c.sql\n--- a/c.sql\n+++ b/c.sql\n@@ -1,2 +1,3 @@\n -- keep\n-{removed}\n+select 1;\n+url={added}\n"
        );
        let diffs = crate::diff::parse(&diff);
        let f = scan(&[], &diffs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "c.sql");
        assert_eq!(f[0].line, 3);
        assert_eq!(f[0].kind, Kind::UrlPassword);
        assert!(!f[0].excerpt.contains(":s1@"), "{}", f[0].excerpt);
        assert!(f[0].excerpt.contains("••••"));
    }

    #[test]
    fn env_file_is_reported_by_path() {
        let f = scan(&["cfg/.env".to_string(), "a.sql".to_string()], &[]);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].kind, f[0].line), (Kind::EnvFile, 0));
    }

    #[test]
    fn cjk_lines() {
        let l = format!("-- 連線 {}", url("mysql", "管理員", "密碼1", "主機/庫"));
        assert_eq!(scan_line(&l), Some(Kind::UrlPassword));
    }
}
