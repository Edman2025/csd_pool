# CSD Pool Release Checklist

Use this checklist before a private beta, public beta, or production rollout.
Every item should have command output, an exported report, or an operator signoff
attached to the release notes.

## 1. Build And Test

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo check --workspace`
- `npm ci --ignore-scripts --prefix ops/wallet-signer`
- `npm test --prefix ops/wallet-signer`
- `ops/bin/csd-pool-ci-local.sh` completed locally or equivalent GitHub Actions
  CI completed successfully
- `ops/bin/csd-pool-real-env-doctor-self-test.sh` passed, proving the real
  environment doctor accepts production-shaped inputs, rejects loopback or
  placeholder launch inputs, and redacts database passwords in reports
- `ops/bin/csd-pool-evidence-redaction-self-test.sh` passed, proving receipt
  and public acceptance evidence redaction scans reject recomputed tampered
  packages
- `ops/bin/csd-pool-public-acceptance-self-test.sh` passed, proving public
  acceptance evidence rejects fixture/example and non-global public endpoints
  even when package hashes are valid
- `ops/bin/csd-pool-release-archive-self-test.sh` passed against the release
  archive, proving package documentation redaction rejects recomputed tampered
  release tarballs
- `cargo build --release --workspace`
- `ops/bin/csd-pool-build-release.sh` completed
- attach `RELEASE-MANIFEST.txt`, `SHA256SUMS`, and archive `.sha256`
- confirm the archive contains executable `ops/wallet-signer/signer.mjs`, its
  lockfile, and packaged `@inversealtruism/csd-client`, `csd-crypto`, and
  `csd-tx` dependencies
- install rehearsal completed with `CSD_POOL_INSTALL_ROOT=/tmp/csd-pool-install-root`
- archive install/rollback self-test completed with `ops/bin/csd-pool-install-release-self-test.sh`
- rollback rehearsal completed with `ops/bin/csd-pool-rollback-release.sh`

## 2. Configuration

- `/etc/csd-pool/csd-pool.env` generated with `ops/bin/csd-pool-generate-env.sh`
- `/etc/csd-pool/config.toml` reviewed against `ops/config.private-beta.toml`
- `/etc/csd-pool/csd-pool.env` contains no placeholder secrets
- `/etc/csd-pool/csd-pool.env`, config, and optional secret files are readable
  by owner only
- `ops/bin/csd-pool-preflight.sh` passes against the real config and env
- `ops/bin/csd-pool-real-env-doctor.sh` passes against the real config and env,
  writing `REAL-ENVIRONMENT-DOCTOR.txt` and
  `real-environment-doctor-summary.json` with `status=ready_for_real_go_live`
- `ops/bin/csd-pool-verify.sh` passes against the real `CSD_POOL_ENV_FILE`
- `ops/bin/csd-pool-real-go-live.sh` passes without `CSD_POOL_GO_LIVE_DRY_RUN`
- lower-level `ops/bin/csd-pool-go-live-check.sh` output is archived by the real
  go-live wrapper
- `ops/bin/csd-pool-generate-signoff.sh` generated `GO-LIVE-SIGNOFF.md`
- `ops/bin/csd-pool-verify-real-go-live-summary.sh` passes against
  `REAL-GO-LIVE-SUMMARY.txt`
- `ops/bin/csd-pool-export-real-go-live-receipt.sh` generated the portable
  `csd-pool-*-real-go-live-receipt-*.tar.gz` receipt
- `ops/bin/csd-pool-verify-real-go-live-receipt.sh` passes against the receipt
  archive and `.sha256` file, including manifest metadata binding and its
  receipt redaction scan
- `ops/bin/csd-pool-public-acceptance.sh` passes from outside the pool host
  against the public API and public Stratum endpoint
- production launch review uses `CSD_POOL_ACCEPTANCE_CANARY_ADDRESS` and
  `CSD_POOL_ACCEPTANCE_REQUIRE_ACCEPTED_SHARE=1` so
  `public-canary-miner.json` proves accepted shares through public
  `/api/miner/<addr20>`
- `ops/bin/csd-pool-verify-public-acceptance-evidence.sh` passes against
  `public-acceptance-evidence.tar.gz` and its `.sha256` file, including its
  public acceptance redaction scan
- `ops/bin/csd-pool-verify-launch-handoff.sh` passes against the release
  archive, real go-live receipt, and public acceptance evidence archive
- `ops/bin/csd-pool-export-launch-handoff.sh` generated the portable
  `csd-pool-*-launch-handoff-*.tar.gz` package
- `ops/bin/csd-pool-verify-launch-handoff-package.sh` passes against the
  handoff package and `.sha256` file, including summary-to-manifest artifact
  name and SHA256 bindings
- `ops/bin/csd-pool-audit-launch-readiness.sh` passes against the handoff
  package and exports `LAUNCH-READINESS-REPORT.txt` plus
  `launch-readiness-summary.json` with `status=launch_ready`
- `ops/bin/csd-pool-export-launch-dossier.sh` generated the portable
  `csd-pool-*-launch-dossier-*.tar.gz` final review package
- `ops/bin/csd-pool-verify-launch-dossier.sh` passes against the dossier
  package and `.sha256` file
- `ops/bin/csd-pool-finalize-launch.sh` completed and wrote
  `FINAL-LAUNCH-REPORT.txt` plus `final-launch-summary.json`
- final launch summary records `require_public_accepted_share=true`
- final launch summary records accepted-share minimums and canary accepted-share
  count matching the launch readiness summary
- if finalization status is `needs_real_environment_evidence`,
  `ops/bin/csd-pool-explain-launch-gaps.sh` completed and wrote
  `LAUNCH-GAPS-REPORT.txt` plus `launch-gaps-summary.json`
- `ops/bin/csd-pool-export-final-review.sh` generated
  `csd-pool-final-review-*.tar.gz` with `FINAL-REVIEW-MANIFEST.txt` and
  `FINAL-REVIEW-SHA256SUMS`
- `ops/bin/csd-pool-verify-final-review.sh` passes against the final review
  package and `.sha256` file
- `ops/bin/csd-pool-final-review-self-test.sh` passes against the final review
  package, proving summary SHA tampering is rejected even after outer hashes are
  recomputed
- attach `HANDOFF-README.txt`, `HANDOFF-MANIFEST.txt`, `HANDOFF-SHA256SUMS`,
  `handoff-summary.json`, and the `csd-pool-*-launch-handoff-*.tar.gz` package
- attach `LAUNCH-READINESS-REPORT.txt` and `launch-readiness-summary.json`
- attach `DOSSIER-MANIFEST.txt`, `DOSSIER-SHA256SUMS`,
  `launch-dossier-summary.json`, and the `csd-pool-*-launch-dossier-*.tar.gz`
  package
- attach `FINAL-LAUNCH-REPORT.txt` and `final-launch-summary.json`
- attach `LAUNCH-GAPS-REPORT.txt` and `launch-gaps-summary.json` for any
  non-launch-ready review package
- attach `PUBLIC-ACCEPTANCE-REPORT.txt`, `public-acceptance-summary.json`,
  `public-stratum-submit-probe.json`, `public-canary-miner.json`, and
  `public-acceptance-evidence.tar.gz`
- confirm the receipt archive contains `RECEIPT-MANIFEST.txt` and
  `RECEIPT-SHA256SUMS`
- attach `csd-pool-*-real-go-live-receipt-*.tar.gz` and its `.sha256` file from
  `CSD_POOL_GO_LIVE_REPORT_DIR`
- attach `REAL-GO-LIVE-SUMMARY.txt` from `CSD_POOL_GO_LIVE_REPORT_DIR`
- confirm `real-go-live-inputs.log` is present and shows
  `real_go_live_inputs_ok=True`, `dry_run_env_false=True`,
  `env_example_path=False`, and `config_example_path=False`
- confirm `launch-toolchain-manifest.json` is present and records
  `csd-pool-workers`, `csd-pool-go-live-check.sh`,
  `csd-pool-verify-go-live-evidence.sh`, `csd-pool-generate-signoff.sh`,
  `csd-pool-export-real-go-live-receipt.sh`, and
  `csd-pool-real-env-doctor.sh`
- confirm `real-go-live-postcheck.log` is present and shows
  `real_go_live_postcheck_ok=True`, `summary_dry_run_false=True`, and a matching
  evidence archive checksum
- confirm `REAL-ENVIRONMENT-DOCTOR.txt` and
  `real-environment-doctor-summary.json` are present and show
  `ready_for_real_go_live` with zero hard failures
- confirm `REAL-GO-LIVE-SUMMARY.txt` records `go_live_report_sha256`,
  `go_live_summary_sha256`, `go_live_signoff_sha256`,
  `real_go_live_inputs_sha256`, `launch_toolchain_manifest_sha256`,
  `real_environment_doctor_report_sha256`,
  `real_environment_doctor_summary_sha256`, and `evidence_archive_sha256`
- attach `GO-LIVE-SIGNOFF.md` from `CSD_POOL_GO_LIVE_REPORT_DIR`
- attach `GO-LIVE-REPORT.txt` and `go-live-summary.json` from
  `CSD_POOL_GO_LIVE_REPORT_DIR`
- attach `go-live-evidence.tar.gz` and `go-live-evidence.tar.gz.sha256`
- confirm `config-snapshot.json` is present, redacted, and has `passed=true`
- confirm `env-snapshot.txt` is present, redacted, and records
  `world_readable=false` plus required key presence
- confirm `secrets-permissions-safety.log` is present and proves env/config
  secret files had no group/other permissions on the target host
- confirm `evidence-redaction-safety.log` is present and proves the evidence
  bundle contains no bearer tokens, plaintext secret env values, or database
  URLs with passwords
- confirm `real-env-readiness.log` is present and proves PostgreSQL,
  watch/submit node URLs, signer URL, separate restore database, and
  production-length operator/signer tokens
- confirm `clock-safety.log` is present and proves the target host clock was
  synchronized before evidence collection
- confirm `disk-safety.log` is present and proves key target filesystems had
  enough free bytes and inodes before evidence collection
- confirm `bind-safety.log` is present and proves internal API, Stratum, and
  signer listeners are loopback-only behind the public edge
- confirm `release-integrity.log` is present and verifies the release
  `SHA256SUMS`
- confirm `systemd-runtime-safety.log` is present and proves daemon, signer, and
  critical worker timers are enabled and active
- confirm `runtime-hardening-safety.log` is present and proves loaded
  daemon/signer units keep expected users and hardening properties
- confirm `resource-limit-safety.log` is present and proves daemon/signer
  configured and live open-file limits meet launch minimums
- confirm `database-runtime.json` is present and proves PostgreSQL connectivity,
  key table reads, transaction write/rollback, and query latency are healthy
- confirm `backup-artifact-safety.log` is present and proves the selected backup
  file is fresh, large enough, and has a recorded sha256
- confirm `restore-drill.log` is present and shows `restore drill complete`
- confirm `restore-api-safety.log` is present and proves restored API pool,
  block, payment, and operator payout status probes passed
- confirm restore API raw JSON reports are present and valid:
  `restore-http-health.json`, `restore-http-pool.json`,
  `restore-http-blocks.json`, `restore-http-payments.json`, and
  `restore-http-operator-payout-status.json`
- confirm `stratum-tcp.log` is present and successful in the evidence archive
- for public-beta/production, confirm `edge-proxy-safety.log` is present and
  proves the target HAProxy config validates and maps public Stratum/API ingress
  to the reviewed loopback pool backends
- for public-beta/production, confirm `public-dns-safety.log` is present and
  proves public API and Stratum hosts resolve to global public addresses
- for public-beta/production, confirm `public-api-tls-safety.log` is present and
  proves the public API certificate is hostname-valid and not expiring before
  `CSD_POOL_PUBLIC_API_TLS_MIN_VALID_DAYS`
- for public-beta/production, confirm `public-api-headers-safety.log` is present
  and proves the public API edge preserves CSP, nosniff, DENY frame policy,
  no-referrer, and Permissions-Policy headers
- for public-beta/production, confirm `public-api-surface-safety.log` is present
  and proves public JSON endpoints preserve JSON content types, safe cache
  headers, and operator-only field boundaries
- for public-beta/production, confirm `public-operator-auth-boundary.log` is
  present and proves the public edge rejects missing or invalid operator bearer
  tokens and accepts the configured token
- for public-beta/production, confirm `public-stratum-smoke.json` is present and
  shows every requested external Stratum smoke client succeeded
- for public-beta/production, confirm `public-stratum-load.json` is present and
  shows `stratum-load-test` passed with `failed_clients=0` and
  `succeeded_clients` at or above `min_success_clients`
- for public-beta/production, confirm `public-port-tiers-safety.log` is present
  and proves every enabled `CSD_POOL_PUBLIC_PORT_TIERS` port accepted TCP and
  included the configured public Stratum probe port
- for public-beta/production, confirm `public-port-tiers-smoke.json` is present
  and proves every enabled `CSD_POOL_PUBLIC_PORT_TIERS` port completed the
  Stratum protocol smoke flow
- confirm public HTTP JSON reports are present and valid:
  `/api/status`, `/api/metrics`, `/api/blocks`, `/api/payments`, and
  `/api/getting-started`
- confirm `metrics-surface-safety.log` and `http-prometheus-metrics.txt` are
  present and prove Prometheus `/metrics` exposes core pool, Stratum, share
  validation, payout, node health, signer health, and freshness metrics
- confirm operator HTTP JSON reports are present and valid:
  `/api/operator/health`, `/api/operator/alerts`,
  `/api/operator/payouts/preview`, and `/api/operator/payouts/status`
- confirm `operator-readiness-safety.log` is present and proves node and signer
  samples are healthy and active alerts are empty
- confirm operator CSV reports are present with expected headers:
  `/api/operator/payouts/export.csv` and
  `/api/operator/payouts/audit/export.csv`
- confirm `http-operator-payout-status.json` is present and exposes the
  reviewed `payouts_enabled` launch state
- confirm `payout-safety.log` is present and proves payouts were paused before
  launch signoff
- confirm `payout-controls-safety.log` is present and proves operator payout
  status, preview, batch list, audit JSON, and CSV exports are structurally ready
  for launch
- confirm `sample-health.json` is present and `runtime-status-binding.log`
  proves `/api/status` reported PostgreSQL-backed `operational` runtime with
  at least one node health sample inside `CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES`
- confirm `runtime-config-binding.log` is present and proves
  `/api/status.config` matches the go-live `config-snapshot.json`
- confirm `node-endpoint-safety.log` is present and proves configured CSD nodes
  are non-loopback, non-mock endpoints with template/network/submit health
- confirm `node-runtime.json` is present and proves configured CSD nodes meet
  template, submit, and watch role quorum plus RPC latency thresholds
- confirm `signer-safety.log` is present and proves the configured signer
  reports a non-mock `/health.mode`
- confirm `payout-limit-safety.log` is present and proves payout limits are
  positive, ordered, and require manual approval below the max batch cap
- confirm `EVIDENCE-SHA256SUMS` is present inside the evidence archive
- confirm the evidence verifier reports metadata consistency across
  `go-live-summary.json`, `GO-LIVE-REPORT.txt`, and `EVIDENCE-MANIFEST.txt`
- confirm the evidence verifier reports evidence freshness within the approved
  launch window
- `ops/bin/csd-pool-verify-go-live-evidence.sh` passes against the attached
  evidence archive
- local adapter contract check passes with `CSD_POOL_VERIFY_MOCK_NODE=1`
- local smoke harness passes with `ops/bin/csd-pool-local-e2e.sh`
- `CSD_POOL_OPERATOR_TOKEN` is unique for the environment
- `CSD_POOL_SIGNER_TOKEN` is unique for the signer
- payout limits reviewed:
  - `[pool].minimum_payout_csd`
  - `[pool].max_payout_batch_csd`
  - `[pool].max_daily_payout_csd`
  - `[pool].manual_payout_approval_csd`

## 3. Database

- `csd-pool-workers migrate` completed against the target database
- latest backup created with `csd-pool-workers backup-db`
- `ops/bin/csd-pool-verify.sh` passes with `CSD_POOL_VERIFY_BACKUP=1`
- restore drill completed with `ops/bin/csd-pool-restore-drill.sh`
- restored API compared on `/api/pool`, `/api/blocks`, and `/api/payments`
- accounting export generated with `csd-pool-workers accounting-export`

## 4. Services

- `csd-pool-daemon.service` enabled and healthy
- `csd-pool-signer.service` enabled on the signer host
- signer `/health.mode` reports a wallet-backed non-mock mode before launch
- payout timers enabled:
  - `csd-pool-payout-create.timer`
  - `csd-pool-payout-sign.timer`
  - `csd-pool-payout-submit.timer`
  - `csd-pool-payout-reconcile.timer`
- reward, block reconciliation, monitoring, and backup timers enabled
- HAProxy config validates with `haproxy -c -f /etc/haproxy/haproxy.cfg`
- `ops/bin/csd-pool-verify.sh` passes against the real `CSD_POOL_HAPROXY_CONFIG`
- systemd services keep `NoNewPrivileges`, `PrivateTmp`, `ProtectHome`, and
  `ProtectSystem=strict` enabled

## 5. Runtime Verification

- `ops/bin/csd-pool-verify.sh` passes with release, migration, HTTP, smoke, and
  load checks enabled
- `/health`, `/api/pool`, `/api/metrics`, `/api/blocks`, `/api/payments`,
  `/getting-started`, and `/api/getting-started` return successfully
- `/status` and `/api/status` return public status successfully
- `/api/status` `release.name`, `release.revision`, and
  `release.timestamp_utc` match the installed `RELEASE-MANIFEST.txt`
- public-beta/production evidence includes `http-public-api-status.json` and
  `external-public-status-binding.log`, proving the external API serves the same
  release and PostgreSQL runtime
- public-beta/production evidence includes `external-public-config-binding.log`,
  proving the external API serves the same runtime config as
  `config-snapshot.json`
- public-beta/production evidence includes `http-public-api-getting-started.json`,
  `getting-started-binding.log`, and
  `external-public-getting-started-binding.log`, proving miner setup JSON uses
  the configured public Stratum address and port tiers
- public-beta/production evidence includes `public-operator-auth-boundary.log`,
  proving public operator endpoints enforce bearer-token authentication at the
  external edge
- public-beta/production evidence includes `public-stratum-tcp.log`,
  `public-port-tiers-safety.log`, `public-port-tiers-smoke.json`, and
  `public-stratum-smoke.json`, proving the external Stratum edge and every
  enabled public port tier are reachable and speak the mining protocol
- public-beta/production evidence includes `public-stratum-load.json`, proving
  the external Stratum edge passes the configured concurrent simulated miner
  load threshold
- operator API works with bearer token:
  - `/api/operator/health`
  - `/api/operator/alerts`
  - `/api/operator/payouts/status`
  - `/api/operator/payouts/preview`
- `csd-pool-workers stratum-smoke` passes through the public Stratum endpoint
- `csd-pool-workers stratum-submit-probe` passes through the public Stratum
  endpoint and records a standard `mining.submit` response
- local e2e `csd-pool-workers stratum-accepted-share-probe` passes against the
  static/easy daemon and `/api/miner/<addr20>` shows `shares_accepted=1`
- local e2e `csd-pool-workers mine-node-candidate-canary` passes against the
  easy mock adapter; an approved isolated official-node canary records a solved
  canonical block using the exact `mine-and-submit` confirmation value
- `csd-pool-workers stratum-load-test` passes with at least 100 simulated
  workers through the public Stratum endpoint
- abuse guard drill with `CSD_POOL_SMOKE_MALFORMED=1` does not destabilize the
  bridge

## 6. Payout Safety

- payouts start paused until operator signoff
- signer is reachable only on private interfaces
- first real payout is below the manual approval threshold
- manual approval path tested for a `needs_approval` batch
- cancel and retry paths tested on a non-production batch
- payout audit events exported from `/api/operator/payouts/audit/export.csv`

## 7. Monitoring And Incident Response

- health sampling timer writes node and signer samples
- `check-alerts` creates active alerts for simulated failure conditions
- operator dashboard shows health, alerts, payout preview, payout batches, and
  payout audit events
- rollback plan names the previous release artifact and config snapshot
- `ops/INCIDENT-RUNBOOK.md` has been reviewed for this environment
- incident contacts and escalation windows are attached to the release notes or
  on-call system

## 8. Launch Decision

- private beta: one Stratum endpoint, one daemon, isolated signer, tested backup
- public beta: DDoS/rate limit fronting, public docs, operator on-call coverage
- production: multi-region edge, external orchestration, security review, and
  independent wallet custody review
