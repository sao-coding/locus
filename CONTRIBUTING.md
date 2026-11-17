# Contributing

Issues are welcome. Open an issue before starting a large pull request.

## Build

Needs Windows, a recent stable Rust, and Go (for the Oracle sidecar).

```powershell
cargo build --release -p app     # target\release\locus.exe
.\sidecar\build.ps1              # sidecar\bin\sidecar.exe (Oracle)
cargo test --workspace --lib --bins
```

Unit tests (`cargo test --workspace --lib --bins`) need nothing. The integration tests under `crates/*/tests` need a local MariaDB, Redis and PostgreSQL: set `BENCH_DSN` (`mysql://user:pass@host:port/db`) for MariaDB, `REDIS_URL` for Redis, and `PG_TEST_HOST` / `PG_TEST_PORT` / `PG_TEST_USER` / `PG_TEST_DB` for PostgreSQL (the password comes from the Windows credential `Locus/test/postgres`). Tests whose server is not configured skip or fail with a message naming the missing setting.

`cargo test --workspace --features dev` / `cargo build -p app --features dev` enable the test hooks (`LOCUS_KEYLOG`, `LOCUS_DEMO_ENV`, `LOCUS_UNLOCK_SECS`, and the bench scenes `locus.exe <scene>` used by the local UI scripts). They are compiled out of normal builds; `LOCUS_UNLOCK_SECS` can only shorten the 15-minute Production unlock. Plain `cargo test` needs no feature.
Do not regenerate `Cargo.lock` and keep the `=` version pins in `Cargo.toml`; `gpui-kit` is a
snapshot and a fresh resolve breaks the build.

## Before a pull request

```powershell
cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings
```

Record user-visible changes with `knope document-change` (`patch` fix, `minor` feature, `major` breaking; while 0.x, `minor` may break things).

## Releasing

```powershell
knope release        # bumps Cargo.toml and updates CHANGELOG.md
git commit -am "chore(release): 0.2.0"; git tag v0.2.0
git push --follow-tags
```

The `release` workflow builds the zip, checksum and attestation from the tag.
