use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=locales");
    embed_sidecar();
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

/// Makes `$OUT_DIR/sidecar.bin` the Go sidecar (`sidecar/bin/sidecar.exe`) if
/// it was built, else an empty file, so `include_bytes!` always compiles.
fn embed_sidecar() {
    let src = Path::new("../../sidecar/bin/sidecar.exe");
    println!("cargo:rerun-if-changed=../../sidecar/bin/sidecar.exe");
    let out = Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("sidecar.bin");
    if src.is_file() {
        std::fs::copy(src, &out).unwrap();
    } else {
        std::fs::write(&out, b"").unwrap();
        println!(
            "cargo:warning=sidecar/bin/sidecar.exe not found: building without the embedded Oracle helper"
        );
    }
}
