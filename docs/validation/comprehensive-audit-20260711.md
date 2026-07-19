# CSD Pool Comprehensive Audit

Date: 2026-07-11

Scope: product requirements, production data paths, Stratum bridge, CSD node
adapter, PostgreSQL accounting, payout signer, release gates, and local runtime
verification.

## Conclusion

The repository is a real implementation candidate and its live startup policy
fails closed when PostgreSQL or candidate submission is missing. It is not yet
evidence of a public mainnet launch from this workstation. A production release
must remain blocked until the target host produces non-dry-run go-live evidence,
public acceptance evidence, and a real CSD node/payout canary.

## Passed

- Rust workspace format, tests, and clippy with `-D warnings`.
- Wallet signer tests using the pinned official CSD SDK and exact node
  transaction JSON.
- Release static checks: `pass=1777 fail=0`.
- Live startup policy rejects missing PostgreSQL and disabled candidate
  submission.
- Static runtime: API health, ten concurrent Stratum handshakes, and an
  accepted-share probe.
- Release artifact build and installation self-test under restrictive `umask`.
- Evidence redaction, real-environment doctor, and public-acceptance negative
  self-tests.

## Bugs fixed in this audit

1. Candidate block transport errors could lose a solved candidate before it was
   written to `blocks`. The bridge now persists a retryable candidate record with
   the solved hash and transport error before returning the error. A regression
   test covers this path.
2. Concurrent systemd workers could race while checking and applying PostgreSQL
   migrations. Migration application now uses a PostgreSQL transaction advisory
   lock for the complete check/apply sequence.
3. Miner detail returned fixed zero values for session earnings and rate fields,
   which were not backed by persisted session data. These fields now return
   `null` until a real session/rate estimator exists. Confirming blocks are read
   from persisted block records when PostgreSQL is available.

## Remaining implementation gaps

- Stratum sessions are not persisted to the `sessions` table. Session start,
  session earnings, and session duration therefore remain unavailable after
  restart.
- Miner hourly/daily earnings estimation is not implemented; the API correctly
  returns `null` rather than fabricated numbers.
- The daemon uses one node-local live template/submission provider. Runtime node
  quorum checks exist, but automatic template failover is not implemented.
- The official node adapter's `/api/network` endpoint does not calculate a
  network hashrate. Network hashrate must come from a verified external
  telemetry source such as the configured Cairn endpoint; otherwise the API
  reports zero/unknown network telemetry.
- PostgreSQL-backed local E2E, restore drill, payout serialization against a
  real database, official-node candidate canary, public DNS/TLS acceptance, and
  a real miner canary were not executable here because the local Docker daemon
  and PostgreSQL service were unavailable.

## Release decision

`release_candidate_ready_for_target_host_verification`

This is not `production_live`. The final gate is:

```text
real environment doctor -> live go-live check -> non-dry-run receipt
-> public acceptance with an accepted-share canary -> owner sign-off
```

No fixture, static job, mock node, or mock signer result should be used as
evidence for that final decision.
