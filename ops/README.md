# CSD Pool Operations Templates

These files are deployment templates for a single-host private beta or a small
public beta with one active pool daemon.

## Layout

```text
/opt/csd-pool/bin/csd-pool-daemon
/opt/csd-pool/bin/csd-pool-workers
/opt/csd-pool/bin/csd-pool-signer
/etc/csd-pool/config.toml
/etc/csd-pool/csd-pool.env
/var/lib/csd-pool
/var/log/csd-pool
/var/backups/csd-pool
```

## Install Binaries

```bash
sudo useradd --system --home /var/lib/csd-pool --shell /usr/sbin/nologin csd-pool
sudo useradd --system --home /var/lib/csd-signer --shell /usr/sbin/nologin csd-signer
cargo build --release --workspace
sudo install -d -o csd-pool -g csd-pool /opt/csd-pool/bin
sudo install -m 0755 target/release/csd-pool-daemon /opt/csd-pool/bin/
sudo install -m 0755 target/release/csd-pool-workers /opt/csd-pool/bin/
sudo install -m 0755 target/release/csd-pool-signer /opt/csd-pool/bin/
```

Before cutting a release candidate, run the local CI mirror:

```bash
ops/bin/csd-pool-ci-local.sh
```

It matches `.github/workflows/ci.yml` and runs `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`,
`cargo check`, static release checks, generated-env preflight, deployment
verification, the local mock-node e2e harness, release archive creation, archive
checksum validation, and release binary verification. When
`CSD_POOL_VERIFY_RELEASE_ARCHIVE` is set, release verification also extracts the
tarball, verifies its `SHA256SUMS`, checks the release manifest and packaged
release-check script, and scans packaged docs for unredacted database URLs or
literal bearer-token examples. Install missing local Rust components with
`rustup component add rustfmt clippy`.
Set `CSD_POOL_CI_FINAL_REVIEW_PACKAGE=/path/to/csd-pool-final-review-*.tar.gz`
when a real or remediation final-review package is available; local CI will run
the final-review verifier and tamper self-test against it.
Local CI always runs `ops/bin/csd-pool-real-env-doctor-self-test.sh`, which
proves the real environment doctor accepts production-shaped launch inputs,
rejects loopback or placeholder launch inputs, rejects example wallet addresses
and unordered payout limits, requires live template/candidate submission mode,
and redacts database passwords in its reports.
After release creation, `ops/bin/csd-pool-live-startup-policy-self-test.sh`
executes the packaged API, daemon, and bridge binaries and proves live mode
fails closed without persistent PostgreSQL or candidate block submission.
Local CI always runs `ops/bin/csd-pool-public-acceptance-self-test.sh`, which
proves public acceptance evidence rejects fixture/example and non-global public
endpoints even when package hashes are valid.
Local CI always runs `ops/bin/csd-pool-evidence-redaction-self-test.sh`, which
constructs tampered real-go-live receipt and public acceptance packages and
proves the intermediate evidence verifiers reject leaked password-bearing URLs
after package hashes are recomputed. Local CI and GitHub Actions also run
`ops/bin/csd-pool-launch-gaps-self-test.sh` to prove launch-gap remediation is
generated for canary accepted-share minimum mismatches. After the release tarball is built, local
CI and GitHub Actions also run `ops/bin/csd-pool-release-archive-self-test.sh`
against that archive to prove packaged documentation redaction rejects a
recomputed tampered release.

For a local PostgreSQL and Redis dependency stack, run:

```bash
ops/bin/csd-pool-dev-env.sh up
```

The helper uses `docker-compose.yml`, starts only dependency services, waits for
PostgreSQL and Redis health checks, generates `/tmp/csd-pool-dev.env`, applies
migrations, and runs `csd-pool-workers check-config` with required env
validation. Use `ops/bin/csd-pool-dev-env.sh status`, `down`, or `reset` to
inspect, stop, or remove the local volumes.

To build a reproducible release directory and archive with checksums, run:

```bash
ops/bin/csd-pool-build-release.sh
```

The script builds `cargo build --release --workspace`, stages all deployable
binaries, ops templates, docs, config examples, and migrations under `dist/`,
then writes `RELEASE-MANIFEST.txt`, `SHA256SUMS`, a `.tar.gz` archive, and an
archive `.sha256` file. Install from the staged `bin/` directory or unpack the
archive on the target host before following the config and systemd steps below.

To install a staged release directory or `.tar.gz` artifact, use:

```bash
sudo ops/bin/csd-pool-install-release.sh dist/csd-pool-<revision>-<timestamp>
```

For a safe rehearsal that does not touch system paths:

```bash
CSD_POOL_INSTALL_ROOT=/tmp/csd-pool-install-root \
  ops/bin/csd-pool-install-release.sh dist/csd-pool-<revision>-<timestamp>
```

To rehearse install, upgrade marker handling, and rollback from an archive in a
temporary root:

```bash
ops/bin/csd-pool-install-release-self-test.sh dist/csd-pool-<revision>-<timestamp>.tar.gz
```

The installer verifies `SHA256SUMS`, copies binaries to `/opt/csd-pool/bin`,
keeps a full release copy under `/opt/csd-pool/releases/<release>`, installs
systemd and HAProxy templates, and creates config/env files only when they do
not already exist. Review `/etc/csd-pool/config.toml` and
`/etc/csd-pool/csd-pool.env` before starting services. It also writes
`/opt/csd-pool/CURRENT_RELEASE` and, on upgrades, moves the prior value to
`/opt/csd-pool/PREVIOUS_RELEASE`. It atomically updates
`/opt/csd-pool/current` on install and rollback; the production signer unit
uses this stable path. It writes `/opt/csd-pool/release.env` from
`RELEASE-MANIFEST.txt`; systemd services load it so `/api/status` and `/health`
report the running release name, revision, and timestamp.

Generate a production env file with fresh bearer tokens:

```bash
sudo CSD_POOL_DATABASE_URL=postgres://csd_pool:<password>@127.0.0.1:5432/csd_pool \
  ops/bin/csd-pool-generate-env.sh /etc/csd-pool/csd-pool.env
```

The generator refuses to overwrite an existing file unless
`CSD_POOL_ENV_FORCE=1` is set. It replaces the placeholder database URL,
`CSD_POOL_OPERATOR_TOKEN`, and `CSD_POOL_SIGNER_TOKEN`, writes with a restrictive
umask, and leaves the rest of the deployment settings from
`ops/env/csd-pool.env.example`.

Run deployment preflight after editing config and env:

```bash
sudo CSD_POOL_ENV_FILE=/etc/csd-pool/csd-pool.env \
  CSD_POOL_PREFLIGHT_CONFIG=/etc/csd-pool/config.toml \
  ops/bin/csd-pool-preflight.sh
```

The preflight sources the env file, rejects world-readable env files, runs
`csd-pool-workers check-config` with `CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1`, and
writes JSON reports under `/tmp` by default. Enable live contract checks only
when the target dependencies are ready:

```bash
CSD_POOL_PREFLIGHT_NODE=1
CSD_POOL_PREFLIGHT_SIGNER=1
CSD_POOL_PREFLIGHT_MIGRATE=1
CSD_POOL_PREFLIGHT_VERIFY=1
```

To roll binaries back to the previous installed release:

```bash
sudo ops/bin/csd-pool-rollback-release.sh
```

The rollback script verifies the target release `SHA256SUMS`, copies its
binaries back into `/opt/csd-pool/bin`, swaps the current/previous markers, and
then expects the operator to restart services and run verification. Pass an
explicit release name to roll back to a specific directory under
`/opt/csd-pool/releases/`.

## Install Config

```bash
sudo install -d -m 0750 -o root -g csd-pool /etc/csd-pool
sudo install -m 0640 -o root -g csd-pool ops/config.private-beta.toml /etc/csd-pool/config.toml
sudo install -m 0640 -o root -g csd-pool ops/env/csd-pool.env.example /etc/csd-pool/csd-pool.env
sudo install -d -m 0750 -o csd-pool -g csd-pool /var/lib/csd-pool /var/log/csd-pool /var/backups/csd-pool
```

Edit `/etc/csd-pool/config.toml` and `/etc/csd-pool/csd-pool.env` before
starting services. Replace all placeholder secrets.

The private-beta config makes the daemon listen only on local ports:

- Stratum backend: `127.0.0.1:33330`
- API backend: `127.0.0.1:8080`

HAProxy exposes public `:3333` and `:80` and forwards to those local backends.

## Install systemd Units

```bash
sudo install -m 0644 ops/systemd/*.service /etc/systemd/system/
sudo install -m 0644 ops/systemd/*.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now csd-pool-daemon.service
sudo systemctl enable --now csd-pool-signer.service
sudo systemctl enable --now \
  csd-pool-reconcile-blocks.timer \
  csd-pool-rewards.timer \
  csd-pool-payout-create.timer \
  csd-pool-payout-sign.timer \
  csd-pool-payout-submit.timer \
  csd-pool-payout-reconcile.timer \
  csd-pool-monitoring.timer \
  csd-pool-backup.timer
```

Payouts are split into independent timers so a signer or node outage does not
block later reconciliation. Batch creation runs every 30 minutes, signing and
submission run every 2 minutes, and payout reconciliation runs every minute.
The database migration default keeps payouts paused; resume them through the
operator API only after wallet limits and signer isolation are verified.

## Deployment Verification

Run the read-only verification script before enabling public traffic:

```bash
ops/bin/csd-pool-verify.sh
```

By default it checks config files, required systemd service sandbox settings,
HAProxy public binds/backends/rate-limit rules, systemd unit syntax when
`systemd-analyze` is available, placeholder secrets in non-example env files,
`csd-pool-workers check-config`, and the local API health endpoints. Optional
checks are opt-in:

```bash
CSD_POOL_VERIFY_RELEASE=1 \
CSD_POOL_VERIFY_BACKUP=1 \
CSD_POOL_VERIFY_MIGRATE=1 \
CSD_POOL_VERIFY_MOCK_NODE=1 \
CSD_POOL_VERIFY_LOCAL_E2E=1 \
CSD_POOL_VERIFY_SMOKE=1 \
CSD_POOL_VERIFY_LOAD=1 \
CSD_POOL_OPERATOR_TOKEN=<redacted> \
  ops/bin/csd-pool-verify.sh
```

Useful overrides:

```bash
CSD_POOL_BIN_DIR=/opt/csd-pool/bin
CSD_POOL_VERIFY_API_URL=http://127.0.0.1:8080
CSD_POOL_VERIFY_STRATUM_ADDR=127.0.0.1:3333
CSD_POOL_CONFIG=/etc/csd-pool/config.toml
CSD_POOL_ENV_FILE=/etc/csd-pool/csd-pool.env
CSD_POOL_HAPROXY_CONFIG=/etc/haproxy/haproxy.cfg
CSD_POOL_BACKUP_DIR=/var/backups/csd-pool
CSD_POOL_BACKUP_MAX_AGE_DAYS=2
CSD_POOL_BACKUP_MIN_BYTES=1024
CSD_POOL_NETWORK_URL=https://cairn-substrate.com
CSD_POOL_NETWORK_TIMEOUT_SECS=2
CSD_POOL_VERIFY_MOCK_NODE_ADDR=127.0.0.1:18790
CSD_POOL_LOAD_TEST_CLIENTS=100
CSD_POOL_LOAD_TEST_MIN_SUCCESS=100
```

`ops/env/csd-pool.env.example` is allowed to contain `change-me` placeholders.
When `CSD_POOL_ENV_FILE` points at a real env file, `ops/bin/csd-pool-verify.sh`
fails if common placeholder values such as `change-me`, `dev-secret`, or
`replace-me` remain.

## Release Checklist

Before a private beta, public beta, or production rollout, walk through
`ops/RELEASE-CHECKLIST.md` and attach the command outputs to the release notes.
Use `ops/INCIDENT-RUNBOOK.md` during drills and incidents; the public status
page is served at `/status` and its machine-readable summary at `/api/status`.
The static checklist verifier confirms the key files and references are present:

```bash
ops/bin/csd-pool-release-check.sh
```

Before running the final wrapper on the target host, use the real environment
doctor to catch bad launch inputs without starting services:

```bash
CSD_POOL_GO_LIVE_TARGET=public-beta \
CSD_POOL_DOCTOR_OUTPUT_DIR=/var/tmp/csd-pool-real-env-doctor \
  ops/bin/csd-pool-real-env-doctor.sh /etc/csd-pool/csd-pool.env /etc/csd-pool/config.toml
```

It writes `REAL-ENVIRONMENT-DOCTOR.txt` and
`real-environment-doctor-summary.json`; proceed to go-live only when the status
is `ready_for_real_go_live`.

The final go-live gate is intentionally stricter than day-to-day verification:

```bash
sudo CSD_POOL_ENV_FILE=/etc/csd-pool/csd-pool.env \
CSD_POOL_CONFIG=/etc/csd-pool/config.toml \
CSD_POOL_BIN_DIR=/opt/csd-pool/bin \
CSD_POOL_GO_LIVE_TARGET=public-beta \
CSD_POOL_GO_LIVE_API_URL=http://127.0.0.1:8080 \
CSD_POOL_GO_LIVE_STRATUM_ADDR=127.0.0.1:3333 \
CSD_POOL_GO_LIVE_PUBLIC_API_URL=https://pool.example.com \
CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR=pool.example.com:3333 \
  ops/bin/csd-pool-real-go-live.sh
```

`ops/bin/csd-pool-real-go-live.sh` is the preferred real launch wrapper. It
rejects dry-run mode, rejects example env/config paths, requires the installed
`csd-pool-workers` binary, enforces HTTPS public API input for public-beta and
production, runs `ops/bin/csd-pool-real-env-doctor.sh`, runs
`ops/bin/csd-pool-go-live-check.sh`, runs
`ops/bin/csd-pool-verify-go-live-evidence.sh` against the generated archive,
generates `GO-LIVE-SIGNOFF.md` with `ops/bin/csd-pool-generate-signoff.sh`, and
writes `real-go-live-inputs.log`, `REAL-ENVIRONMENT-DOCTOR.txt`,
`real-environment-doctor-summary.json`, `launch-toolchain-manifest.json`,
`real-go-live-postcheck.log`, and `REAL-GO-LIVE-SUMMARY.txt` in
`CSD_POOL_GO_LIVE_REPORT_DIR`.
`real-go-live-inputs.log` proves the wrapper used real env/config paths,
non-dry-run state, executable installed binaries/scripts, and public HTTPS plus
Stratum inputs when required. `launch-toolchain-manifest.json` records the exact
worker binary plus go-live, evidence verifier, signoff, receipt exporter, and
doctor scripts that generated the accepted evidence. The postcheck proves the
accepted archive is not dry-run evidence and that the signoff, report, summary,
archive, and `.sha256` line agree. `REAL-GO-LIVE-SUMMARY.txt` records
`real_go_live_inputs_sha256`, `launch_toolchain_manifest_sha256`,
`real_environment_doctor_report_sha256`,
`real_environment_doctor_summary_sha256`, `go_live_report_sha256`,
`go_live_summary_sha256`, `go_live_signoff_sha256`, and
`evidence_archive_sha256` for release-note attachment. The wrapper then runs
`ops/bin/csd-pool-export-real-go-live-receipt.sh` to create a portable
`csd-pool-*-real-go-live-receipt-*.tar.gz` containing the summary, input report,
launch toolchain manifest, doctor report, postcheck, signoff, go-live reports,
evidence archive, `RECEIPT-MANIFEST.txt`, and `RECEIPT-SHA256SUMS`.
`ops/bin/csd-pool-verify-real-go-live-receipt.sh`
verifies that portable receipt without requiring the original report directory:
it checks the receipt archive hash, internal `RECEIPT-SHA256SUMS`,
input/postcheck proofs, launch toolchain manifest entries and SHA binding,
copied summary/report/signoff hashes,
and public acceptance later requires that launch toolchain proof to appear in
`receipt-verify.log` before accepting external edge evidence. Public acceptance
also packages `acceptance-toolchain-manifest.json`, binding the external
reviewer's acceptance script, receipt verifier, and workers binary to the
submitted acceptance evidence.
doctor ready_for_real_go_live proof, copied summary/report/signoff hashes,
`RECEIPT-MANIFEST.txt` target/source/verifier metadata, and the embedded go-live
evidence archive. Use
`ops/bin/csd-pool-go-live-check.sh` directly for lower-level dry-run or debugging
workflows.

After the operator hands off a real go-live receipt, run an independent public
acceptance pass from outside the pool host or from a clean review machine:

```bash
CSD_POOL_ACCEPTANCE_PUBLIC_API_URL=https://pool.example.com \
CSD_POOL_ACCEPTANCE_PUBLIC_STRATUM_ADDR=pool.example.com:3333 \
CSD_POOL_BIN_DIR=/opt/csd-pool/bin \
  ops/bin/csd-pool-public-acceptance.sh /path/to/csd-pool-public-beta-real-go-live-receipt-20260623T000001Z.tar.gz
```

The acceptance script verifies the portable receipt with
`ops/bin/csd-pool-verify-real-go-live-receipt.sh`, confirms the receipt endpoint
binding matches the requested public API and Stratum target, fetches public
`/health`, `/api/status`, `/api/pool`, and `/api/getting-started`, checks the
getting-started miner command binds the public Stratum address, runs
`stratum-smoke` against the public Stratum edge, runs `stratum-submit-probe` to
exercise the public `mining.submit` response path, verifies the smoke canary
miner through public `/api/miner/<addr20>` and `/api/miner/<addr20>/workers`,
and writes `PUBLIC-ACCEPTANCE-REPORT.txt`, `public-stratum-submit-probe.json`,
`public-canary-miner.json`, `public-acceptance-summary.json`, and
`public-acceptance-evidence.tar.gz`; the summary and report record the verified
receipt archive SHA256 so handoff can bind acceptance to the exact receipt
content. Set `CSD_POOL_ACCEPTANCE_LOAD=1` to add a
public `stratum-load-test` pass. For production launch review, set
`CSD_POOL_ACCEPTANCE_CANARY_ADDRESS` to a real miner address and
`CSD_POOL_ACCEPTANCE_REQUIRE_ACCEPTED_SHARE=1`; the canary miner report then
must show at least `CSD_POOL_ACCEPTANCE_MIN_ACCEPTED_SHARES` accepted shares
through the public API and a fresh `last_seen_ts` within
`CSD_POOL_ACCEPTANCE_CANARY_MAX_AGE_SECONDS`; required accepted-share evidence
must use that configured real miner address, not the automatic smoke-test
worker. Run
`ops/bin/csd-pool-verify-public-acceptance-evidence.sh` against
`public-acceptance-evidence.tar.gz` and its `.sha256` file before accepting the
handoff; the verifier rejects fixture/example public endpoints and requires
`public-endpoint-routability.log` to prove the public API and Stratum hosts
resolved to global public addresses before the artifact can enter handoff. It
also requires `public-acceptance-summary.json.reports` to bind to the standard
package files, requires `public-status-release-binding.log` to prove public
`/api/status` release identity matches the real go-live receipt, and requires
receipt SHA256 metadata so reviewers cannot be pointed at misleading report
paths or a same-named receipt. For the
final delivery review, run
`ops/bin/csd-pool-verify-launch-handoff.sh` with the release archive, the real
go-live receipt, and the public acceptance evidence archive; it verifies release
`SHA256SUMS`, confirms the release manifest records the launch verifiers and
evidence redaction plus release archive self-tests, runs the release package's
own release verifier against the supplied tarball, runs the release package's
own receipt and acceptance verifiers, recomputes the supplied receipt SHA256,
checks the acceptance evidence references the supplied receipt, and cross-checks
public `/api/status` release identity against the `go-live-summary.json` copied
into that receipt. Use
`ops/bin/csd-pool-export-launch-handoff.sh` to bundle the release archive, real
go-live receipt, and public acceptance evidence into a portable
`csd-pool-*-launch-handoff-*.tar.gz` containing `HANDOFF-README.txt`,
`HANDOFF-MANIFEST.txt`, `HANDOFF-SHA256SUMS`, and `handoff-summary.json`;
reviewers should run
`ops/bin/csd-pool-verify-launch-handoff-package.sh` against that final package;
the package verifier requires the summary artifact names and SHA256 values to
match the handoff manifest before rerunning the embedded handoff verification.
The real go-live receipt verifier and public acceptance evidence verifier both
run redaction scans for bearer tokens, secret env assignments, PostgreSQL
password URLs, and URL basic-auth passwords before those intermediate packages
can feed the handoff. `ops/bin/csd-pool-launch-handoff-self-test.sh` exercises
the handoff verifier with matched and mismatched public `/api/status` release
identity so CI catches regressions in that cross-artifact binding.
Then run `ops/bin/csd-pool-audit-launch-readiness.sh` against the same handoff
package. It reuses the package verifier, inspects the embedded real go-live
receipt and public acceptance evidence, rejects fixture/example/placeholder
launch identity values, checks that public acceptance endpoints match the
receipt, requires public acceptance routability evidence for global public
API/Stratum DNS, checks that the canary accepted-share minimum matches the
public acceptance summary, requires the public canary miner to meet that
accepted-share minimum when accepted-share evidence is mandatory, requires the
canary miner `last_seen_ts` to be fresh, requires the canary source to be the
configured real miner when public accepted-share evidence is mandatory, requires
`public_acceptance_toolchain_manifest_verified` so the external reviewer
toolchain binding remains visible in the launch dossier, and writes
`LAUNCH-READINESS-REPORT.txt` plus
`launch-readiness-summary.json`. The audit exits successfully only when the
summary reports `status=launch_ready`; otherwise it reports
`needs_real_environment_evidence` with the missing proof called out. Set
`CSD_POOL_READINESS_REQUIRE_PUBLIC_ACCEPTED_SHARE=1` to make public
accepted-share evidence mandatory for public-beta; production targets require it
automatically.
For the final single-file review package, run
`ops/bin/csd-pool-export-launch-dossier.sh` against the verified handoff
package. It bundles the handoff package, `LAUNCH-READINESS-REPORT.txt`,
`launch-readiness-summary.json`, `launch-dossier-summary.json`,
`DOSSIER-MANIFEST.txt`, and `DOSSIER-SHA256SUMS`; reviewers verify it with
`ops/bin/csd-pool-verify-launch-dossier.sh`. The dossier exporter refuses
non-launchable readiness by default, and the dossier verifier requires the
critical readiness checks, including public acceptance endpoint routability and
public Stratum accepted-share observation plus the public canary accepted-share
minimum, to be present and passed. It also binds `launch-dossier-summary.json` and
`launch-readiness-summary.json` to `DOSSIER-MANIFEST.txt` for the embedded
handoff package name, handoff SHA256, readiness paths, and accepted-share
requirement; CI runs `ops/bin/csd-pool-launch-dossier-self-test.sh` to prove
summary and readiness tampering are rejected. Use
`CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1` only to produce a gap dossier during
remediation review.
The preferred final wrapper is `ops/bin/csd-pool-finalize-launch.sh`: pass the
release archive, real go-live receipt, and public acceptance evidence archive.
It verifies inputs, exports and verifies the handoff package, exports and
verifies the launch dossier, and writes `FINAL-LAUNCH-REPORT.txt` plus
`final-launch-summary.json`. It fails by default unless the dossier is
`launch_ready` and public Stratum accepted-share evidence is present; set
`CSD_POOL_FINAL_REQUIRE_PUBLIC_ACCEPTED_SHARE=0` only for a non-launch rehearsal
that intentionally does not prove an accepted share. Set
`CSD_POOL_FINAL_ALLOW_NON_LAUNCHABLE=1` only when producing a gap package for
remediation.
`final-launch-summary.json` embeds the launch readiness hard-failure count,
public accepted-share requirement/observation status, accepted-share minimums,
canary accepted-share count, canary freshness, and configured canary-source plus
public acceptance toolchain checks;
`ops/bin/csd-pool-verify-final-review.sh` cross-checks those fields against the
embedded dossier readiness summary.
When the final status is `needs_real_environment_evidence`, run
`ops/bin/csd-pool-explain-launch-gaps.sh` with the final output directory,
`final-launch-summary.json`, `launch-readiness-summary.json`, or the launch
dossier archive. It writes `LAUNCH-GAPS-REPORT.txt` plus
`launch-gaps-summary.json`, grouping each failed hard readiness check with the
exact real-environment evidence needed for the next review attempt, including
regenerating public acceptance when the canary accepted-share minimum does not
match the summary.
For reviewer handoff, run `ops/bin/csd-pool-export-final-review.sh` with the
final output directory plus the optional doctor and gaps directories. It creates
`csd-pool-final-review-*.tar.gz` with `FINAL-REVIEW-MANIFEST.txt`,
`FINAL-REVIEW-SHA256SUMS`, the final launch reports, the handoff package, the
launch dossier package, and any doctor/gap reports. Reviewers validate that
single archive with `ops/bin/csd-pool-verify-final-review.sh`; the verifier also
reruns the embedded handoff and launch dossier package verifiers, validates
doctor/gap summary JSON, and requires `launch_ready` reviews to include a
`ready_for_real_go_live` doctor summary. It also binds the handoff and dossier
SHA values in `final-launch-summary.json` to the final review manifest and
embedded packages, binds the release/receipt/public acceptance SHA values to the
embedded handoff manifest, proves the top-level doctor summary matches the copy
inside the embedded real go-live receipt, and runs a final review redaction scan for bearer
tokens, secret env assignments, PostgreSQL password URLs, and URL basic-auth
passwords in top-level review reports. Run `ops/bin/csd-pool-final-review-self-test.sh` against the
final-review package to prove the verifier rejects a recomputed outer archive
after tampering with the final summary handoff SHA or embedded handoff package
path; non-launch-ready packages also get a gap status tamper check, and
launch-ready packages get doctor receipt-binding and embedded dossier
required-readiness-check tamper checks.

`ops/bin/csd-pool-go-live-check.sh` refuses example/template config in real
mode, sources the production env, checks `CSD_POOL_DATABASE_URL`,
`CSD_POOL_OPERATOR_TOKEN`, `CSD_POOL_SIGNER_TOKEN`, `CSD_POOL_SIGNER_URL`,
`CSD_POOL_SIGNER_NODE_URL`, `CSD_POOL_SIGNER_PRIVATE_KEY_FILE`, and
`CSD_POOL_PUBLIC_STRATUM_ADDR`, requires live watch/submit node URLs and a
separate `CSD_POOL_RESTORE_DATABASE_URL`, requires externally scoped public API and
Stratum probe addresses for public-beta/production, then runs preflight with
`CSD_POOL_PREFLIGHT_NODE=1`, `CSD_POOL_PREFLIGHT_SIGNER=1`,
`CSD_POOL_PREFLIGHT_MIGRATE=1`, and `CSD_POOL_PREFLIGHT_VERIFY=1`. It follows
with deployment verification using `CSD_POOL_VERIFY_RELEASE=1`,
`CSD_POOL_VERIFY_BACKUP=1`, `CSD_POOL_VERIFY_MIGRATE=1`,
`CSD_POOL_VERIFY_SMOKE=1`, and `CSD_POOL_VERIFY_LOAD=1`, probes public HTTP
including `/getting-started`, `/api/getting-started`, `/api/metrics`,
`/metrics`, `/api/blocks`, and `/api/payments`, confirms the public Stratum TCP endpoint
can be reached and passes a protocol-level `stratum-smoke`, verifies `/api/status` release metadata matches
`RELEASE-MANIFEST.txt`, verifies runtime `/api/status` reports PostgreSQL-backed
`operational` mode with at least one node health sample, verifies external
public `/api/status` reports the same release and runtime, probes
`CSD_POOL_GO_LIVE_PUBLIC_API_URL` and
`CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR` as the public edge, records public DNS
resolution safety for the public API and Stratum hosts, verifies the release
directory `SHA256SUMS`, records a redacted
`config-snapshot.json` from `check-config`, records a redacted
`env-snapshot.txt`, writes a payout preview report, executes
`ops/bin/csd-pool-restore-drill.sh` against `CSD_POOL_BACKUP_PATH` and
`CSD_POOL_RESTORE_DATABASE_URL`, then probes operator health, alerts, payout
preview, payout status, payout batch CSV export, and payout audit JSON/CSV
export into `http-operator-*` reports. It also writes
`GO-LIVE-REPORT.txt` and
`go-live-summary.json` under
`CSD_POOL_GO_LIVE_REPORT_DIR` with host metadata, release manifest values,
config/env checksums, public endpoints, pass/fail counts, and links to every
command log including `config-snapshot.json`, `env-snapshot.txt`,
`secrets-permissions-safety.log`,
`evidence-redaction-safety.log`,
`real-env-readiness.log`, `clock-safety.log`, `disk-safety.log`,
`bind-safety.log`, `edge-proxy-safety.log`, `database-migration.json`,
`database-migration-safety.log`, `database-runtime.json`, `release-integrity.log`, `status-release-binding.log`,
`runtime-status-binding.log`, `runtime-config-binding.log`,
`metrics-surface-safety.log`, `http-prometheus-metrics.txt`,
`external-public-status-binding.log`, `external-public-config-binding.log`,
`getting-started-binding.log`,
`external-public-getting-started-binding.log`,
`public-dns-safety.log`,
`sample-health.json`, `node-endpoint-safety.log`, `signer-safety.log`,
`payout-limit-safety.log`, `payout-safety.log`, `systemd-runtime-safety.log`,
`runtime-hardening-safety.log`,
`resource-limit-safety.log`,
`service-provenance-safety.log`,
`backup-artifact-safety.log`, `restore-drill.log`, `restore-api-safety.log`,
`restore-http-health.json`, `restore-http-pool.json`,
`restore-http-blocks.json`, `restore-http-payments.json`,
`restore-http-operator-payout-status.json`,
`operator-readiness-safety.log`, the
public/operator HTTP JSON reports,
`http-public-api-status.json`, `http-public-api-getting-started.json`,
`public-api-tls-safety.log`,
`public-api-headers-safety.log`,
`public-api-surface-safety.log`,
`public-operator-auth-boundary.log`,
`http-public-api-pool.json`, `external-public-pool-binding.log`,
`public-stratum-tcp.log`, `public-port-tiers-safety.log`,
`public-port-tiers-smoke.json`, `public-stratum-smoke.json`,
`public-stratum-submit-probe.json`, `public-stratum-load.json`, operator payout CSV exports, and
`stratum-tcp.log`. The same directory is packaged into
`go-live-evidence.tar.gz` plus `go-live-evidence.tar.gz.sha256`; override the basename with
`CSD_POOL_GO_LIVE_EVIDENCE_NAME` when you want release-specific evidence names.
`CSD_POOL_GO_LIVE_DRY_RUN=1` prints the planned commands for release review but
is not a launch pass.

Verify a collected evidence bundle offline before signoff:

```bash
ops/bin/csd-pool-verify-go-live-evidence.sh /path/to/go-live-evidence.tar.gz
ops/bin/csd-pool-generate-signoff.sh /path/to/go-live-evidence.tar.gz GO-LIVE-SIGNOFF.md
ops/bin/csd-pool-verify-real-go-live-summary.sh /path/to/REAL-GO-LIVE-SUMMARY.txt
```

The evidence verifier uses a per-run scratch directory so independent review
jobs can run in parallel. Set `CSD_POOL_EVIDENCE_KEEP_DIR=1` to keep the
extracted archive and intermediate logs, or set `CSD_POOL_EVIDENCE_TMP_DIR` to
place those scratch files under a specific directory. Non-dry-run evidence must
also be fresh: by default `finished_at_utc` in `go-live-summary.json` must be no
older than `CSD_POOL_EVIDENCE_MAX_AGE_HOURS=48`, and cannot be from the future
beyond `CSD_POOL_EVIDENCE_MAX_CLOCK_SKEW_SECONDS=300`.

The verifier checks the `.sha256` file, rejects unsafe archive paths, validates
`EVIDENCE-SHA256SUMS` for every bundled report, validates
`go-live-summary.json`, requires `status=passed` and `fail=0`, verifies evidence
freshness, verifies required
logs including `env-snapshot.txt`, `release-integrity.log`,
`secrets-permissions-safety.log`,
`evidence-redaction-safety.log`,
`real-env-readiness.log`, `database-migration.json`,
`database-migration-safety.log`, `runtime-status-binding.log`,
`external-public-status-binding.log`, `public-dns-safety.log`,
`sample-health.json`, `restore-drill.log`,
and `stratum-tcp.log` are present, requires
`go-live-summary.json`, `GO-LIVE-REPORT.txt`, and `EVIDENCE-MANIFEST.txt` to
agree on status/counts, target, dry-run flag, endpoint identity, and evidence
archive metadata, requires evidence archive metadata consistency, requires
`config-snapshot.json` to validate with `passed=true` and key pool/listen/payout
fields, requires public and operator HTTP reports to validate as JSON, requires
operator payout CSV exports to expose the expected headers, requires the
`node-endpoint-safety.log` report with `config-snapshot.json` and
`check-node-template.json` to prove configured CSD nodes are non-loopback,
non-mock endpoints with passing template, network, and submit health, requires the
`node-runtime.json` report to prove configured CSD nodes meet template, submit,
and watch role quorum, health, network, template-contract, and latency checks,
requires the
`check-signer.json` report and `signer-safety.log` to prove signer `/health.mode`
is present and not `mock`, `dev`, or `test`, and `/health.wallet_address`
matches `CSD_POOL_SIGNER_WALLET_ADDRESS`, requires
`payout-limit-safety.log` plus `config-snapshot.json` to prove positive ordered
payout limits and manual approval below the max batch cap, requires the
`backup-artifact-safety.log` report to prove the selected backup file is present,
fresh, large enough, and has a recorded sha256, requires the
`restore-api-safety.log` report with `restore-drill.log` to prove the restored
database was served through the restore API and operator payout status endpoint,
requires raw restore API JSON reports for `/health`, `/api/pool`, `/api/blocks`,
`/api/payments`, and `/api/operator/payouts/status`, and requires the restored
operator payout status JSON to expose `payouts_enabled`, requires the
`/api/status` release metadata to match the recorded release manifest, requires the
public-beta/production external public status JSON and
`public-dns-safety.log` to prove the public API and Stratum hosts resolve to
global public addresses, `public-api-tls-safety.log` to prove the public API uses
HTTPS with a hostname-valid certificate whose remaining validity meets
`CSD_POOL_PUBLIC_API_TLS_MIN_VALID_DAYS`, `public-api-headers-safety.log` to
prove the public API edge preserves baseline browser security headers,
`public-api-surface-safety.log` to prove public JSON endpoints preserve JSON
content types, safe cache headers, and operator-only field boundaries,
`public-operator-auth-boundary.log` to prove the public edge rejects missing or
invalid operator bearer tokens and accepts the configured token,
`external-public-status-binding.log` to prove the
public edge serves the same release and PostgreSQL runtime,
`external-public-config-binding.log` to prove the public edge serves the same
runtime config, `external-public-pool-binding.log` to prove public `/api/pool`
matches `/api/status` counters and payout settings, `getting-started-binding.log` and
`external-public-getting-started-binding.log` to prove miner setup JSON uses the
configured public Stratum address and port tiers, public Stratum TCP
probe, `public-port-tiers-safety.log` for every enabled public port tier, and
`public-port-tiers-smoke.json` plus `public-stratum-smoke.json` protocol reports
to pass, requires `public-stratum-load.json` to prove concurrent simulated miners
passed through the public Stratum edge, requires
`payout-safety.log` plus operator payout status to prove
payouts are paused before launch, requires `payout-controls-safety.log` to prove
operator payout status, preview, batch list, audit JSON, and CSV exports are
structurally ready, requires the
operator payout status JSON to expose `payouts_enabled`, requires a recorded
release manifest plus release checksum `OK` output, requires the env snapshot to
show `world_readable=false` and required key presence, requires
`secrets-permissions-safety.log` to prove env/config secret files are readable
by owner only with no group/other permissions, requires
`evidence-redaction-safety.log` plus an independent archive scan to prove the
evidence bundle contains no bearer tokens, plaintext secret env values, or
database URLs with passwords, requires
`real-env-readiness.log` to prove PostgreSQL, live adapter, payout RPC, and signer URL presence,
restricted wallet key and supported Node.js runtime, separate restore database,
and production-length operator/signer tokens, requires
`clock-safety.log` to prove the target host clock was synchronized before
collecting launch evidence, requires `disk-safety.log` to prove key target
filesystems had enough free bytes and inodes before evidence collection, requires
`bind-safety.log` to prove internal API, Stratum, and signer listeners remained
loopback-only while public ingress used the configured edge endpoints, requires
`edge-proxy-safety.log` to prove the target HAProxy config validates and maps
public Stratum/API ingress to the reviewed loopback pool backends, requires
`database-migration-safety.log` plus `database-migration.json` to prove every
known schema migration is applied and the database latest version matches the
release code, requires
`database-runtime.json` to prove PostgreSQL connectivity, key table reads,
transaction write/rollback, and query latency are healthy, requires
`node-runtime.json` to prove configured CSD node role quorum and RPC latency are
healthy, requires
`systemd-runtime-safety.log` to prove the daemon, signer, and critical worker
timers are enabled and active, requires `runtime-hardening-safety.log` to prove
the target host loaded daemon/signer service users and systemd hardening
properties, requires `resource-limit-safety.log` to prove the
daemon and signer systemd plus live process open-file limits meet launch
minimums, requires
`service-provenance-safety.log` to prove the running daemon and signer process
binary checksums match the current release `SHA256SUMS`, requires
`runtime-status-binding.log` and `http-api-status.json` to prove PostgreSQL-backed
`operational` runtime with at least one node health sample whose
`latest_sample_at` is no older than `CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES`, requires
`metrics-surface-safety.log` and `http-prometheus-metrics.txt` to prove
Prometheus `/metrics` exposes core pool, Stratum, share validation, payout,
node health, signer health, and freshness samples, requires
`runtime-config-binding.log` to prove `/api/status.config` matches the go-live
`config-snapshot.json`, requires
`operator-readiness-safety.log` with operator health and alert JSON to prove node
and signer samples are healthy and active alerts are empty, requires
`restore-drill.log` to show `restore drill complete`, requires `stratum-tcp.log`
to contain `connected` for real launch evidence, and rejects dry-run evidence
unless `CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1` is explicitly set for rehearsal.
The signoff generator runs the same verifier before writing
`GO-LIVE-SIGNOFF.md`, then summarizes target, release, endpoints, archive hash,
verification status, and critical evidence-file presence for launch review.
`ops/bin/csd-pool-verify-real-go-live-summary.sh` is the final offline receipt
check: it validates every path and sha256 recorded in
`REAL-GO-LIVE-SUMMARY.txt`, confirms `real-go-live-inputs.log` recorded
`real_go_live_inputs_ok=True` with non-example env/config paths and non-dry-run
state, confirms `real-go-live-postcheck.log` recorded
`real_go_live_postcheck_ok=True`, confirms target consistency across the summary,
inputs report, postcheck, and `go-live-summary.json`, and reruns the evidence
verifier on the referenced archive. It also requires the doctor summary to show
`ready_for_real_go_live` with zero hard failures for the same target.
`ops/bin/csd-pool-export-real-go-live-receipt.sh` should be used whenever the
receipt needs to be regenerated from a verified `REAL-GO-LIVE-SUMMARY.txt`.
`ops/bin/csd-pool-verify-real-go-live-receipt.sh` is the handoff check for a
received receipt archive.

Before enabling `CSD_POOL_TEMPLATE_MODE=live`, run the live adapter contract
check from the target host:

```bash
CSD_POOL_CONFIG=/etc/csd-pool/config.toml csd-pool-workers check-node-template
```

The command exits non-zero if `/api/rpc/mining/template` cannot be fetched or
parsed into the Stratum `PoolJob` shape. Its JSON output also records `/health`,
`/api/network`, and submit-node health so operators can attach the result to the
release notes.

Before public-beta/production signoff, run the multi-node runtime gate as well:

```bash
CSD_POOL_CONFIG=/etc/csd-pool/config.toml csd-pool-workers check-node-runtime
```

The report enforces `CSD_POOL_NODE_RUNTIME_MIN_TEMPLATE_NODES`,
`CSD_POOL_NODE_RUNTIME_MIN_SUBMIT_NODES`,
`CSD_POOL_NODE_RUNTIME_MIN_WATCH_NODES`, and the configured health, network, and
template latency thresholds.

For deterministic release-candidate checks without a live CSD node, the verifier
can start the local mock adapter and run the same worker contract check:

```bash
CSD_POOL_VERIFY_HTTP=0 CSD_POOL_VERIFY_MOCK_NODE=1 ops/bin/csd-pool-verify.sh
```

The mock check writes `/tmp/csd-pool-mock-node.log` and
`/tmp/csd-pool-mock-node-template-check.json`, then stops the mock process.

For an approved destructive protocol canary, run
`csd-pool-workers mine-node-candidate-canary` with
`CSD_POOL_NODE_CANDIDATE_CANARY_CONFIRM=mine-and-submit`. Template and submit
URLs must be the same adapter node. The command parallel-searches the advertised
network target, submits the reconstructed candidate, and fails unless the node
reports the block canonical with at least one confirmation. Local E2E uses an
explicitly easy mock target; the recorded official-node run is under
`docs/validation/official-node-candidate-canary-20260710.md`.

For a complete local smoke harness, run:

```bash
CSD_POOL_E2E_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool \
  ops/bin/csd-pool-local-e2e.sh
```

It starts `csd-pool-mock-node`, `csd-pool-signer`, and `csd-pool-daemon` in
live-template mode; checks `/health`, `/api/status`, `/api/pool`,
`/getting-started`, `/api/getting-started`, `/api/history`, and `/api/metrics`;
runs `stratum-smoke`; verifies the template, solved-candidate canary, and signer
contract reports; runs `reward-dry-run` plus `payout-dry-run`; and cleans up the child processes. The
deployment verifier can include the same harness with
`CSD_POOL_VERIFY_LOCAL_E2E=1`.
The live-template daemon requires real PostgreSQL persistence and candidate
submission during this E2E.

Before resuming payouts, verify the isolated signer contract from the payout
worker host:

```bash
CSD_POOL_CONFIG=/etc/csd-pool/config.toml csd-pool-workers check-signer
```

The command calls signer `/health`, posts a one-output, 546-base-unit dust-safe
`contract-check-*` request back to the signer wallet, validates the returned
official CSD `node_tx`, signed input scripts, exact output and `txid`, and exits
without broadcasting anything. For real
go-live evidence, signer `/health.mode` must identify a wallet-backed
non-mock mode; `mock`, `dev`, and `test` modes are rejected. Its
`/health.wallet_address` must match `CSD_POOL_SIGNER_WALLET_ADDRESS`, and
`node_tx_present`, `node_tx_valid`, and `node_tx_outputs_match_request` must all
be true. The bundled legacy raw mock response cannot be misreported as a
production wallet signer; `raw_tx_mock_prefix_present` remains diagnostic only.

The production `csd-pool-signer.service` runs the packaged Node.js wallet
sidecar at `/opt/csd-pool/current/ops/wallet-signer/signer.mjs`, using pinned
official CSD SDK packages included in the release archive. Install Node.js 18+
on the signer host and provision the private key separately:

```bash
sudo install -o csd-signer -g csd-signer -m 0600 /secure/source/csd-wallet.key \
  /etc/csd-pool/signer-wallet.key
sudo systemctl enable --now csd-pool-signer.service
```

Set `CSD_POOL_SIGNER_NODE_URL` to the direct official CSD RPC base used for UTXO
lookup and input-value verification. Startup fails when the key is invalid,
too broadly readable, or does not derive `CSD_POOL_SIGNER_WALLET_ADDRESS`.
Signing is globally serialized by PostgreSQL and only one `signed` or
`submitted` payout batch may be in flight; `sign-payouts` reports
`blocked_by_inflight_batch` while the prior transaction awaits confirmation.
Set `CSD_POOL_PAYOUT_NODE_URL` to a direct official CSD RPC base as well;
`submit-payouts` sends `{ "tx": node_tx }` to its `/tx/submit` endpoint.
`CSD_POOL_SUBMIT_NODE_URL` remains the mining adapter used for structured block
candidate submission. Transport errors and ambiguous node responses keep funds
locked and the payout in `signed`/`submitted` for retry or reconciliation;
operators must investigate before cancelling an unresolved signed payout.

## Install HAProxy

```bash
sudo install -m 0644 ops/haproxy/haproxy.cfg /etc/haproxy/haproxy.cfg
sudo haproxy -c -f /etc/haproxy/haproxy.cfg
sudo systemctl reload haproxy
```

`ops/bin/csd-pool-verify.sh` checks that HAProxy keeps public Stratum on
`:3333`, public HTTP on `:80`, the local Stratum backend on `127.0.0.1:33330`,
the local API backend on `127.0.0.1:8080`, the Stratum per-IP connection cap,
and the API `/health` check. If `haproxy` is installed, it also runs
`haproxy -c -f`.

For HTTPS, put Caddy or nginx in front of the API backend and keep CSD RPC,
PostgreSQL, and the signer on private interfaces.

## Accounting Export

```bash
CSD_POOL_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool \
  /opt/csd-pool/bin/csd-pool-workers accounting-export /var/backups/csd-pool/ledger.csv
```

The export is append-only ledger oriented and includes reward, fee, lock,
unlock, payout, and orphan reversal rows.

For release verification, set `CSD_POOL_VERIFY_BACKUP=1`. The verifier checks
`CSD_POOL_BACKUP_DIR` for a `.dump` newer than `CSD_POOL_BACKUP_MAX_AGE_DAYS`
and larger than `CSD_POOL_BACKUP_MIN_BYTES`.

## Stratum Smoke Test

Run this after the daemon and HAProxy are up:

```bash
CSD_POOL_SMOKE_CLIENTS=100 \
  /opt/csd-pool/bin/csd-pool-workers stratum-smoke 127.0.0.1:3333
```

`stratum-smoke`, `stratum-submit-probe`, `stratum-accepted-share-probe`, and
`stratum-load-test` always print a JSON report. They exit non-zero when required
checks fail, which is why `ops/bin/csd-pool-verify.sh` can use them as release
gates. Local e2e runs the accepted-share probe against a static/easy daemon and
verifies the accepted share through `/api/miner/<addr20>`.

The command simulates miners through `mining.subscribe` and
`mining.authorize`, waits for difficulty and job notifications, and prints JSON
with success count, failure count, and latency. For abuse-guard drills, add
`CSD_POOL_SMOKE_MALFORMED=1` to send one invalid JSON frame per connection.

## Restore Drill

```bash
createdb csd_pool_restore
CSD_POOL_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool_restore \
CSD_POOL_RESTORE_CONFIRM=restore \
  /opt/csd-pool/bin/csd-pool-workers restore-db /var/backups/csd-pool/csd_pool.dump
```

Point `csd-pool-api` at the restore database and compare `/api/pool`,
`/api/blocks`, `/api/payments`, and operator payout status before declaring the
backup valid.

The restore drill wrapper prints a safe dry-run plan unless explicitly
confirmed:

```bash
CSD_POOL_BACKUP_PATH=/var/backups/csd-pool/csd_pool.dump \
CSD_POOL_RESTORE_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool_restore \
  ops/bin/csd-pool-restore-drill.sh
```

To execute the restore and run migrations against the restore database:

```bash
CSD_POOL_BACKUP_PATH=/var/backups/csd-pool/csd_pool.dump \
CSD_POOL_RESTORE_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool_restore \
CSD_POOL_RESTORE_DRILL_CONFIRM=restore-drill \
  ops/bin/csd-pool-restore-drill.sh
```

For go-live evidence, set `CSD_POOL_RESTORE_API_URL=http://127.0.0.1:8081` so the
restore API's `/health`, `/api/pool`, `/api/blocks`, and `/api/payments`
endpoints are probed after restore. Set `CSD_POOL_OPERATOR_TOKEN`; launch
evidence also requires the restore API operator payout status probe. When the
go-live wrapper runs the drill it sets `CSD_POOL_RESTORE_REPORT_DIR`, so those
restore API responses are archived as `restore-http-*.json` evidence files.
