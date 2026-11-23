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

The `release` workflow builds the exe, checksum and attestation from the tag.

## Tests

`cargo nextest run --workspace --lib --bins` needs nothing else. The tests in `crates/*/tests` talk to
real servers; CI starts them with the scripts in `scripts/ci/` and you can too:

```powershell
./sidecar/build.ps1                          # sidecar/bin/sidecar.exe
./scripts/ci/setup-mariadb.ps1 -Port 3317    # sets BENCH_DSN; synthetic fixtures unless devenv/fixtures-real/student.tsv exists
./scripts/ci/setup-redis.ps1 -Port 6390      # sets REDIS_URL
./scripts/ci/setup-postgres.ps1 -Port 5433   # sets PG_TEST_* (needs a PostgreSQL install, `-PgBin`)
cargo nextest run --release --workspace --profile integration -E 'kind(test)'
cargo nextest run --release -p db-redis --profile integration --run-ignored ignored-only
./scripts/ci/stop-local.ps1                  # stops what the scripts started
```

- `devenv/fixtures-real/student.tsv` is masked real data and never committed; without it
  `scripts/ci/gen-fixtures.ps1` writes a deterministic synthetic file with the same columns and row count.
- `LOCUS_TEST_SLACK=4` multiplies every latency limit in the timing tests (CI sets it; default 1).
- `PG_TEST_PASSWORD` overrides the Windows credential `Locus/test/postgres` for the PostgreSQL tests.
- The Oracle tests (`oracle_live`, `routines_live`, `structure_live`) are `#[ignore]` and need a live Oracle; CI never runs them.
