# External Relay Application ACK V15

V15 is a local-only protocol core and deployment guard for two independent
external full-node relays. It does not change or replace the deployed V13 node
binary. V14 is already reserved by the candidate propagation timeline trial,
so this feature intentionally uses a new version.

## Proven adapter boundary

The pinned V13 official adapter supports `SyncRequest::GetBlock` and can
observe a local `ResponseSent`. It does not support full-block push and does
not expose a callback that binds a delayed response to successful local block
application. Therefore the only compatible current design is
`ANNOUNCE_THEN_PULL`, and deployment remains blocked until the official
adapter gains both of these explicit hooks:

1. a bounded delayed response channel for `/csd/external-relay-receipt/15`;
2. an application-accept callback emitted only after full block validation and
   canonical application on the relay.

The protocol core models PUSH for future capability negotiation, but the
current capability fixture rejects it. It never treats local canonical state,
gossip publication, A/B signed signals, `GetBlock`, or a local payload flush as
a remote application receipt.

## Safety boundary

- The feature is disabled unless both relay slots D and E are configured.
- Relay ACK keys are dedicated Ed25519 keys, separate from node identity,
  wallet, and payment signer keys.
- ACKs bind version, mode, sender role, relay slot, correlation, generation,
  content digest, nonce, expiry, validation, and application acceptance.
- Replay, expiry, wrong direction, wrong content, wrong generation, wrong key,
  and negative application results fail closed.
- The replay guard removes only expired entries. A full guard rejects a new
  ACK and never evicts an unexpired token.
- Pull-request, payload-complete, application-accept, and terminal counters are
  idempotent per delivery, including duplicate callback delivery.
- Candidate delivery is asynchronous, bounded, idempotent, and fail open with
  respect to template production, candidate persistence, candidate submit, and
  the existing C relay.
- Snapshots expose only lane counters and latency buckets. Raw block/hash/tip,
  peer identity/address, credentials, and payment material are not emitted.

## Local validation

```bash
cargo test --manifest-path ops/csd-node-adapter/v15/Cargo.toml
python3 ops/bin/csd-pool-external-relay-v15-canary-gate-self-test.py
```

The deployment guard remains `BLOCKED_EXTERNAL_INPUTS` until two genuinely
independent relay hosts, their dedicated public keys, and the missing official
adapter hooks are independently sealed.
