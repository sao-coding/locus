//! Runs the real `git` against throwaway repositories in the temp dir (a bare repo is the
//! remote). Skipped when git is not installed.
use locus_git::ops::{Discard, Repo};
use locus_git::{Cancel, Git, PullMode, secrets};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("locus-git-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn git_ok() -> bool {
    Repo::version(&Git::default()).is_ok()
}

fn sh(dir: &Path, args: &[&str]) {
    let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Git::default().run(dir, &a, None).unwrap();
}

fn identity(dir: &Path) {
    sh(dir, &["config", "user.email", "t@example.invalid"]);
    sh(dir, &["config", "user.name", "Tester"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
}

#[test]
fn status_stage_commit_diff_log() {
    if !git_ok() {
        return;
    }
    let d = scratch("flow");
    let repo = Repo::init(&Git::default(), &d, true).unwrap();
    identity(&d);
    assert!(d.join(".gitignore").exists());
    let st = repo.status().unwrap();
    assert!(st.branch.oid.is_none(), "unborn branch");

    std::fs::create_dir_all(d.join("報表")).unwrap();
    std::fs::write(d.join("報表/月 報.sql"), "select 1;\n").unwrap();
    let st = repo.status().unwrap();
    let e = st.find("報表/月 報.sql").expect("CJK path as is");
    assert!(e.is_untracked());

    // Untracked diff is synthesised.
    let df = repo.diff_file("報表/月 報.sql", false, true).unwrap();
    assert_eq!(df[0].added(), 1);

    repo.stage(&["報表/月 報.sql".to_string()]).unwrap();
    let st = repo.status().unwrap();
    assert_eq!(st.staged().count(), 1);
    let staged = repo.diff_file("報表/月 報.sql", true, false).unwrap();
    assert_eq!(staged[0].added(), 1);

    repo.commit("first\n\nbody", false, false, &Cancel::new())
        .unwrap();
    let log = repo.log(0, 10, None).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].subject, "first");
    let files = repo.commit_files(&log[0].hash).unwrap();
    assert_eq!(files[0].path, "報表/月 報.sql");
    assert_eq!(repo.diff_commit(&log[0].hash, None).unwrap()[0].added(), 1);

    // Edit → M in the work tree, diff shows the change.
    std::fs::write(d.join("報表/月 報.sql"), "select 2;\n").unwrap();
    let st = repo.status().unwrap();
    assert_eq!(st.unstaged().filter(|e| !e.is_untracked()).count(), 1);
    let df = repo.diff_file("報表/月 報.sql", false, false).unwrap();
    assert_eq!((df[0].added(), df[0].removed()), (1, 1));

    // Discard puts it back.
    repo.discard(&["報表/月 報.sql".to_string()], Discard::Worktree)
        .unwrap();
    let left: Vec<_> = repo
        .status()
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(left, [".gitignore"]);

    // History of one file.
    assert_eq!(repo.log(0, 10, Some("報表/月 報.sql")).unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn unstage_before_the_first_commit_and_empty_log() {
    if !git_ok() {
        return;
    }
    let d = scratch("unborn");
    let repo = Repo::init(&Git::default(), &d, false).unwrap();
    identity(&d);
    assert!(repo.log(0, 10, None).unwrap().is_empty());
    std::fs::write(d.join("a.sql"), "x").unwrap();
    repo.stage_all().unwrap();
    assert_eq!(repo.status().unwrap().staged().count(), 1);
    repo.unstage(&["a.sql".to_string()]).unwrap();
    assert_eq!(repo.status().unwrap().staged().count(), 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn branches_push_pull_against_a_bare_remote() {
    if !git_ok() {
        return;
    }
    let base = scratch("remote");
    let bare = base.join("origin.git");
    let one = base.join("one");
    let two = base.join("two");
    std::fs::create_dir_all(&bare).unwrap();
    sh(&bare, &["init", "--bare", "-b", "main"]);
    let g = Git::default();
    let a = Repo::init(
        &g,
        &{
            std::fs::create_dir_all(&one).unwrap();
            one.clone()
        },
        false,
    )
    .unwrap();
    identity(&one);
    sh(&one, &["checkout", "-B", "main"]);
    sh(&one, &["remote", "add", "origin", bare.to_str().unwrap()]);
    std::fs::write(one.join("a.sql"), "select 1;\n").unwrap();
    a.stage_all().unwrap();
    a.commit("c1", false, false, &Cancel::new()).unwrap();
    let st = a.status().unwrap();
    assert!(st.branch.upstream.is_none());
    a.push(st.branch.head.as_deref(), false, &Cancel::new())
        .unwrap();
    let st = a.status().unwrap();
    assert_eq!(st.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.branch.ahead, st.branch.behind), (0, 0));

    // A second clone pushes a commit; the first sees it behind, then pulls it.
    sh(&base, &["clone", bare.to_str().unwrap(), "two"]);
    identity(&two);
    std::fs::write(two.join("b.sql"), "select 2;\n").unwrap();
    let b = Repo::new(g.clone(), two.clone());
    b.stage_all().unwrap();
    b.commit("c2", false, false, &Cancel::new()).unwrap();
    b.push(Some("main"), true, &Cancel::new()).unwrap();

    a.fetch(&Cancel::new()).unwrap();
    assert_eq!(a.status().unwrap().branch.behind, 1);
    a.pull(PullMode::FfOnly, &Cancel::new()).unwrap();
    assert_eq!(a.status().unwrap().branch.behind, 0);
    assert!(one.join("b.sql").exists());

    // Branches: create, list, switch back.
    a.create_branch("feature/報表").unwrap();
    let list = a.branches().unwrap();
    assert!(list.iter().any(|x| x.name == "feature/報表" && x.current));
    a.switch("main", false).unwrap();
    assert_eq!(a.status().unwrap().branch.head.as_deref(), Some("main"));

    // A failing command carries git's own words.
    let e = a.switch("nope", false).unwrap_err();
    assert!(e.to_string().to_lowercase().contains("nope"), "{e}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn secret_scan_on_a_real_staged_diff() {
    if !git_ok() {
        return;
    }
    let d = scratch("secret");
    let repo = Repo::init(&Git::default(), &d, false).unwrap();
    // Built at run time: no credential-shaped literal in the source.
    let line = format!("-- url\n{}://{}:{}@host/db\n", "mysql", "root", "s1");
    std::fs::write(d.join("conn.sql"), line).unwrap();
    std::fs::write(d.join(".env"), "A=1\n").unwrap();
    repo.stage_all().unwrap();
    let paths: Vec<String> = repo
        .status()
        .unwrap()
        .staged()
        .map(|e| e.path.clone())
        .collect();
    let f = secrets::scan(&paths, &repo.diff_staged().unwrap());
    assert!(f.iter().any(|x| x.path == "conn.sql" && x.line == 2));
    assert!(f.iter().any(|x| x.path == ".env" && x.line == 0));
    let _ = std::fs::remove_dir_all(&d);
}
