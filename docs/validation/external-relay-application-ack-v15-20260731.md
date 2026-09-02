# External Relay Application ACK V15

## Decision

The existing names V13 and V14 are occupied by full-block delivery-attempt
observability and candidate propagation timeline telemetry, respectively. The
new remote application-accept protocol is therefore V15.

The current official adapter proves only `ANNOUNCE_THEN_PULL` transport
capability: an external peer can request a block with `GetBlock`, and the local
node can observe that its response was flushed. It has no full-block PUSH
request and no remote application ACK. V15 does not reinterpret either event.

## Receipt definition

`remote_application_receipt_observed` may become true only after the sender
verifies a dedicated relay signature over all of the following:

- protocol version and delivery mode;
- sender role and anonymous relay slot;
- candidate correlation and generation;
- block-content digest;
- nonce and expiry;
- block validation and application acceptance results.

The relay signs only after its full node validates the complete block and its
application layer accepts it. Node identity, wallet, and payment signer keys
are forbidden as ACK keys.

Each delivery records pull-request, payload-complete, application-accept, and
terminal stages at most once. Replay capacity never evicts an unexpired ACK
token: expired entries are cleared first, and a still-full guard rejects new
ACKs fail-closed.

## Current block

Deployment is blocked because no two independent external full-node relay
hosts or dedicated relay keys have been supplied, and the pinned official
adapter lacks a delayed relay-response channel plus an application-accept
callback. The local protocol core and gate deliberately refuse to produce a
deployable target while those inputs are absent.

## Canary contract

Once the blocked inputs are independently sealed, the future canary changes
one variable: enable V15 delivery on A/B while preserving the V13 baseline.
Both A and B schedule bounded, asynchronous delivery to relay D and E after
local canonical persistence. A candidate-submit path never waits for V15.

The first observation window remains pending below 10 mature V15 candidates.
At or above 10 it is only a bounded comparison against an equal predecessor
window; it cannot prove causal orphan improvement. Two consecutive orphans,
negative/expired/invalid ACKs, transport timeouts, counter non-conservation,
persistent A/B lag, or submit/hashrate/generation redlines stop the feature and
restore the V13 flags without rolling back V13 itself.

## Relay independence envelope

Each relay should have at least 4 dedicated vCPU, 8 GiB RAM, 250 GiB SSD/NVMe,
100 Mbps symmetric connectivity, monitored clock synchronization, and enough
headroom for full validation without swap. D and E must differ in geographic
region, network provider/ASN, cloud account or operator, fault domain, host,
storage, ACK key, and failure budget. Neither may be A, B, C, a miner, or a
wallet/payment host.
