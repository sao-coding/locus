fn main() {
    // The strings live with the app (crates/app/locales); both crates embed them.
    println!("cargo:rerun-if-changed=../app/locales");
}
