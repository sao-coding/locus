//! `git log` with a fixed `-z` format → commits; `show --name-status -z` → the files of one.

const FS: char = '\x1f';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Unix seconds.
    pub time: i64,
    pub subject: String,
}

/// Arguments of one page of history. With `path`, only that file (following renames).
pub fn args(skip: usize, count: usize, path: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "log".into(),
        "-z".into(),
        format!("--format=%H{FS}%h{FS}%an{FS}%at{FS}%s"),
        format!("--skip={skip}"),
        format!("-n{count}"),
    ];
    if let Some(p) = path {
        a.push("--follow".into());
        a.push("--".into());
        a.push(p.to_string());
    }
    a
}

pub fn parse(out: &str) -> Vec<Commit> {
    out.split('\0')
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| {
            let f: Vec<&str> = r.trim_start_matches('\n').splitn(5, FS).collect();
            if f.len() < 5 {
                return None;
            }
            Some(Commit {
                hash: f[0].to_string(),
                short: f[1].to_string(),
                author: f[2].to_string(),
                time: f[3].parse().unwrap_or(0),
                subject: f[4].to_string(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// `M`, `A`, `D`, `R`, `C`, `T`.
    pub letter: char,
    pub path: String,
    pub orig: Option<String>,
}

/// Arguments listing the files a commit changed.
pub fn files_args(hash: &str) -> Vec<String> {
    [
        "show",
        "--no-color",
        "--format=",
        "--name-status",
        "-z",
        "-M",
        hash,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Parses `--name-status -z`: `M NUL path NUL`, `R100 NUL old NUL new NUL`.
pub fn parse_files(out: &str) -> Vec<FileChange> {
    let mut v = Vec::new();
    let mut it = out.split('\0').filter(|s| !s.is_empty());
    while let Some(code) = it.next() {
        let code = code.trim();
        let Some(letter) = code.chars().next() else {
            continue;
        };
        if letter == 'R' || letter == 'C' {
            let (Some(old), Some(new)) = (it.next(), it.next()) else {
                break;
            };
            v.push(FileChange {
                letter,
                path: new.to_string(),
                orig: Some(old.to_string()),
            });
        } else {
            let Some(p) = it.next() else { break };
            v.push(FileChange {
                letter,
                path: p.to_string(),
                orig: None,
            });
        }
    }
    v
}

/// How long ago a commit was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    Now,
    Minutes(u32),
    Hours(u32),
    Days(u32),
    Months(u32),
    Years(u32),
}

pub fn age(now: i64, then: i64) -> Age {
    let s = (now - then).max(0);
    match s {
        0..=59 => Age::Now,
        60..=3599 => Age::Minutes((s / 60) as u32),
        3600..=86_399 => Age::Hours((s / 3600) as u32),
        86_400..=2_591_999 => Age::Days((s / 86_400) as u32),
        2_592_000..=31_535_999 => Age::Months((s / 2_592_000) as u32),
        _ => Age::Years((s / 31_536_000) as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(age(1000, 990), Age::Now);
        assert_eq!(age(1000, 1000 + 5), Age::Now);
        assert_eq!(age(10_000, 10_000 - 120), Age::Minutes(2));
        assert_eq!(age(100_000, 100_000 - 7300), Age::Hours(2));
        assert_eq!(age(1_000_000, 1_000_000 - 3 * 86_400), Age::Days(3));
        assert_eq!(age(10_000_000, 10_000_000 - 3 * 2_592_000), Age::Months(3));
        assert_eq!(age(100_000_000, 0), Age::Years(3));
    }

    #[test]
    fn args_page_and_follow() {
        let a = args(50, 25, None);
        assert!(a.contains(&"--skip=50".to_string()) && a.contains(&"-n25".to_string()));
        assert!(!a.contains(&"--follow".to_string()));
        let a = args(0, 10, Some("a b/報表.sql"));
        assert_eq!(a[a.len() - 3..], ["--follow", "--", "a b/報表.sql"]);
    }

    #[test]
    fn parses_records() {
        let out = format!(
            "aaa{FS}a1{FS}王小明{FS}1700000000{FS}修正 月報表\0bbb{FS}b2{FS}Ann{FS}1690000000{FS}fix: a{FS}b\0"
        );
        let c = parse(&out);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].author, "王小明");
        assert_eq!(c[0].subject, "修正 月報表");
        assert_eq!(c[0].time, 1_700_000_000);
        // A unit separator inside the subject stays in the subject.
        assert_eq!(c[1].subject, format!("fix: a{FS}b"));
    }

    #[test]
    fn empty_and_garbage() {
        assert!(parse("").is_empty());
        assert!(parse("junk\0").is_empty());
    }

    #[test]
    fn name_status_with_rename_and_cjk() {
        let out = "M\0a.sql\0R100\0舊 名.sql\0新 名.sql\0A\0b.sql\0D\0c.sql\0";
        let f = parse_files(out);
        assert_eq!(f.len(), 4);
        assert_eq!(f[0].letter, 'M');
        assert_eq!(f[1].letter, 'R');
        assert_eq!(f[1].path, "新 名.sql");
        assert_eq!(f[1].orig.as_deref(), Some("舊 名.sql"));
        assert_eq!(f[3].path, "c.sql");
    }
}
