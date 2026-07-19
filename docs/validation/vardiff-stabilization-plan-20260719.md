# CSD VarDiff Stabilization Plan - 2026-07-19

Status: code and test candidate only. This change has not been deployed to the
production pool, and no production service was restarted while preparing it.

## Observed baseline

The production read-only snapshot that motivated this change showed:

- 310 active Stratum connections;
- 32,859 accepted shares, 939 `low_difficulty` rejects, and 45 stale shares in
  one hour;
- 16,533 difficulty changes across 32,343 comparable accepted shares, or about
  51% of shares causing a difficulty transition;
- mature V100 workers near 3.0 GH/s and two T4 workers near 1.39 GH/s.

The previous controller doubled or halved difficulty from one share interval.
Poisson share timing therefore caused routine oscillation even when worker
hashrate was stable.

## Candidate behavior

The candidate defaults are:

```toml
initial_difficulty = 16.0
min_difficulty = 8.0
max_difficulty = 512.0
target_share_secs = 20
vardiff_retarget_secs = 120
vardiff_ewma_alpha = 0.25
vardiff_fast_share_ratio = 0.75
vardiff_slow_share_ratio = 1.5
vardiff_max_adjustment_factor = 1.5
vardiff_transition_grace_secs = 15
```

Difficulty 16 targets about 23 seconds per share at 3.0 GH/s. Difficulty 8
targets about 25 seconds per share at 1.39 GH/s. This gives both deployed worker
classes a useful starting point without accepting difficulty below 8.

The controller:

1. bounds individual interval samples before feeding an EWMA;
2. never changes difficulty more often than every 120 seconds;
3. raises difficulty only when the EWMA is below 75% of the target and lowers
   it only when the EWMA is above 150% of the target;
4. limits one adjustment to a factor of 1.5;
5. accepts a finite positive `mining.suggest_difficulty` only before the first
   accepted share and clamps it to the configured range;
6. preserves the prior difficulty for 15 seconds after an upward transition,
   and records a grace-accepted share at that prior difficulty.

The grace is intentionally upward-only. A downward transition already accepts
work produced at the old, harder target and does not need a second validation
path.

## Expected impact

Acceptance targets for the canary are:

- difficulty transitions below 5% of accepted shares;
- `low_difficulty` below 0.5%;
- stale rate no higher than the baseline;
- no reduction in miner-reported hashrate or accepted-difficulty hashrate;
- no change to block-candidate validation or submission behavior.

Reducing `low_difficulty` from the observed 2.8% does not imply a 2.8% increase
in block production. Low-difficulty accounting shares are not equivalent to
network-valid block candidates. The direct benefits are stable accounting,
less protocol churn, cleaner hashrate estimates, and less submission overhead.
Any production uplift must be measured from uninterrupted hashing and
candidate-confirmation evidence rather than inferred from reject counters.

## Staged rollout

1. Keep production unchanged while the full workspace test and release checks
   pass.
2. Capture 30 minutes of baseline data from one V100 and one T4.
3. Deploy the candidate to only those two sessions for at least two hours.
4. Compare accepted difficulty per second, local hashrate, transition count,
   reject reasons, stale count, reconnects, and service errors.
5. Expand to eight V100 workers plus the two T4 workers for at least four hours.
6. Hold a 24-hour observation window before proposing fleet-wide rollout.

Do not use block count alone to judge the short canary because block discovery
is too sparse and variable at one or ten workers.

## Rollback

Keep the previous daemon binary and configuration immutable. Roll back the
canary immediately if any of these conditions holds for 30 minutes:

- accepted-difficulty hashrate is below 95% of its matched baseline;
- `low_difficulty` exceeds 1% after the first retarget window;
- stale exceeds 0.5% or materially exceeds the matched baseline;
- repeated disconnects, malformed protocol responses, daemon restarts, or
  block-candidate submission regressions appear.

Rollback consists of restoring the prior binary and prior `[stratum]`
configuration, restarting only the canary pool instance, and confirming the
old release revision plus accepted-share recovery. A fleet rollout must not
start until canary rollback has been rehearsed.
