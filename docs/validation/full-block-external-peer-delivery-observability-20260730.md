# Full-Block External Peer Delivery Observability

## Scope

This change adds bounded telemetry around candidate full-block delivery without
changing candidate detection, consensus submission, peer selection, template
ownership, or relay policy.

Evidence levels are intentionally distinct:

- `DELIVERY_ATTEMPT_ONLY`: an external request was correlated with a candidate,
  the payload was prepared, queued, and locally flushed.
- `NOT_OBSERVABLE_PROTOCOL_NO_ACK`: the official sync protocol provides no
  remote application acknowledgement.
- A signed A/B observation signal proves remote process transport only.
- Local publish, canonical acceptance, and local relay acknowledgement are not
  remote receipt evidence.

The implementation stores only bounded, hash-blinded counters and latency
buckets. It does not persist raw block or header hashes, peer identities,
addresses, credentials, or block payloads.

## Validation

The adapter was reconstructed against pinned upstream commit
`d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c`.

- `cargo check --lib --bin csd`: passed
- `cargo test --lib`: 75 passed
- `cargo test --bin csd`: 75 passed
- `cargo test --test idle_connection_timeout`: 5 passed
- Patch whitespace validation: passed
- Sensitive-field scan: passed

The idle-connection suite includes the production-duration 130-second,
30-pair transport exercise. No production evidence, credentials, deployment
executors, or sealed logs are part of this repository change.
