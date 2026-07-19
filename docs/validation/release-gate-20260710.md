# CSD Pool Release Gate Validation - 2026-07-10

This report records the locally reproducible release and launch-gate evidence
for the current workspace. It is an implementation and delivery validation;
it is not a claim that the pool is connected to CSD mainnet.

## Release Artifact

- Archive: `dist/csd-pool-nogit-20260710T120100Z.tar.gz`
- SHA256: `8a8aea5259598b6ba7cf537fe14449fa7203f791d616d61dd0f5144424238a33`
- Archive checksum file: `dist/csd-pool-nogit-20260710T120100Z.tar.gz.sha256`
- Wallet signer package: included with locked production dependencies
- Official node adapter patch and manifest: included under `ops/csd-node-adapter/`

## Passed Evidence

- Rust workspace tests, formatting, Clippy, release build, and static release
  verifier passed in the current validation cycle.
- `csd-pool-release-archive-self-test.sh` passed. It verified archive hashes,
  packaged documentation redaction, required CI and launch-gate coverage, and
  rejection of recomputed tampered archives.
- `csd-pool-install-release-self-test.sh` passed under `umask 0077`, including
  first install, upgrade markers, atomic current symlink, and rollback.
- `csd-pool-live-startup-policy-self-test.sh` passed for the packaged binaries;
  live mode fails closed without PostgreSQL and candidate submission.
- Real-environment doctor, public-acceptance, evidence-redaction, launch
  handoff, launch dossier, launch gaps, and evidence self-tests passed.
- Static Stratum smoke and accepted-share probes passed against the local
  daemon. Mock-node template, candidate, and signer contract probes passed.
- The official-node candidate canary is documented in
  `official-node-candidate-canary-20260710.md`.

## External Verification Still Required

The following checks require real deployment inputs and were intentionally not
represented as passed by local fixtures:

- PostgreSQL and Redis live persistence, migrations, restore drill, and payout
  serialization against a real database.
- A live official CSD node adapter with real watch, submit, and payout RPCs.
- The production wallet signer with an isolated 32-byte private key and a
  wallet address verified against the official SDK.
- Public DNS/TLS/HAProxy routing, external Stratum acceptance, and a real
  canary miner producing accepted shares through the public edge.

The local Docker dependency check was attempted but the Docker daemon was not
available in this environment. The release is therefore ready for target-host
verification, but the final launch status remains `needs_real_environment_evidence`
until `ops/bin/csd-pool-real-go-live.sh` and the external public acceptance pass
produce a non-dry-run receipt.

## Target-Host Acceptance Sequence

1. Install the release archive and run `ops/bin/csd-pool-preflight.sh`.
2. Run `ops/bin/csd-pool-real-env-doctor.sh` with real env/config and public
   endpoints.
3. Run `ops/bin/csd-pool-real-go-live.sh` without dry-run mode.
4. Verify the generated receipt with
   `ops/bin/csd-pool-verify-real-go-live-receipt.sh`.
5. From an external host, run `ops/bin/csd-pool-public-acceptance.sh` with a
   real canary address and require accepted shares.
6. Export and verify the launch handoff and dossier packages before enabling
   public mining traffic.
