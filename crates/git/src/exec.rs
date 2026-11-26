//! Running `git`: one place that builds the command line (arguments are a vector, never a
//! shell string), hides the console window, never waits for a prompt, and stops on a
//! timeout or a cancel.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where to get git when it is not installed.
pub const INSTALL_URL: &str = "https://git-scm.com/downloads";

/// Stops a running command: clone it, hand one copy to `run`, call `cancel` on the other.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// `git` could not be started: not installed, or the configured path is wrong.
    /// Holds what was tried (`git` or the path).
    NotInstalled(String),
    Timeout,
    Cancelled,
    /// git ran and failed; `stderr` is what it said (or its stdout when stderr was empty).
    Failed {
        code: Option<i32>,
        stderr: String,
    },
    Io(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::NotInstalled(p) => write!(f, "git not found ({p})"),
            GitError::Timeout => write!(f, "git timed out"),
            GitError::Cancelled => write!(f, "cancelled"),
            GitError::Failed { stderr, .. } => write!(f, "{}", stderr.trim()),
            GitError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GitError {}

/// What a finished command printed.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub code: Option<i32>,
}

impl Output {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// The git executable and how long a command may run.
#[derive(Debug, Clone)]
pub struct Git {
    /// `None` = `git` from PATH.
    pub exe: Option<PathBuf>,
    pub timeout: Duration,
}

/// Output beyond this is dropped (a diff of a huge generated file).
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

impl Default for Git {
    fn default() -> Self {
        Self {
            exe: None,
            timeout: Duration::from_secs(120),
        }
    }
}

impl Git {
    /// `path` is the Settings value: empty = PATH.
    pub fn with_path(path: &str) -> Self {
        let p = path.trim();
        Self {
            exe: (!p.is_empty()).then(|| PathBuf::from(p)),
            ..Self::default()
        }
    }

    pub fn program(&self) -> String {
        self.exe
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "git".to_string())
    }

    /// The full argument list of a command: fixed config first, then `args`.
    /// `core.quotepath=false` keeps non-ASCII names (CJK) as they are.
    pub fn full_args(args: &[String]) -> Vec<String> {
        let mut v: Vec<String> = ["-c", "core.quotepath=false", "-c", "color.ui=never"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        v.extend(args.iter().cloned());
        v
    }

    fn command(&self, cwd: &Path, args: &[String]) -> Command {
        let mut c = Command::new(self.exe.as_deref().unwrap_or(Path::new("git")));
        c.args(Self::full_args(args))
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Never wait for a username / password on a console nobody sees.
            .env("GIT_TERMINAL_PROMPT", "0")
            // A background status must not take index.lock away from a commit.
            .env("GIT_OPTIONAL_LOCKS", "0")
            // File names are names, not patterns.
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GCM_INTERACTIVE", "never");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }

    /// Runs git in `cwd` and returns its output when it exits 0.
    pub fn run(
        &self,
        cwd: &Path,
        args: &[String],
        cancel: Option<&Cancel>,
    ) -> Result<Output, GitError> {
        let out = self.run_any(cwd, args, cancel)?;
        if out.code == Some(0) {
            Ok(out)
        } else {
            Err(failed(out))
        }
    }

    /// Like `run` but a non-zero exit is returned as output, not an error.
    pub fn run_any(
        &self,
        cwd: &Path,
        args: &[String],
        cancel: Option<&Cancel>,
    ) -> Result<Output, GitError> {
        let mut child = self.command(cwd, args).spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GitError::NotInstalled(self.program())
            } else {
                GitError::Io(e.to_string())
            }
        })?;
        let out_t = child.stdout.take().map(reader);
        let err_t = child.stderr.take().map(reader);
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {}
                Err(e) => return Err(GitError::Io(e.to_string())),
            }
            if cancel.is_some_and(|c| c.is_cancelled()) {
                kill(&mut child);
                return Err(GitError::Cancelled);
            }
            if start.elapsed() > self.timeout {
                kill(&mut child);
                return Err(GitError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        let stdout = out_t
            .map(|t| t.join().unwrap_or_default())
            .unwrap_or_default();
        let stderr = err_t
            .map(|t| t.join().unwrap_or_default())
            .unwrap_or_default();
        Ok(Output {
            stdout,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            code: status.code(),
        })
    }
}

fn failed(out: Output) -> GitError {
    let msg = if out.stderr.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        out.stderr
    };
    let msg = tidy(&msg);
    GitError::Failed {
        code: out.code,
        stderr: msg,
    }
}

fn kill(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn reader<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_OUTPUT {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
        buf
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_args_put_config_before_the_command() {
        let a = Git::full_args(&["status".into(), "--porcelain=v2".into()]);
        assert_eq!(&a[..2], ["-c", "core.quotepath=false"]);
        assert_eq!(a[a.len() - 2..], ["status", "--porcelain=v2"]);
    }

    #[test]
    fn empty_setting_means_path() {
        assert_eq!(Git::with_path("  ").exe, None);
        assert_eq!(Git::with_path("").program(), "git");
        assert_eq!(
            Git::with_path("C:/Tools/git.exe").exe,
            Some(PathBuf::from("C:/Tools/git.exe"))
        );
    }

    #[test]
    fn a_bad_path_is_not_installed() {
        let g = Git::with_path("Z:/no/such/git.exe");
        let e = g
            .run(&std::env::temp_dir(), &["--version".into()], None)
            .unwrap_err();
        assert!(matches!(e, GitError::NotInstalled(_)), "{e:?}");
    }
}

/// git rewrites progress lines with `\r`; show each line once, as it ended up.
pub fn tidy(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|l| l.rsplit('\r').next().unwrap_or(l).trim_end())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tidy_tests {
    use super::tidy;

    #[test]
    fn keeps_the_last_state_of_a_progress_line() {
        assert_eq!(
            tidy("remote: Counting:  50%\rremote: Counting: 100%, done.\nFrom x\n\n"),
            "remote: Counting: 100%, done.\nFrom x"
        );
        assert_eq!(tidy("a\r\nb\r\n"), "a\nb");
    }
}
