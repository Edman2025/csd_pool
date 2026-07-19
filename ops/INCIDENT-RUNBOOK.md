# CSD Pool Incident Runbook

Use this runbook for private beta, public beta, and production incidents. Keep
external contact details in the release notes or the on-call system; do not
commit personal phone numbers or chat links here.

## 1. Triage

Start with public and operator health:

```bash
curl -fsS http://127.0.0.1:8080/status >/dev/null
curl -fsS http://127.0.0.1:8080/api/status | jq .
curl -fsS http://127.0.0.1:8080/metrics | rg 'csd_pool_service_up|csd_node_|csd_pool_share_validation'
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  /opt/csd-pool/bin/csd-pool-workers check-alerts
```

Classify impact:

- **SEV1:** Stratum unavailable, payouts unsafe, or CSD node submission path
  unavailable.
- **SEV2:** degraded node health, high stale/reject rate, delayed payouts, or
  dashboard/operator API degradation.
- **SEV3:** documentation, cosmetic dashboard, or single non-critical worker
  issue.

## 2. Stabilize

For payout or signer risk, pause automatic payout creation/signing/submission:

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/pause
```

Reconciliation should continue so already-submitted transactions can settle.

For Stratum/API failures:

```bash
systemctl status csd-pool-daemon.service
journalctl -u csd-pool-daemon.service -n 200 --no-pager
systemctl status haproxy
haproxy -c -f /etc/haproxy/haproxy.cfg
```

For CSD node failures:

```bash
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
CSD_POOL_CONFIG=/etc/csd-pool/config.toml \
  /opt/csd-pool/bin/csd-pool-workers sample-health
curl -fsS http://127.0.0.1:8080/api/status | jq '.node_count,.unhealthy_services'
```

## 3. Diagnose

Check the most common signals:

```bash
curl -fsS http://127.0.0.1:8080/metrics | rg 'workers_online|shares_total|blocks_|payout_|service_up'
curl -fsS -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  'http://127.0.0.1:8080/api/operator/alerts?status=active&limit=100' | jq .
curl -fsS -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/status | jq .
```

Export evidence before making risky changes:

```bash
curl -fsS -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/audit/export.csv \
  -o /var/backups/csd-pool/payout-audit-$(date +%s).csv
CSD_POOL_DATABASE_URL=postgres://user:<redacted>@127.0.0.1:5432/csd_pool \
  /opt/csd-pool/bin/csd-pool-workers accounting-export \
  /var/backups/csd-pool/ledger-$(date +%s).csv
```

## 4. Roll Back

Before rolling back, record the current revision, config checksum, and latest
backup path in the incident notes.

```bash
sha256sum /etc/csd-pool/config.toml /etc/csd-pool/csd-pool.env
cat /opt/csd-pool/CURRENT_RELEASE /opt/csd-pool/PREVIOUS_RELEASE
ls -lh /var/backups/csd-pool/*.dump | tail
systemctl stop csd-pool-daemon.service
ops/bin/csd-pool-rollback-release.sh
systemctl start csd-pool-daemon.service
ops/bin/csd-pool-verify.sh
```

Do not roll back database migrations without a restore drill and owner signoff.

## 5. Recover

After the root cause is fixed:

```bash
ops/bin/csd-pool-verify.sh
CSD_POOL_SMOKE_CLIENTS=20 /opt/csd-pool/bin/csd-pool-workers stratum-smoke 127.0.0.1:3333
curl -fsS http://127.0.0.1:8080/api/status | jq .
```

Resume payouts only after signer health, wallet limits, and payout preview are
reviewed:

```bash
curl -fsS -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/preview | jq .
curl -fsS -X POST \
  -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/payouts/resume
```

Resolve acknowledged alerts:

```bash
curl -fsS -X POST \
  -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
  http://127.0.0.1:8080/api/operator/alerts/<fingerprint>/resolve
```

## 6. Post-Incident

Attach to the incident record:

- `/api/status` output before and after recovery
- relevant `/metrics` excerpt
- active alert export or screenshots
- payout audit CSV when payout state changed
- rollback or deploy revision
- customer/miner impact window
- follow-up tasks and owner
