# Contributing

Issues are welcome. Open an issue before starting a large pull request.

## Build

Needs Windows, a recent stable Rust, and Go (for the Oracle sidecar).

```powershell
cargo build --release -p app     # target\release\locus.exe
.\sidecar\build.ps1              # sidecar\bin\sidecar.exe (Oracle)
cargo test --workspace --lib --bins
```

Building the app rebuilds `sidecar\bin\sidecar.exe` by itself when it is missing or stale (its Go sources changed, or its build differs from the workspace version), as long as Go is on `PATH`. `build.ps1` is for building it on its own. CI (`CI` set) and `LOCUS_SIDECAR_BUILD=0` turn this off: the workflows run `build.ps1` themselves.

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

Releases are automated, like changesets (modelled on Tessera's `version.yml` and Arke's release notice):

1. Every user-visible PR adds a changeset (`knope document-change`, committed in `.changeset/`). Mention the issue it fixes as `(#N)` at the end of the text (`(#12, #15)` for several): knope copies the text into CHANGELOG.md and the release notes, and the release notice uses those references.
2. On each push to `main` the `version` workflow looks at `.changeset/`:
   - changesets pending: it runs `knope prepare-release` on the `release` branch and opens or updates the PR `chore(release): x.y.z` (version bump, the new CHANGELOG section as the PR body);
   - none pending: it closes a stale release PR, reads the version in `Cargo.toml`, and if the tag `vx.y.z` does not exist, creates it through the API and calls `release.yml` (a tag pushed with `GITHUB_TOKEN` starts no workflow).
3. Merging the release PR is the decision to release, whatever its merge title. `release.yml` tests and builds the exe (read-only token), then the `publish` job attests it and creates the GitHub Release with `--verify-tag`. The notes come from `.github/release-notes.md` plus the CHANGELOG section (`scripts/ci/release-notes.ps1 -Version x.y.z` previews them). The `virustotal` job appends its links, and the `notice` job comments on the issues the notes reference as `(#N)` (a stable release also closes them; an RC only comments; issues labeled `needs testing` stay open and are asked to try it).

Setup, once: Settings > Actions > General > "Allow GitHub Actions to create and approve pull requests". PRs opened with the default `GITHUB_TOKEN` do not start `ci`; run it on the `release` branch with Actions > ci > Run workflow (`workflow_dispatch`), or add a repository secret `RELEASE_TOKEN` (fine-grained PAT or GitHub App token with Contents and Pull requests write) and the workflow uses it, so `ci` starts by itself.

Retry and manual runs:

- A release that failed after its tag was made: Actions > release > Run workflow with the version (`0.2.0`). `gh release create` fails if the release already exists; delete that release first.
- A release candidate: set the `Cargo.toml` version, tag `v0.2.0-rc.1` by hand (`git tag v0.2.0-rc.1; git push origin v0.2.0-rc.1`), then run `release` with `0.2.0-rc.1`. Versions must be `X.Y.Z` or `X.Y.Z-rc.N`, and `X.Y.Z` must match `Cargo.toml`.
- Notices for an existing release: Actions > release notice (`dry_run` is on by default).
- `knope release` still bumps `Cargo.toml` and CHANGELOG.md locally if you ever need to do it without the PR.

The workflows are checked with `actionlint` and `zizmor` (`.github/zizmor.yml`).

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
