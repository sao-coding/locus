use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=locales");
    embed_sidecar();
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

const SIDECAR_DIR: &str = "../../sidecar";
const SIDECAR_EXE: &str = "../../sidecar/bin/sidecar.exe";
/// What the sidecar is built from: a change here makes `sidecar.exe` stale.
const SIDECAR_SOURCES: [&str; 4] = ["main.go", "go.mod", "go.sum", "internal"];

/// Makes `$OUT_DIR/sidecar.bin` the Go sidecar (`sidecar/bin/sidecar.exe`) if
/// it was built, else an empty file, so `include_bytes!` always compiles.
///
/// Locally, a missing or stale `sidecar.exe` (another build in its hello
/// frame, or sources newer than it) is rebuilt first with `go build`, stamped
/// like `sidecar/build.ps1` does, so a version bump or a Go change needs no
/// manual step. In CI (`CI` set) or with `LOCUS_SIDECAR_BUILD=0` nothing is
/// built: the workflows run `sidecar/build.ps1` themselves, and jobs without
/// Go build without the sidecar. Without Go the file is used as it is.
fn embed_sidecar() {
    let src = Path::new(SIDECAR_EXE);
    println!("cargo:rerun-if-changed={SIDECAR_EXE}");
    for s in SIDECAR_SOURCES {
        println!("cargo:rerun-if-changed={SIDECAR_DIR}/{s}");
    }
    println!("cargo:rerun-if-env-changed=CI");
    println!("cargo:rerun-if-env-changed=LOCUS_SIDECAR_BUILD");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let auto = std::env::var_os("CI").is_none()
        && std::env::var("LOCUS_SIDECAR_BUILD").as_deref() != Ok("0");
    if auto && let Some(why) = stale(src, &version) {
        build_sidecar(&version, &why);
    }

    let out = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("sidecar.bin");
    let bytes = if src.is_file() {
        std::fs::read(src).unwrap()
    } else {
        println!(
            "cargo:warning=sidecar/bin/sidecar.exe not found: building without the embedded Oracle helper"
        );
        Vec::new()
    };
    // Unchanged bytes keep the old file, so the app is not rebuilt for nothing.
    if std::fs::read(&out).ok().as_deref() != Some(&bytes[..]) {
        std::fs::write(&out, &bytes).unwrap();
    }
}

/// Why `exe` must be rebuilt, or `None` when it is current.
fn stale(exe: &Path, version: &str) -> Option<String> {
    let Ok(built) = std::fs::metadata(exe).and_then(|m| m.modified()) else {
        return Some("it is missing".into());
    };
    let newest = SIDECAR_SOURCES
        .iter()
        .filter_map(|s| newest_mtime(&Path::new(SIDECAR_DIR).join(s)))
        .max();
    if newest.is_some_and(|t| t > built) {
        return Some("its sources changed".into());
    }
    match hello_build(exe) {
        Some(b) if b == version => None,
        Some(b) => Some(format!("it is build {b}, the app is {version}")),
        None => Some("it sends no hello frame (built before protocol 2)".into()),
    }
}

/// Latest modification time of a file, or of any file under a folder.
fn newest_mtime(p: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_dir() {
        return meta.modified().ok();
    }
    std::fs::read_dir(p)
        .ok()?
        .filter_map(|e| newest_mtime(&e.ok()?.path()))
        .max()
}

/// The build in `exe --hello` (sidecar/PROTOCOL.md): `[u32 len][u8 21][u32 qid]`,
/// then `u16 protocol`, `u16 len` + build.
fn hello_build(exe: &Path) -> Option<String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--hello");
    for v in ["SIDECAR_HOST", "SIDECAR_PASSWORD"] {
        cmd.env_remove(v);
    }
    let out = cmd.output().ok()?.stdout;
    if out.get(4) != Some(&21) {
        return None;
    }
    let n = u16::from_le_bytes(out.get(11..13)?.try_into().ok()?) as usize;
    String::from_utf8(out.get(13..13 + n)?.to_vec()).ok()
}

fn build_sidecar(version: &str, why: &str) {
    let status = Command::new("go")
        .current_dir(SIDECAR_DIR)
        .args(["build", "-trimpath", "-ldflags"])
        .arg(format!("-s -w -X main.build={version}"))
        .args(["-o", "bin/sidecar.exe", "."])
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => println!(
            "cargo:warning=rebuilding sidecar/bin/sidecar.exe ({why}) failed ({s}); using it as it is"
        ),
        Err(e) => println!(
            "cargo:warning=sidecar/bin/sidecar.exe needs a rebuild ({why}) but Go is not available ({e}); run sidecar/build.ps1"
        ),
    }
}
