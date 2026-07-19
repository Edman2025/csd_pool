# CSD Pool Mainnet Production Acceptance - 2026-07-19

This report records real mainnet evidence from the private CSD pool deployed on
`125.124.229.35`. It contains no bearer tokens, passwords, or private-key
material.

## Release

- Pool revision: `77eb17641c42b723677029207d38799326a67223`
- Pool daemon SHA256:
  `38e9365cf2960f41bbc8aad5773144733d99e62a6904391e8f6dfcbf634db7e4`
- Official node source revision:
  `d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c`
- Node adapter patch SHA256:
  `6f3a42738202b21a04fd5f069552ea742baa122638757f399e70eedf215fced7`
- Node binary SHA256:
  `cad550af691660ff39fec46b9296a3b16b9daadd767821ced6c35429dc0ecd08`
- Mining and signer address:
  `0x831e14ea97fc0f0f459a65147aa18cc47700ae76`

## Mainnet And Template Validation

At the final snapshot, both local nodes and the public Cairn RPC reported height
`57380`, tip
`0000000000001037a6414294127f7cc358d3f479954a70fd483971d20d6ed65b`,
and chainwork `12668281391048795782`.

Five authenticated template calls completed in `0.569-0.847 ms`. The template
parent matched the live node tip, `clean_jobs` was true, and the configured
mining address occurred exactly once in the reconstructed coinbase.

The node adapter now selects a consensus-valid pool timestamp without blocking
on wall clock. The pool daemon also revalidates the template parent against the
live tip before broadcasting. Production refresh logs reported `1-2 ms`, with
zero stale-template discards and zero warning-or-higher log entries after the
release restart.

## Stratum And Share Validation

- Public endpoint: `125.124.229.35:3333`
- External cloud probe passed:
  `subscribe -> authorize -> set_difficulty -> notify`
- Internal management, node RPC, and signer ports remained loopback-only.
- Active production Stratum sessions: `308`
- Distinct production workers with accepted shares after restart: `308`
- Stable two-minute window:
  - accepted shares: `1093`
  - low-difficulty rejects: `20`
  - stale old-job submits: `2`
  - sessions at start and end: `308`
- Final pool estimate: `950.5 GH/s`
- Final in-process counters:
  - accepted: `15394`
  - rejected: `239`
  - stale: `36`

All production workers remained connected after the controlled daemon restart.
The low-difficulty rejects were routine VarDiff transitions. No continuous
rejected/stale pattern, connection ban, or shared-NAT eviction was observed.

## Mainnet Block And Reward Closure

Three pool blocks were confirmed and paid directly to the configured mining
address:

| Height | Block hash | Coinbase txid | Reward |
| --- | --- | --- | ---: |
| 57336 | `000000000000104d1f49f78b1137d159832052d1af70464b6b2e5b3f364b4aa3` | `fb31548e3e9f22d11858794cbfe6dabf90cada37b8164e7d87a7b1614b072643` | 50 CSD |
| 57367 | `0000000000000ca9d4125245a213ed9f65f6b0649809f45ff24a9b30ef0ea2eb` | `a899dbdee628409f13020d8fcf7c7a22b6ba05318fa05c2b2f6ebab37b85698d` | 50 CSD |
| 57371 | `00000000000007a5983e6128459d1b59827e7fd29bbef73562571fe4e6840676` | `a8b477afc584a91d0be9967fffc3e74bbc5d6d08ceff4ec3d7a6467b31213047` | 50 CSD |

The blocks at heights `57367` and `57371` were found after the nonblocking
template fix was deployed. Both reached the configured 10-confirmation depth,
were marked `confirmed` by PostgreSQL reconciliation, and appeared in the
official wallet UTXO response.

The final official wallet snapshot reported:

- confirmed balance: `170.41943782 CSD`
- unspent mined value: `150 CSD`
- unspent coinbase outputs: `3`

This proves the complete real path:

```text
mainnet template -> Stratum job -> accepted share -> block candidate
-> node acceptance -> peer propagation -> 10 confirmations
-> coinbase UTXO at the configured wallet
```

The three orphaned candidate records were submitted before the timestamp fix
deployment and remain in PostgreSQL as audit history.

## Operations And Security

- Daemon, node A, node B, and signer: active, zero restarts after deployment.
- Warning-or-higher journal entries after deployment: zero for all four
  services.
- Node peers at final snapshot: four on each node.
- NTP synchronization: enabled.
- Data volume: 52% used, 91 GiB available.
- Daemon hardening: dedicated user, `NoNewPrivileges`, `PrivateTmp`,
  `ProtectHome`, and strict `ProtectSystem`.
- Signer key: owned by the signer service account with mode `0600`.
- Signer health address matched the configured mining address.
- Automated payout: disabled. Coinbase rewards are paid directly to the single
  private-pool wallet, so no automatic transfer is required for mining income.
- Reconciliation, health sampling, and backup timers: active.
- Pre-deploy database dump checksum and `pg_restore --list`: passed.
- Isolated restore drill: passed with 8 migrations, 20,930 jobs, 91,920 shares,
  and 4 block records from the pre-deploy snapshot.

## Decision

`production_live`

The pool is performing real mainnet work and has completed two post-fix
block-to-wallet reward closures. Production miners may remain connected.
Automatic payout must stay disabled until a separate multi-recipient payout
acceptance and operator approval are completed.
