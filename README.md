# CSD Mining Pool Design

This repository starts as an architecture note for building a CSD mining pool
similar in operating shape to Pearlhash or Herominers.

## Recommendation

Use **Yamaduo / `dangraagu/CSD-Mining-pool-public` as the first CSD-specific
reference**, and **Miningcore as the mature pool architecture reference**.

Yamaduo matters because it is already a live CSD pool shape:

- public pool page: `https://pool.yamaduo.no/`
- Stratum endpoint: `pool.yamaduo.no:3333`
- miner source: `https://github.com/dangraagu/CSD-Mining-pool-public`
- miner uses Stratum v1 methods: `mining.subscribe`, `mining.authorize`,
  `mining.set_difficulty`, `mining.notify`, `mining.submit`
- dashboard API shape: `/api/pool`, `/api/metrics`, `/api/history`,
  `/api/miner/:address`

Miningcore still matters because it already has mature pieces a production pool
needs:

- Stratum server and miner session management
- Variable share difficulty
- Share accounting
- Block candidate tracking
- Payment processor
- PostgreSQL persistence
- Stats API and WebSocket event stream

CSD still needs a custom adapter because current CSD operations are closer to
solo mining with local `csd-cuda-search` workers than a normal Bitcoin-style
pool. The missing production boundary is a stable pool work protocol:

- pool creates block templates
- miners search assigned nonce ranges
- miners submit shares and candidate blocks
- pool validates shares cheaply
- pool submits valid blocks to CSD nodes
- pool accounts rewards and pays miners

See:

- [docs/architecture.md](docs/architecture.md) for the technical architecture.
- [docs/product-design.md](docs/product-design.md) for the product design.
- [docs/protocol-spec.md](docs/protocol-spec.md) for Stratum/CSD protocol
  compatibility.
- [docs/api-spec.md](docs/api-spec.md) for public dashboard/API contracts.
- [docs/data-model.md](docs/data-model.md) for persistence and accounting.
- [docs/implementation-plan.md](docs/implementation-plan.md) for milestones and
  build order.

## Current CSD Facts From Prior Operations

- Chain: Compute Substrate / CSD, Bitcoin-derived UTXO PoW chain
- Target block time: about 120 seconds
- Current block reward: 50 CSD
- Value unit: 1 CSD = 100,000,000 base units
- Public network telemetry: `https://cairn-substrate.com/dashboard.html`
- Public network API: `https://cairn-substrate.com/api/network`
- Public RPC proxy: `https://cairn-substrate.com/api/rpc`
- Local node RPC observed in ops: `http://127.0.0.1:8790`
- Local mining logs contain `cuda_round_completed ... hps=...`
- Existing internal fleet has run as same-wallet solo mining, not true pooled
  mining.

Set `CSD_POOL_NETWORK_URL=https://cairn-substrate.com` for `/api/pool` to fill
`network_hashrate_hs` from public Cairn telemetry. The request is short-timeout
and fails open so the dashboard stays available during telemetry outages.
Public telemetry requests do not reuse `CSD_POOL_NODE_TOKEN`; a protected
telemetry endpoint must use the separate `CSD_POOL_NETWORK_TOKEN`.
`pool_hashrate_hs` is estimated from accepted Stratum share difficulty over the
current process runtime; it starts at 0 until enough accepted-share timing data
exists.

## Suggested Build Order

1. **Reverse/read Yamaduo protocol**
   Treat `CSD-Mining-pool-public` as the compatibility target. Confirm the
   Stratum 9-tuple mapping, coinbase/extranonce handling, share difficulty, and
   payout API.

2. **Build compatible CSD pool server**
   Implement our own Stratum server that accepts the public CSD miner without
   requiring miner-side changes.

3. **Production pool**
   Harden vardiff, anti-cheat, payout, DDoS controls, block confirmation logic,
   and miner-facing pages before opening to external users.

## Current Implementation

The repository now includes a Rust workspace with the first pool building
blocks:

```text
crates/csd-pool-protocol   Stratum JSON-RPC method parsing/serialization
crates/csd-pool-consensus  CSD coinbase/header/hash helpers
crates/csd-pool-bridge     Minimal Stratum TCP bridge skeleton
crates/csd-pool-api        Public API skeleton for dashboard endpoints
crates/csd-pool-config     TOML config model and defaults
crates/csd-pool-daemon     Unified API + Stratum process with shared state
crates/csd-pool-db         Migration registry
crates/csd-pool-mock-node  Local CSD adapter contract server for integration checks
crates/csd-pool-node       CSD node client and template provider abstraction
crates/csd-pool-state      Runtime worker/share/block counters
crates/csd-pool-accounting PPLNS allocation and payout selection logic
crates/csd-pool-workers    Reward/payout worker dry-run commands
migrations/                PostgreSQL schema migrations
ops/                       systemd, HAProxy, env, and restore-drill templates
```

Run tests:

```bash
cargo test --workspace
```

Run the local CI gate before a release candidate:

```bash
ops/bin/csd-pool-ci-local.sh
```

It mirrors the GitHub Actions workflow in `.github/workflows/ci.yml`: format,
clippy, tests, check, static release gates, generated-env preflight, deployment
verification, local mock-node e2e, release build, archive checksum, and release
binary verification. When `CSD_POOL_VERIFY_RELEASE_ARCHIVE` points at a release
tarball, `ops/bin/csd-pool-verify.sh` also extracts that archive offline,
verifies its `SHA256SUMS`, confirms the release manifest carries the release
check, and runs a package documentation redaction scan.
When a real or remediation final-review package is available, set
`CSD_POOL_CI_FINAL_REVIEW_PACKAGE=/path/to/csd-pool-final-review-*.tar.gz`; the
CI script then runs both `ops/bin/csd-pool-verify-final-review.sh` and
`ops/bin/csd-pool-final-review-self-test.sh` against that package.
The default CI also runs `ops/bin/csd-pool-evidence-redaction-self-test.sh`,
which builds tampered receipt and public acceptance evidence packages and proves
their verifiers reject leaked PostgreSQL password URLs and URL basic-auth
passwords even when package hashes are recomputed. It also runs
`ops/bin/csd-pool-real-env-doctor-self-test.sh`, proving the doctor accepts
production-shaped launch inputs, rejects loopback/placeholder inputs, and
redacts database passwords in reports. CI also runs
`ops/bin/csd-pool-public-acceptance-self-test.sh`, proving public acceptance
evidence rejects fixture/example and non-global public endpoints even when
package hashes are valid, and runs `ops/bin/csd-pool-launch-gaps-self-test.sh`
to prove launch-gap remediation is generated for canary accepted-share minimum
mismatches. After building a release, CI runs `ops/bin/csd-pool-release-archive-self-test.sh`
against the release tarball to prove package documentation redaction rejects a
recomputed tampered archive.

Start local PostgreSQL and Redis dependencies for development:

```bash
ops/bin/csd-pool-dev-env.sh up
```

This uses `docker-compose.yml`, waits for both services, generates
`/tmp/csd-pool-dev.env`, applies migrations, and runs `check-config` with env
validation. Stop with `ops/bin/csd-pool-dev-env.sh down`; remove local volumes
with `ops/bin/csd-pool-dev-env.sh reset`.

Deployment templates live in [ops/README.md](/Users/edman_openclaw/Documents/csd_pool/ops/README.md).
They cover release binary installation, `/etc/csd-pool` configuration,
systemd services/timers, HAProxy fronting, deployment verification, and backup
restore drills for a single-host private/public beta. The release gate is
tracked in [ops/RELEASE-CHECKLIST.md](/Users/edman_openclaw/Documents/csd_pool/ops/RELEASE-CHECKLIST.md)
and can be sanity-checked with `ops/bin/csd-pool-release-check.sh`.
The final live-readiness gate is `ops/bin/csd-pool-go-live-check.sh`; run it on
the target host after config, services, backups, and public endpoints are ready.
It forces real env/config files, live node and signer contracts, migrations,
backup freshness, HTTP probes including `GET /getting-started` and
`GET /api/getting-started`, `GET /api/metrics`, `GET /api/blocks`, and
`GET /api/payments`, operator API probes for health, alerts, payout preview,
payout status, payout batch CSV export, and payout audit JSON/CSV export, local
and external public Stratum TCP plus protocol smoke connectivity, external public API reachability,
`/api/status` release metadata matching the installed `RELEASE-MANIFEST.txt`,
runtime `/api/status` binding that proves PostgreSQL-backed operational mode
with node health samples, external public `/api/status` binding that proves the
public edge is serving the same release and runtime, public DNS safety proving
the configured public API and Stratum hosts resolve to global public addresses,
Stratum smoke/load tests,
release artifact checksum verification, a backup artifact safety report proving
the selected backup exists, is fresh, meets the minimum size, and has a recorded
sha256, restore drill execution from the latest backup, a payout preview safety
report, and a hard check that payouts are paused before launch signoff. It also records a real environment readiness report that
requires PostgreSQL, live watch/submit node URLs, signer URL, a private official
SDK node URL, an existing restricted 32-byte signer key, Node.js 18+, separate
restore database URL, and production-length operator/signer tokens, plus a clock safety
report proving the target host clock was synchronized, and a disk safety report
proving key runtime, report, backup, and optional PostgreSQL data filesystems
have enough free space and inodes, plus a bind safety report proving internal
API, Stratum, and signer listeners remain loopback-only while public ingress
uses the configured edge endpoints. It records a redacted
`config-snapshot.json` from `check-config` plus a
redacted `env-snapshot.txt` with env file permissions and required-key presence,
then writes `GO-LIVE-REPORT.txt` and
`go-live-summary.json` under `CSD_POOL_GO_LIVE_REPORT_DIR` so the exact release,
host, config checksums, public endpoints, and pass/fail counts can be attached
to launch notes. It also packages the full report directory as
`go-live-evidence.tar.gz` with a matching `.sha256` file.

Build a deployable release archive with:

```bash
ops/bin/csd-pool-build-release.sh
```

It stages release binaries, ops templates, docs, migrations, `SHA256SUMS`, and
`RELEASE-MANIFEST.txt` under `dist/`, then creates a `.tar.gz` archive plus an
archive checksum.

Install a staged release or archive with:

```bash
ops/bin/csd-pool-install-release.sh dist/csd-pool-<revision>-<timestamp>
```

Use `CSD_POOL_INSTALL_ROOT=/tmp/csd-pool-install-root` for a safe staging
install rehearsal before touching `/opt/csd-pool`, `/etc/csd-pool`, or systemd.
For a complete temporary install, upgrade, and rollback rehearsal against a
release archive, run
`ops/bin/csd-pool-install-release-self-test.sh dist/csd-pool-<revision>-<timestamp>.tar.gz`.
Installed releases maintain `CURRENT_RELEASE` and `PREVIOUS_RELEASE` markers
plus an atomic `/opt/csd-pool/current` symlink, so the wallet signer resolves
the same version selected by install or rollback. The rollback script can
restore the prior binary set after a failed rollout with
`ops/bin/csd-pool-rollback-release.sh`. The install script also
writes `/opt/csd-pool/release.env`,
which systemd services load to expose `CSD_POOL_RELEASE_NAME`,
`CSD_POOL_RELEASE_REVISION`, and `CSD_POOL_RELEASE_TIMESTAMP_UTC` through
`/api/status` and `/health` for launch evidence.
Generate a production env file with fresh operator and signer tokens using
`ops/bin/csd-pool-generate-env.sh`.

Before enabling services or public traffic, run the deployment preflight:

```bash
CSD_POOL_ENV_FILE=/etc/csd-pool/csd-pool.env \
CSD_POOL_PREFLIGHT_CONFIG=/etc/csd-pool/config.toml \
  ops/bin/csd-pool-preflight.sh
```

The preflight sources the env file, checks restrictive permissions, runs
`csd-pool-workers check-config` with required env validation enabled, and can
optionally add live node/signer/migration checks with `CSD_POOL_PREFLIGHT_NODE=1`,
`CSD_POOL_PREFLIGHT_SIGNER=1`, and `CSD_POOL_PREFLIGHT_MIGRATE=1`.

Before the final go-live wrapper, run the real environment doctor. It does not
start services; it validates that the env/config are real files, required
go-live variables are present, tokens are production length, public API/Stratum
endpoints are globally routable, CSD node endpoints are non-loopback, and the
restore database is separate. It also requires live template mode and candidate
submission, loopback-only service listeners, non-example 40-hex mining and
signer wallet addresses, and positive payout limits ordered as
`minimum <= manual approval < max batch <= max daily`:

```bash
CSD_POOL_GO_LIVE_TARGET=public-beta \
CSD_POOL_DOCTOR_OUTPUT_DIR=/var/tmp/csd-pool-real-env-doctor \
  ops/bin/csd-pool-real-env-doctor.sh /etc/csd-pool/csd-pool.env /etc/csd-pool/config.toml
```

It writes `REAL-ENVIRONMENT-DOCTOR.txt` and
`real-environment-doctor-summary.json`; the expected status before go-live is
`ready_for_real_go_live`.

The release binaries also fail closed in `live` mode: API, daemon, and bridge
startup require persistent PostgreSQL, and the bridge requires candidate block
submission. `ops/bin/csd-pool-live-startup-policy-self-test.sh` executes the
packaged binaries and proves those unsafe startup combinations exit before
serving miners or HTTP traffic.

Before routing miners to the pool, run the go-live gate:

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

`ops/bin/csd-pool-real-go-live.sh` is the preferred target-host launch entry:
it refuses `CSD_POOL_GO_LIVE_DRY_RUN=1`, requires real env/config files and an
installed binary directory, runs `ops/bin/csd-pool-real-env-doctor.sh` as a
preflight gate, runs `ops/bin/csd-pool-go-live-check.sh`, immediately verifies
the produced evidence with `ops/bin/csd-pool-verify-go-live-evidence.sh`,
generates a reviewer-facing `GO-LIVE-SIGNOFF.md` with
`ops/bin/csd-pool-generate-signoff.sh`, and writes
`real-go-live-inputs.log`, `REAL-ENVIRONMENT-DOCTOR.txt`,
`real-environment-doctor-summary.json`, `launch-toolchain-manifest.json`, and
`REAL-GO-LIVE-SUMMARY.txt` next to the evidence archive.
`real-go-live-inputs.log` records the real env/config path checks,
binary/script hashes, non-dry-run state, and public HTTPS/Stratum launch inputs.
`launch-toolchain-manifest.json` binds the accepted evidence to the exact
worker binary plus go-live, evidence verifier, signoff, receipt exporter, and
doctor scripts used on the target host.
The wrapper also exports a `csd-pool-*-real-go-live-receipt-*.tar.gz` receipt
that bundles the summary, input report, launch toolchain manifest, doctor
report, postcheck, signoff, go-live reports, and evidence archive with
`RECEIPT-MANIFEST.txt` plus `RECEIPT-SHA256SUMS`.
The receipt is portable: run `ops/bin/csd-pool-verify-real-go-live-receipt.sh`
against the receipt archive and its `.sha256` file to verify the included hashes,
input/postcheck proofs, and embedded evidence archive without relying on the
original host paths. The receipt verifier also checks that
`RECEIPT-MANIFEST.txt` target/source/verifier metadata matches the packaged
summary, verifies the launch toolchain manifest has the required executable
entries and SHA values, and runs a receipt redaction scan for bearer tokens,
secret env assignments, PostgreSQL password URLs, and URL basic-auth passwords
before accepting the package.
After that, a reviewer can run a public acceptance pass from outside the pool
host to prove the handed-off receipt matches the live public edge:

```bash
CSD_POOL_ACCEPTANCE_PUBLIC_API_URL=https://pool.example.com \
CSD_POOL_ACCEPTANCE_PUBLIC_STRATUM_ADDR=pool.example.com:3333 \
CSD_POOL_BIN_DIR=/opt/csd-pool/bin \
  ops/bin/csd-pool-public-acceptance.sh /path/to/csd-pool-public-beta-real-go-live-receipt-20260623T000001Z.tar.gz
```

`ops/bin/csd-pool-public-acceptance.sh` verifies the receipt, checks that the
receipt endpoints match the requested public API and Stratum target, fetches
public JSON endpoints, validates the getting-started Stratum binding, runs
`stratum-smoke`, runs `stratum-submit-probe` to exercise the public
`mining.submit` response path, verifies the smoke canary miner through public
`/api/miner/<addr20>` and `/api/miner/<addr20>/workers`, and exports
`PUBLIC-ACCEPTANCE-REPORT.txt`, `public-stratum-submit-probe.json`,
`public-canary-miner.json`, `public-acceptance-summary.json`, and
`public-acceptance-evidence.tar.gz`; the summary and report also record the
verified receipt archive SHA256 so later handoff checks can bind acceptance
evidence to the exact receipt content, not only its filename. Set
`CSD_POOL_ACCEPTANCE_LOAD=1` when the reviewer also wants public
`stratum-load-test` evidence. For production, point
`CSD_POOL_ACCEPTANCE_CANARY_ADDRESS` at a real miner address and set
`CSD_POOL_ACCEPTANCE_REQUIRE_ACCEPTED_SHARE=1`; the acceptance bundle then
requires that public `/api/miner/<addr20>` reports at least
`CSD_POOL_ACCEPTANCE_MIN_ACCEPTED_SHARES` accepted shares and a fresh
`last_seen_ts` within `CSD_POOL_ACCEPTANCE_CANARY_MAX_AGE_SECONDS`. When
accepted-share evidence is required, the canary source must be the configured
real miner address rather than the automatic smoke-test worker. Run
`ops/bin/csd-pool-verify-public-acceptance-evidence.sh` against
`public-acceptance-evidence.tar.gz` and its `.sha256` file to verify the
acceptance bundle offline; the verifier also rejects fixture/example public
endpoints, requires `public-endpoint-routability.log` to prove the public API
and Stratum hosts resolved to global public addresses, requires the summary
`reports` paths to point at the standard package files, requires
`public-status-release-binding.log` to prove public `/api/status` release
identity matches the real go-live receipt, requires receipt SHA256 metadata, and
runs the same redaction scan across the public acceptance reports. It also
requires `receipt-verify.log` to show the launch toolchain manifest proof from
the receipt verifier, so public acceptance cannot be based on an older receipt
verification pass that skipped target-host toolchain binding. The acceptance
bundle includes `acceptance-toolchain-manifest.json`, which records the external
reviewer's acceptance script, receipt verifier, and workers binary used to probe
the public edge.
The final handoff check is
`ops/bin/csd-pool-verify-launch-handoff.sh`: pass it the release archive, real
go-live receipt, and public acceptance evidence so it can verify the release
checksums, run the release package's own verifier against the supplied release
tarball, confirm the release carries both the evidence redaction and release
archive self-tests, and then use the release package's own verifiers against
both evidence artifacts. It also recomputes the supplied receipt archive SHA256
and compares it with the receipt hash recorded by public acceptance evidence,
then cross-checks public `/api/status` release identity against the
`go-live-summary.json` copied into the real go-live receipt.
Use `ops/bin/csd-pool-export-launch-handoff.sh` to package those
three artifacts into a portable `csd-pool-*-launch-handoff-*.tar.gz` with
`HANDOFF-README.txt`, `HANDOFF-MANIFEST.txt`, `HANDOFF-SHA256SUMS`, and
`handoff-summary.json`; reviewers can run
`ops/bin/csd-pool-verify-launch-handoff-package.sh` against that package for the
single-file delivery check. The package verifier checks that
`handoff-summary.json` agrees with `HANDOFF-MANIFEST.txt` on artifact names and
SHA256 values before rerunning the embedded handoff verification.
Run `ops/bin/csd-pool-audit-launch-readiness.sh` against the verified handoff
package before routing real miners. It emits `LAUNCH-READINESS-REPORT.txt` and
`launch-readiness-summary.json`, rejects fixture/example/placeholder evidence,
checks that public acceptance endpoints match the real go-live receipt, requires
public acceptance routability evidence for global public API/Stratum DNS, checks
that the canary accepted-share minimum matches the public acceptance summary, and
requires the public canary miner to meet that accepted-share minimum when
accepted-share evidence is mandatory. It also requires the canary miner
`last_seen_ts` to be fresh and the canary source to be the configured real miner
when public accepted-share evidence is required. It records
`public_acceptance_toolchain_manifest_verified` as a hard check so the launch
dossier proves the external reviewer toolchain was bound in the public
acceptance evidence.
returns success only when the machine-readable status is `launch_ready`.
`CSD_POOL_READINESS_REQUIRE_PUBLIC_ACCEPTED_SHARE=1` makes public accepted-share
evidence a hard readiness gate; production targets enforce that automatically.
The final review artifact is produced with
`ops/bin/csd-pool-export-launch-dossier.sh` from the verified handoff package.
It bundles the handoff archive plus `LAUNCH-READINESS-REPORT.txt`,
`launch-readiness-summary.json`, `launch-dossier-summary.json`,
`DOSSIER-MANIFEST.txt`, and `DOSSIER-SHA256SUMS`; verify it offline with
`ops/bin/csd-pool-verify-launch-dossier.sh`. The dossier verifier requires
launch-ready readiness and checks that required readiness checks, including
public acceptance endpoint routability and public Stratum accepted-share
observation plus the public canary accepted-share minimum, are present and
passed. It also binds
`launch-dossier-summary.json` and `launch-readiness-summary.json` to
`DOSSIER-MANIFEST.txt` for the embedded handoff package name, handoff SHA256,
readiness paths, and accepted-share requirement;
`ops/bin/csd-pool-launch-dossier-self-test.sh` proves summary and readiness
handoff-SHA tampering are rejected.
For the complete final sequence, run `ops/bin/csd-pool-finalize-launch.sh` with
the release archive, real go-live receipt, and public acceptance evidence. It
performs the handoff verification/export, dossier export/verification, and writes
`FINAL-LAUNCH-REPORT.txt` plus `final-launch-summary.json`; by default it fails
unless readiness is `launch_ready` and public Stratum accepted-share evidence is
present. Set `CSD_POOL_FINAL_REQUIRE_PUBLIC_ACCEPTED_SHARE=0` only for a
non-launch rehearsal that intentionally does not prove an accepted share.
The final summary embeds the launch readiness hard-failure count, public
accepted-share requirement/observation status, accepted-share minimums, canary accepted-share count,
canary freshness, configured canary-source, and public acceptance toolchain checks;
the final-review verifier cross-checks those fields against the embedded dossier
readiness summary.
When finalization reports `needs_real_environment_evidence`, run
`ops/bin/csd-pool-explain-launch-gaps.sh` against the final output directory,
`final-launch-summary.json`, `launch-readiness-summary.json`, or the launch
dossier archive. It writes `LAUNCH-GAPS-REPORT.txt` and
`launch-gaps-summary.json` with each missing hard gate mapped to the concrete
real-world evidence to collect next, including regenerating public acceptance
when the canary accepted-share minimum does not match the summary.
For the final single-file reviewer handoff, run
`ops/bin/csd-pool-export-final-review.sh` with the final output directory plus
the optional doctor and gaps directories. It packages `FINAL-LAUNCH-REPORT.txt`,
`final-launch-summary.json`, the handoff package, the launch dossier package,
doctor reports, and gap reports into `csd-pool-final-review-*.tar.gz` with
`FINAL-REVIEW-MANIFEST.txt` and `FINAL-REVIEW-SHA256SUMS`. Reviewers verify that
archive with `ops/bin/csd-pool-verify-final-review.sh`, which also reruns the
embedded handoff and launch dossier verifiers, validates doctor/gap summary
JSON, and requires a `launch_ready` review to include a
`ready_for_real_go_live` doctor summary. It also checks that the handoff and
dossier SHA values recorded in `final-launch-summary.json` match the final
review manifest and embedded packages, binds the release/receipt/public
acceptance SHA values to the embedded handoff manifest, proves the top-level
doctor summary matches the copy inside the embedded real go-live receipt, and
runs a final review redaction scan for bearer tokens, secret env assignments, PostgreSQL password
URLs, and URL basic-auth passwords in top-level review reports. Run
`ops/bin/csd-pool-final-review-self-test.sh` against a final-review package to
prove the verifier rejects a package whose outer SHA files are recomputed after
tampering with the final summary handoff SHA or embedded handoff package path;
for non-launch-ready packages it also proves gap status tampering is rejected,
and for launch-ready packages it proves doctor receipt-binding and embedded
dossier readiness-check tampering are rejected.
Reviewers can rerun
`ops/bin/csd-pool-verify-real-go-live-summary.sh` against that summary to
validate recorded hashes, the real input report, the real-go-live postcheck, and
the referenced evidence archive, including doctor readiness and target
consistency across the summary, inputs report, postcheck, doctor summary, and
`go-live-summary.json`. Use
`ops/bin/csd-pool-go-live-check.sh` directly only when you intentionally need the
lower-level gate.

For `CSD_POOL_GO_LIVE_TARGET=public-beta` or `production`, the public API URL
and public Stratum probe address are required. Production public API evidence
must use HTTPS.

Use `CSD_POOL_GO_LIVE_DRY_RUN=1` only to inspect the command plan in CI or
release review; a real launch must pass without dry-run. Attach
the generated real go-live receipt archive first, then
`REAL-GO-LIVE-SUMMARY.txt`, `GO-LIVE-SIGNOFF.md`, `GO-LIVE-REPORT.txt`,
`go-live-summary.json`, `go-live-evidence.tar.gz`, the `.sha256` file, and the
linked command logs to the launch decision. The evidence bundle
also includes `config-snapshot.json`, `env-snapshot.txt`,
`secrets-permissions-safety.log`,
`evidence-redaction-safety.log`,
`real-env-readiness.log`, `clock-safety.log`, `release-integrity.log`,
`disk-safety.log`,
`bind-safety.log`,
`edge-proxy-safety.log`,
`database-migration.json`, `database-migration-safety.log`,
`database-runtime.json`,
`status-release-binding.log`, `runtime-status-binding.log`,
`runtime-config-binding.log`,
`external-public-status-binding.log`, `external-public-config-binding.log`,
`getting-started-binding.log`,
`external-public-getting-started-binding.log`,
`public-dns-safety.log`,
`sample-health.json`,
`node-endpoint-safety.log`, `signer-safety.log`, `payout-limit-safety.log`,
`payout-safety.log`, `payout-controls-safety.log`, `systemd-runtime-safety.log`,
`runtime-hardening-safety.log`,
`resource-limit-safety.log`,
`service-provenance-safety.log`,
`backup-artifact-safety.log`, `restore-drill.log`, `restore-api-safety.log`,
restore API raw JSON reports such as `restore-http-pool.json` and
`restore-http-operator-payout-status.json`,
`operator-readiness-safety.log`,
`stratum-tcp.log`,
public HTTP JSON reports such as `http-api-pool.json`,
`pool-endpoint-binding.log`, `http-api-metrics.json`, and
`http-api-payments.json`, external public reachability reports such as
`http-public-api-status.json`, `http-public-api-pool.json`,
`external-public-pool-binding.log`, `public-dns-safety.log`,
`http-public-api-getting-started.json`,
`public-api-tls-safety.log`,
`public-api-headers-safety.log`,
`public-api-surface-safety.log`,
`public-operator-auth-boundary.log`,
`public-stratum-tcp.log`, `public-port-tiers-safety.log`,
`public-port-tiers-smoke.json`, `public-stratum-smoke.json`,
`public-stratum-submit-probe.json`, and `public-stratum-load.json`, operator HTTP JSON reports such as
`http-operator-health.json` and `http-operator-payout-preview.json`, operator
CSV reports such as `http-operator-payout-batches.csv` and
`http-operator-payout-audit.csv`, and `EVIDENCE-SHA256SUMS` for per-file
verification inside the archive.
Reviewers can independently verify the evidence bundle with:

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

That verifier checks the archive checksum, required reports, JSON summary,
pass/fail status, evidence freshness, cross-checks `go-live-summary.json`, `GO-LIVE-REPORT.txt`,
and `EVIDENCE-MANIFEST.txt` for matching status/counts, target, dry-run flag,
endpoint identity, and evidence archive metadata,
requires a recorded release manifest plus successful release
`SHA256SUMS` verification, requires `config-snapshot.json` to pass and include
key pool/listen/payout fields, requires `env-snapshot.txt` to show
non-world-readable permissions and required key presence, requires
`secrets-permissions-safety.log` to prove env/config secret files were readable
by owner only with no group/other permissions, requires
`evidence-redaction-safety.log` and an independent archive scan to prove the
evidence bundle contains no bearer tokens, plaintext secret env values, or
database URLs with passwords, requires
`real-env-readiness.log` to prove PostgreSQL, live adapter, payout RPC, and signer URL presence,
restricted wallet key and supported signer runtime, separate restore database,
and production-length operator/signer tokens, requires
`clock-safety.log` to prove the target host clock was synchronized before
collecting launch evidence, requires `disk-safety.log` to prove key target
filesystems had enough free bytes and inodes before evidence collection, requires
`bind-safety.log` to prove internal API, Stratum, and signer listeners remained
loopback-only while public ingress used the configured edge endpoints, requires
`edge-proxy-safety.log` to prove the target HAProxy config validates and maps
public Stratum/API ingress to the reviewed loopback pool backends,
`database-migration-safety.log` plus `database-migration.json` to prove the live
database schema has every known migration applied and its latest applied version
matches the release code,
`database-runtime.json` to prove PostgreSQL connectivity, key table reads,
transaction write/rollback, and query latency are healthy,
`systemd-runtime-safety.log` to prove the daemon, signer, and critical worker
timers are enabled and active, requires `runtime-hardening-safety.log` to prove
the target host loaded daemon/signer service users and systemd hardening
properties, requires `resource-limit-safety.log` to prove the
daemon and signer systemd plus live process open-file limits meet launch
minimums,
timers are enabled and active,
`service-provenance-safety.log` to prove the running daemon and signer process
binary checksums match the current release `SHA256SUMS`,
`node-endpoint-safety.log` plus `config-snapshot.json` and
`check-node-template.json` to prove configured CSD nodes are non-loopback,
non-mock endpoints with passing template, network, and submit health,
`node-runtime.json` to prove all configured CSD nodes pass runtime health,
network, template-contract, role-quorum, and latency thresholds,
`signer-safety.log` plus `check-signer.json` to prove `/health.mode` is present
and not `mock`, `dev`, or `test`, and `/health.wallet_address` matches
`CSD_POOL_SIGNER_WALLET_ADDRESS`,
requires `payout-limit-safety.log` plus `config-snapshot.json` to prove positive
ordered payout limits and a manual approval threshold below the max batch cap,
requires `backup-artifact-safety.log` to prove the selected backup file is
present, fresh, large enough, and has a recorded sha256,
requires `restore-api-safety.log` plus `restore-drill.log` to prove the restored
database was served through the restore API and operator payout status endpoint,
requires restore API raw JSON reports including
`restore-http-operator-payout-status.json`,
`http-api-status.json` release metadata to match the recorded release manifest,
requires `runtime-status-binding.log` plus `http-api-status.json` to prove
PostgreSQL-backed operational runtime with at least one node health sample whose
`latest_sample_at` is no older than `CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES`,
requires `runtime-config-binding.log` to prove `/api/status.config` matches the
go-live `config-snapshot.json`,
requires `operator-readiness-safety.log` plus operator health and alert JSON to
prove node and signer samples are healthy and active alerts are empty,
requires public-beta/production external public status JSON plus
`public-dns-safety.log` to prove the public API and Stratum hosts resolve to
global public addresses, `public-api-tls-safety.log` to prove the public API
uses HTTPS with a hostname-valid certificate whose remaining validity meets
`CSD_POOL_PUBLIC_API_TLS_MIN_VALID_DAYS`, `public-api-headers-safety.log` to
prove the public API edge preserves baseline browser security headers,
`public-api-surface-safety.log` to prove public JSON endpoints preserve JSON
content types, safe cache headers, and operator-only field boundaries,
`public-operator-auth-boundary.log` to prove the external public edge rejects
missing or invalid operator bearer tokens and accepts the configured token,
`external-public-status-binding.log` to prove the
public edge serves the same release and PostgreSQL runtime,
`external-public-config-binding.log` to prove the public edge serves the same
runtime config, `getting-started-binding.log` and
`external-public-getting-started-binding.log` to prove miner setup JSON uses the
configured public Stratum address and port tiers,
`pool-endpoint-binding.log` and `external-public-pool-binding.log` to prove
`/api/pool` matches `/api/status` counters and payout settings, and Stratum TCP plus
`public-port-tiers-safety.log`, `public-port-tiers-smoke.json`, and
`public-stratum-smoke.json` reports to prove enabled public Stratum port tiers
are reachable and speak the mining protocol, with
`public-stratum-load.json` proving the public Stratum edge accepted concurrent
simulated miners through the configured endpoint,
requires payouts to be paused for launch,
the public and operator HTTP reports to validate as JSON, requires
operator payout CSV reports to expose the expected headers, requires
`http-operator-payout-status.json` to expose `payouts_enabled`, requires
`payout-controls-safety.log` to prove operator payout status, preview, batch
list, audit JSON, and CSV exports are structurally ready, requires
`restore-drill.log` to show `restore drill complete`, requires a successful
`connected` Stratum TCP probe in real launch evidence, and rejects dry-run
evidence unless `CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1` is explicitly set for
CI/review rehearsal.

Run the unified pool daemon:

```bash
CSD_POOL_CONFIG=config.example.toml cargo run -p csd-pool-daemon
```

This starts both the Stratum listener and the public API. Miner authorization
and share counters are held in one shared runtime state, so `/api/pool`,
`/api/metrics`, and miner endpoints reflect live Stratum activity inside the
same process.

The bridge assigns `[stratum].initial_difficulty` on authorization and applies
per-session vardiff after accepted shares. Shares faster than half the target
interval double difficulty; shares slower than twice the target interval halve
it, clamped to `[stratum].min_difficulty` and `[stratum].max_difficulty`.
Assigned difficulty is enforced by validating submits against
`base_share_target / ceil(difficulty)` before the share is persisted.

If `CSD_POOL_DATABASE_URL` is set, the Stratum bridge applies migrations on
startup and persists Stratum jobs plus accepted shares to PostgreSQL. Duplicate
shares are guarded both in-session and by the database uniqueness constraint.

The Stratum bridge defaults to a static easy-target development job. To request
live jobs from a CSD node/template adapter, set:

```bash
CSD_POOL_TEMPLATE_MODE=live \
CSD_POOL_CONFIG=config.example.toml \
cargo run -p csd-pool-daemon
```

In live mode the bridge uses `[pool].mining_address` and the first
`[[csd_nodes]]` entry whose `role` includes `template`. You can override those
with `CSD_POOL_MINING_ADDRESS` and `CSD_POOL_NODE_URL`.

The pinned official CSD node does not expose pool mining RPCs by default.
Production deployments must build the authenticated adapter in
`ops/csd-node-adapter`; its manifest fixes the audited upstream commit and patch
checksum. Configure `CSD_POOL_NODE_TOKEN` in the pool and the same value as
`CSD_POOL_ADAPTER_TOKEN` on each private adapter node. The bridge polls for tip
changes and proactively broadcasts clean jobs to every authorized session.

After provisioning an isolated node or approved canary network, the full solved
candidate path can be exercised with `csd-pool-workers
mine-node-candidate-canary`. It will not run unless
`CSD_POOL_NODE_CANDIDATE_CANARY_CONFIRM=mine-and-submit` is set. The command
searches the official network target in parallel, submits the exact coinbase and
84-byte header, then requires the block to become canonical with at least one
confirmation. Do not run it casually against production: a success is a real
mined block submission.

Template and candidate submission must currently resolve to the same adapter
URL because each official node owns its template transaction cache. Live bridge
startup rejects mismatched URLs instead of risking every solved block being
reported as an unknown job.

Candidate block submission is an explicit switch:

```bash
CSD_POOL_SUBMIT_CANDIDATES=true \
CSD_POOL_TEMPLATE_MODE=live \
CSD_POOL_CONFIG=config.example.toml \
cargo run -p csd-pool-daemon
```

When enabled, the bridge posts a structured candidate payload to
`/api/rpc/block/submit` on the first configured CSD node whose role includes
`submit`, or to `CSD_POOL_SUBMIT_NODE_URL` if set. The adapter receives the
84-byte header, hash, exact serialized coinbase, coinbase txid, merkle root,
extranonce2, ntime, and nonce. The official-node adapter validates and commits
the candidate through the chain's native consensus and reorg paths.

Before switching miners to live mode, verify the CSD node/template adapter
contract:

```bash
CSD_POOL_CONFIG=config.example.toml cargo run -p csd-pool-workers -- check-node-template
```

Before public-beta/production signoff, also verify every configured CSD node
and the role quorum:

```bash
CSD_POOL_CONFIG=config.example.toml cargo run -p csd-pool-workers -- check-node-runtime
```

`check-node-runtime` probes each configured `[[csd_nodes]]` entry, requires the
minimum template/submit/watch quorum, verifies health and network RPCs, fetches
templates from template-role nodes, and enforces
`CSD_POOL_NODE_RUNTIME_MAX_*_MS` latency thresholds.

For local contract testing without a real CSD node, start the mock adapter:

```bash
CSD_POOL_MOCK_NODE_LISTEN=127.0.0.1:8790 cargo run -p csd-pool-mock-node
```

It serves `/health`, `/api/network`, `/api/rpc/mining/template`,
`/api/rpc/block/submit`, `/api/rpc/block/status`, `/api/rpc/tx/submit`, and
`/api/rpc/tx/status` using deterministic in-memory state.

`check-node-template` fetches `/health`, `/api/network`, and
`/api/rpc/mining/template?address=<pool_addr20>`, parses the template into the
same `PoolJob` used by Stratum, and emits JSON with endpoint latency, network
telemetry, job id, target hex, coinbase sizes, and submit-node health. The
process exits non-zero when the template cannot be fetched or parsed.

The deployment verifier can run that local contract automatically. This starts
`csd-pool-mock-node`, waits for `/health`, runs `check-node-template` against
it, writes the JSON result to `/tmp/csd-pool-mock-node-template-check.json`,
and stops the mock process:

```bash
CSD_POOL_VERIFY_HTTP=0 CSD_POOL_VERIFY_MOCK_NODE=1 ops/bin/csd-pool-verify.sh
```

For a fuller local e2e pass, run the bundled smoke harness. It starts the mock
CSD node, mock signer, and `csd-pool-daemon` in live-template mode on local-only
ports; checks the public API; runs `stratum-smoke`; verifies `check-node-template`
and signer contract reports; runs `reward-dry-run` and `payout-dry-run`; and
cleans up all child processes:

```bash
CSD_POOL_E2E_DATABASE_URL=postgres://csd_pool:<redacted>@127.0.0.1:5432/csd_pool \
  ops/bin/csd-pool-local-e2e.sh
```

The live-template E2E uses real PostgreSQL persistence and enables candidate
submission; it no longer relies on an ephemeral live mode that production
binaries correctly reject.

The same path can be included in deployment verification:

```bash
CSD_POOL_VERIFY_HTTP=0 CSD_POOL_VERIFY_LOCAL_E2E=1 ops/bin/csd-pool-verify.sh
```

Run the bridge skeleton:

```bash
CSD_POOL_STRATUM_LISTEN=127.0.0.1:3333 cargo run -p csd-pool-bridge
```

or with a config file:

```bash
CSD_POOL_CONFIG=config.example.toml cargo run -p csd-pool-bridge
```

The bridge currently handles `mining.subscribe`, `mining.authorize`, and
`mining.submit` framing. It sends a static easy-target test job by default,
can switch to live CSD template jobs, validates submits through the CSD 84-byte
header path, rejects duplicate submit tuples, persists accepted shares when a
database URL is configured, and can submit block candidates when explicitly
enabled.

The bridge also has an in-memory abuse guard for the single-process deployment
path. `[abuse]` controls per-IP active connection caps, malformed JSON frame
limits, per-address active session caps, authorization failure limits, invalid
share limits, and temporary ban duration. The same settings can be supplied
without a config file through `CSD_POOL_MAX_CONNECTIONS_PER_IP`,
`CSD_POOL_MAX_SESSIONS_PER_ADDRESS`, `CSD_POOL_MALFORMED_FRAME_LIMIT`,
`CSD_POOL_AUTH_FAILURE_LIMIT`, `CSD_POOL_INVALID_SHARE_LIMIT`, and
`CSD_POOL_BAN_SECS`.

Run the API skeleton:

```bash
CSD_POOL_API_LISTEN=127.0.0.1:8080 cargo run -p csd-pool-api
```

or with the same config file:

```bash
CSD_POOL_CONFIG=config.example.toml cargo run -p csd-pool-api
```

Current public endpoints:

```text
GET /
GET /getting-started
GET /health
GET /status
GET /metrics
GET /api/getting-started
GET /api/status
GET /api/pool
GET /api/metrics
GET /api/history
GET /api/miner/:address
GET /api/miner/:address/workers
GET /api/blocks
GET /api/payments
```

`/getting-started` is the miner-facing setup page. It reads
`/api/getting-started` to show the public Stratum endpoint, username format,
copyable miner commands, enabled port tiers, and payout rules. Set
`CSD_POOL_PUBLIC_STRATUM_ADDR=pool.example.com:3333` for production display, and
optionally set `CSD_POOL_PUBLIC_PORT_TIERS=3333:standard:8,5555:high:64:disabled`
to publish additional difficulty tiers.

With PostgreSQL configured, `/api/history` serves durable bucketed samples from
accepted `shares` and rejected/stale `share_events`; in plain terms, history is
backed by accepted shares plus rejected/stale share events. `net_hs` uses the
current network telemetry source when `CSD_POOL_NETWORK_URL` is set. Without
PostgreSQL it falls back to a single in-memory runtime sample.
`round_effort_pct` is live when network telemetry is available and is based on
current-round accepted share difficulty.
Found block records persist `effort_pct` when candidate blocks are submitted, so
block history can show found-block effort. `/api/pool` aggregates 24h, 7d, and
lifetime block effort into luck percentages, and the dashboard shows 24h luck on
the blocks KPI. The Recent Blocks table shows finder/worker, status,
confirmations, reward, and per-block effort.
Miner lookup and worker detail use persisted accepted shares plus rejected/stale
share events when PostgreSQL is configured.
The built-in dashboard share activity chart can switch between 12h, 24h, and 7d
views backed by `/api/history`, and falls back to live runtime counters if
history is unavailable.

`GET /` serves the built-in public dashboard. It is a dense operations console
with pool KPIs, share activity, recent blocks, recent payments, and service
health/alert summary space, backed by the public JSON endpoints above. The
dashboard also includes a miner address lookup backed by
`/api/miner/:address` and `/api/miner/:address/workers`. Operators can enter a
bearer token locally in the dashboard to load `/api/operator/health` and active
alerts and resolve acknowledged incidents; the token is kept in browser `localStorage` as
`csd_pool_operator_token`. Dashboard renderers HTML-escape API-sourced strings
before writing table, alert, health, and payout rows. All API responses include
baseline security headers: Content-Security-Policy, X-Content-Type-Options,
X-Frame-Options, Referrer-Policy, and Permissions-Policy.

When `CSD_POOL_DATABASE_URL` (or the configured `[database].url_env`) is set,
`csd-pool-api` and `csd-pool-daemon` connect to PostgreSQL, apply migrations,
and serve persisted block, payment, miner balance, worker, and pool-fee data.
Without a database URL, the API stays in live in-memory mode for local testing.
Operator endpoints require `Authorization: Bearer <token>` where the token is
read from `CSD_POOL_OPERATOR_TOKEN` or `[api].operator_token_env`. Operator and
signer bearer tokens are compared with a fixed-time equality helper.

Database schema starts at:

```text
migrations/0001_init.sql
```

The first migration keeps `shares` as a regular table so duplicate-share
idempotency can be enforced globally with
`unique(job_id, worker_id, extranonce2, ntime, nonce)`. Public-scale time
partitioning should be added as a later migration after the ingestion and
archive strategy is fixed.

Reward and payout pure logic is implemented in `csd-pool-accounting`:

- PPLNS allocation by share difficulty
- pool fee deduction in basis points
- exact allocation total preservation
- payout candidate selection by threshold and recipient limit

The `csd-pool-db` crate defines the persistence boundary:

- migration registry
- `PoolRepository` trait
- `AsyncPoolRepository` trait
- `InMemoryRepository` for tests and dry-runs
- `PgRepository` for PostgreSQL-backed workers
- `MiningRepository` for Stratum job/share/block-candidate persistence
- `DashboardRepository` for PostgreSQL-backed public API reads
- ledger, balance, and payout batch write/read methods

Apply PostgreSQL migrations:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- migrate
```

Create and restore PostgreSQL backups:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- backup-db backups/csd_pool-$(date +%Y%m%d%H%M%S).dump
```

`backup-db` uses `pg_dump -Fc --no-owner --no-privileges` and prints the backup
path, redacted command, and file size as JSON. If no path is passed, it writes
to `$CSD_POOL_BACKUP_DIR/csd_pool-<unix_ts>.dump`, or to
`backups/csd_pool-<unix_ts>.dump` when the directory variable is unset.
`CSD_POOL_BACKUP_PATH` can provide a one-off exact path. Release verification
can check for a recent `.dump` with `CSD_POOL_VERIFY_BACKUP=1`, using
`CSD_POOL_BACKUP_DIR`, `CSD_POOL_BACKUP_MAX_AGE_DAYS`, and
`CSD_POOL_BACKUP_MIN_BYTES`.

Restore is intentionally gated:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool_restore \
CSD_POOL_RESTORE_CONFIRM=restore \
  cargo run -p csd-pool-workers -- restore-db backups/csd_pool-20260616090000.dump
```

`restore-db` uses `pg_restore --clean --if-exists --no-owner --no-privileges`.
For restore drills, target a fresh `csd_pool_restore` database first, run
`migrate`, then start the API against the restored URL and compare `/api/pool`,
`/api/blocks`, `/api/payments`, and operator payout status before touching
production. The deployment helper `ops/bin/csd-pool-restore-drill.sh` prints a
dry-run plan by default and runs restore plus migrations only when
`CSD_POOL_RESTORE_DRILL_CONFIRM=restore-drill` is set.

Reconcile submitted blocks against a CSD node/watch adapter:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_WATCH_NODE_URL=http://127.0.0.1:8790 \
  cargo run -p csd-pool-workers -- reconcile-blocks
```

The watcher command reads blocks in `submitted`, `seen_on_chain`, or
`immature`, calls `GET /api/rpc/block/status?hash=<hash>`, and updates block
status, height, confirmations, and reward amount.

Settle confirmed block rewards into immutable ledger entries:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- settle-rewards
```

The settlement command reads confirmed blocks that do not yet have block reward
ledger entries, builds a PPLNS allocation from accepted shares for the block's
job, writes `reward_immature` miner entries plus a `pool_fee` entry, and updates
immature balance cache for miners.

Mature rewards after the configured confirmation depth:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- mature-rewards
```

The maturity command turns eligible `reward_immature` entries into
`reward_mature` entries, moves amounts from immature balances to confirmed
balances, and is idempotent per miner/block. Confirmed balances are what payout
selection reads.

Reverse rewards for blocks later marked orphaned:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- reverse-orphans
```

The orphan reversal command reads orphaned blocks that already have
`reward_immature` entries, writes negative `reward_orphan_reversal` entries, and
subtracts the amount from immature or confirmed balances depending on whether
the reward had already matured. It is idempotent per miner/block.

Create, sign, submit, and reconcile automatic payout batches:

Start the development signer in another terminal:

```bash
CSD_POOL_SIGNER_TOKEN=<redacted> \
  cargo run -p csd-pool-signer
```

The Rust `csd-pool-signer` binary is a deterministic mock signer for integration testing. It
validates exact outputs and returns a stable `raw_tx_hex` plus `txid`, but it
does not hold real wallet keys. Production releases include
`ops/wallet-signer/signer.mjs`, backed by pinned official
`@inversealtruism/csd-*` packages. The production systemd unit runs this
sidecar as `csd-signer`; it loads `CSD_POOL_SIGNER_PRIVATE_KEY_FILE`, checks
that its mode is 0600 or stricter, derives and binds
`CSD_POOL_SIGNER_WALLET_ADDRESS`, fetches available UTXOs from
`CSD_POOL_SIGNER_NODE_URL`, verifies input values, and returns official CSD
`node_tx` JSON. Its `/health`
response must expose a non-mock `mode` value such as `wallet`; real go-live
evidence is rejected when `check-signer` reports `mock`, `dev`, or `test`.
It must also expose `wallet_address`, which go-live evidence compares against
`CSD_POOL_SIGNER_WALLET_ADDRESS` to prevent binding the pool to the wrong signer
wallet. Real go-live evidence requires `node_tx_present`, `node_tx_valid`, and
`node_tx_outputs_match_request`; setting the bundled mock signer's mode to
`wallet` or returning arbitrary non-mock hex is not enough for production
signoff. `raw_tx_mock_prefix_present` remains a diagnostic compatibility field,
but official `node_tx` proof is the production requirement.

Provision the key without putting it in the env file or release archive:

```bash
sudo install -o csd-signer -g csd-signer -m 0600 /secure/source/csd-wallet.key \
  /etc/csd-pool/signer-wallet.key
sudo systemctl enable --now csd-pool-signer.service
```

Node.js 18 or newer is required on the signer host. Release archives include
the lockfile and production `node_modules`, so deployment does not fetch npm
packages. The payout worker serializes signing with a PostgreSQL advisory lock
and will not sign another batch while any batch is `signed` or `submitted`;
`blocked_by_inflight_batch` identifies the prior batch awaiting submission or
confirmation. This prevents concurrent requests from reusing the same wallet
UTXO view.

Before resuming payouts, verify the isolated signer contract without touching
PostgreSQL or broadcasting a transaction:

```bash
CSD_POOL_SIGNER_URL=http://127.0.0.1:8890 \
CSD_POOL_SIGNER_TOKEN=<redacted> \
  cargo run -p csd-pool-workers -- check-signer
```

`check-signer` calls `/health`, then posts a one-output, 546-base-unit dust-safe
`contract-check-*` request back to the signer wallet. For production evidence it
requires an official CSD transaction with valid prevout/signature/output byte
arrays, an exact matching payment output, and a 64-hex-character `txid`. It does
not broadcast the contract-check transaction.

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- payout-preview

CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- create-payouts

CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_SIGNER_URL=http://127.0.0.1:8890 \
CSD_POOL_SIGNER_TOKEN=<redacted> \
  cargo run -p csd-pool-workers -- sign-payouts

CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_PAYOUT_NODE_URL=http://127.0.0.1:8789 \
  cargo run -p csd-pool-workers -- submit-payouts

CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_WATCH_NODE_URL=http://127.0.0.1:8790 \
  cargo run -p csd-pool-workers -- reconcile-payouts
```

`payout-preview` lists the next selected recipients and totals without locking
balances and reports whether the configured `[pool].max_payout_batch_csd` cap,
`[pool].max_daily_payout_csd` cap, or
`[pool].manual_payout_approval_csd` threshold would block automatic payout
creation. The same values can be overridden with
`CSD_POOL_MAX_PAYOUT_BATCH_CSD`, `CSD_POOL_MAX_DAILY_PAYOUT_CSD`, and
`CSD_POOL_MANUAL_PAYOUT_APPROVAL_CSD`. `create-payouts` selects confirmed balances above
`[pool].minimum_payout_csd`, refuses to create a batch above
`[pool].max_payout_batch_csd`, the remaining daily payout cap, or the manual
approval threshold. Batches above the manual threshold are created as
`needs_approval`, lock balances with `payout_lock` entries, and become signable
only after `POST /api/operator/payouts/{batch_id}/approve` moves them to
`created`. Batches within limits are created directly as `created` in one
database transaction. Payout creation, manual approval, cancel, retry, signing,
submission, confirmation, and failure paths append immutable
`payout_audit_events` that are visible through
`GET /api/operator/payouts/audit` and exportable from
`GET /api/operator/payouts/audit/export.csv`.

The public `/api/pool` `next_payout_secs` field uses the latest persisted payout
batch creation time plus the configured payout interval when PostgreSQL is
enabled. Before the first batch exists, it falls back to the configured default
countdown.
`sign-payouts` calls
`POST /api/payout/sign` on the isolated signer and stores the returned raw
transaction and txid. `submit-payouts` broadcasts the raw transaction via
official `POST /tx/submit`. `CSD_POOL_PAYOUT_NODE_URL` must point to the direct
official CSD RPC base; if it is absent, the legacy pool adapter route remains
available only for compatibility. `reconcile-payouts` checks
`GET /api/rpc/tx/status?txid=<txid>` and writes `payout_sent` after
confirmation. Ambiguous network failures or node rejections never unlock a
signed payout automatically: the batch remains `signed` for retry and alerting.
An `already present or mempool conflict` response with the expected txid moves
the batch to `submitted` for chain reconciliation. Signing failures that occur
before broadcast can still write `payout_failed_unlock` safely.

Payouts can be paused and resumed through the operator API:

```bash
curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  -X POST http://127.0.0.1:8080/api/operator/payouts/pause

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  -X POST http://127.0.0.1:8080/api/operator/payouts/resume

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/preview

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/export.csv

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/audit?limit=20

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/audit/export.csv?limit=1000

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  -X POST http://127.0.0.1:8080/api/operator/payouts/<batch_id>/approve

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  -X POST http://127.0.0.1:8080/api/operator/payouts/<batch_id>/cancel

curl -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  -X POST http://127.0.0.1:8080/api/operator/payouts/<batch_id>/retry
```

The pause flag is stored in PostgreSQL `pool_settings`; `create-payouts`,
`sign-payouts`, and `submit-payouts` stop when payouts are disabled.
`reconcile-payouts` keeps running so already-submitted batches can settle.
New deployments and upgrades default to paused payouts; operators must call
`POST /api/operator/payouts/resume` after wallet limits, signer isolation, and
manual approval policy are verified. Pause and resume actions are shown in the
operator dashboard and are recorded in `payout_audit_events`.
Operator retry creates a new payout batch from a failed or cancelled batch and
locks balances again. Operator cancel is allowed for `created` or `signed`
batches and returns funds with `payout_failed_unlock`.

Sample service health and generate operator alerts:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_SIGNER_URL=http://127.0.0.1:8890 \
  cargo run -p csd-pool-workers -- sample-health

CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_STUCK_PAYOUT_MINUTES=60 \
CSD_POOL_BLOCK_SUBMISSION_STUCK_MINUTES=10 \
CSD_POOL_NO_ACCEPTED_SHARE_MINUTES=10 \
CSD_POOL_MAX_TEMPLATE_AGE_SECS=120 \
CSD_POOL_WORKER_OFFLINE_MINUTES=15 \
CSD_POOL_SHARE_QUALITY_WINDOW_MINUTES=10 \
CSD_POOL_SHARE_QUALITY_MIN_TOTAL=20 \
CSD_POOL_MAX_REJECT_RATE=0.05 \
CSD_POOL_MAX_STALE_RATE=0.02 \
  cargo run -p csd-pool-workers -- check-alerts
```

`sample-health` writes latest CSD node and signer health to `node_samples`.
When the API uses PostgreSQL, `/metrics` exports those latest samples as
`csd_pool_service_up`, `csd_node_rpc_latency_seconds`, `csd_node_height`, and
`csd_node_peers`.
Go-live evidence includes `http-prometheus-metrics.txt` and
`metrics-surface-safety.log`; the verifier requires Prometheus `/metrics` to
expose core pool, Stratum, share validation, payout, node health, signer health,
and freshness metrics before real launch signoff.
`check-alerts` creates active alerts for failing health checks, payout batches
stuck in `created`, `signed`, or `submitted` longer than the configured
threshold, block candidates whose submit response is not ok or whose submitted
state is older than `CSD_POOL_BLOCK_SUBMISSION_STUCK_MINUTES`, pools with no
accepted shares for `CSD_POOL_NO_ACCEPTED_SHARE_MINUTES`, stale latest mining
jobs older than `CSD_POOL_MAX_TEMPLATE_AGE_SECS`, and workers whose
`last_seen_at` is older than `CSD_POOL_WORKER_OFFLINE_MINUTES`. It also uses persisted rejected/stale share
events to flag workers above `CSD_POOL_MAX_REJECT_RATE` or
`CSD_POOL_MAX_STALE_RATE` over the configured share quality window. Operator
APIs expose `/api/operator/health` and `/api/operator/alerts`, and the built-in
dashboard can resolve active alerts through the operator token flow.

Export immutable accounting ledger entries:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  cargo run -p csd-pool-workers -- accounting-export exports/ledger.csv
```

`accounting-export` writes CSV columns for entry index, miner or `pool`,
signed base-unit amount, signed CSD amount, ledger kind, ref type, and ref id.
Without a path argument it prints the CSV to stdout for piping into storage or
accounting tools.

Run a Stratum smoke/load check against a bridge or HAProxy endpoint:

```bash
CSD_POOL_SMOKE_CLIENTS=50 \
  cargo run -p csd-pool-workers -- stratum-smoke 127.0.0.1:3333

CSD_POOL_SMOKE_CLIENTS=50 \
CSD_POOL_SMOKE_MALFORMED=1 \
  cargo run -p csd-pool-workers -- stratum-smoke 127.0.0.1:3333

CSD_POOL_LOAD_TEST_CLIENTS=100 \
  cargo run -p csd-pool-workers -- stratum-load-test 127.0.0.1:3333
```

`stratum-smoke` opens concurrent TCP sessions, sends
`mining.subscribe`/`mining.authorize`, waits for `mining.set_difficulty` and
`mining.notify`, then emits a JSON success/failure summary with latency stats.
`CSD_POOL_SMOKE_MALFORMED=1` sends one invalid JSON line per session before the
normal handshake so private-beta tests can exercise malformed-frame counters and
temporary ban thresholds. `CSD_POOL_STRATUM_SMOKE_ADDR` can provide the default
endpoint and `CSD_POOL_SMOKE_TIMEOUT_SECS` controls per-read/write timeout.
`stratum-load-test` uses the same handshake path for 100+ simulated miners and
emits a `passed` flag, success/failure counts, latency stats, and
connections-per-second. Local e2e also runs `stratum-accepted-share-probe`
against a static/easy daemon and verifies the accepted share through
`/api/miner/<addr20>`. Use `CSD_POOL_LOAD_TEST_CLIENTS` and
`CSD_POOL_LOAD_TEST_MIN_SUCCESS` to tune public-beta acceptance thresholds. Both
commands print their JSON report before exiting non-zero when the acceptance
criteria fail, so deployment scripts can rely on the process status.

Run reward/payout worker dry-runs:

```bash
cargo run -p csd-pool-workers -- reward-dry-run
cargo run -p csd-pool-workers -- payout-dry-run
```

If `CSD_POOL_DATABASE_URL` is set, the dry-runs apply migrations and persist to
PostgreSQL. Without it, they use the in-memory repository for fast local checks.

The dry-runs emit JSON including reward allocations, ledger entries, payout
recipients, payout lock entries, and repository-persisted snapshots. They do not
sign or submit transactions.

The live template path expects a CSD node or adapter that exposes:

```text
GET  /api/rpc/mining/template?address=<pool_addr20>
POST /api/rpc/block/submit
GET  /api/rpc/block/status?hash=<hash>
POST /tx/submit                    # official payout RPC
GET  /api/rpc/tx/status?txid=<txid>
```

The standard template response expected by the pool is:

```json
{
  "job_id": "job1",
  "prev_hash_be_hex": "64 hex chars",
  "coinb1_hex": "hex",
  "coinb2_hex": "hex",
  "merkle_branches_hex": [],
  "version_hex": "20000000",
  "nbits_hex": "207fffff",
  "ntime_hex": "665544cc",
  "clean_jobs": true,
  "share_target_hex": "64 hex chars",
  "network_target_hex": "64 hex chars"
}
```
