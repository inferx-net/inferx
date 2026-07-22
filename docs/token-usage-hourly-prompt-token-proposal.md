# Token Usage Hourly Semantics Proposal

> Status: proposal for review.
> Date: 2026-07-22

`k8s1/` is the deployment target we maintain going forward. Today it is a
truncated copy of `k8s/` and is missing the token-billing backfill and
verification assets described in Phase 0; those must be ported from `k8s/`
first. After Phase 0, `k8s/` is not maintained for this work and can be
ignored.

## Summary

Change `TokenUsageHourly.input_tokens` to store hourly summed `prompt_tokens`
instead of the current pricing-dependent "billable input" value.

Keep billing exact by continuing to store and use:

- `input_numer`
- `cached_numer`
- `output_numer`
- `charge_cents`

This separates **usage truth** from **billing math**.

## Problem

Today, `TokenUsageEvent` stores raw model-reported usage:

- `prompt_tokens`
- `cached_tokens`
- `completion_tokens`

But `TokenUsageHourly.input_tokens` does not consistently mean raw prompt
tokens. It is currently computed as:

- `prompt_tokens - cached_tokens` when cached tokens have a positive rate
- full `prompt_tokens` when cached-token rate is `0`

That makes `input_tokens`:

- rate-dependent
- not a true usage count
- inconsistent with common API semantics, where prompt/input tokens include the
  cached subset
- confusing in reporting, especially when token rates change mid-hour

An hourly usage table should preserve actual token counts regardless of billing
configuration.

## Proposal

Redefine `TokenUsageHourly.input_tokens` to mean:

- `SUM(prompt_tokens)` for the hour

In other words, use the existing `input_tokens` column to store raw prompt
token totals.

Billing remains separate:

- `billable_input_tokens` remains a separate intermediate expression in the
  rollup/overlay logic and continues to mean the current rate-aware billing
  basis
- `input_numer` continues to hold `SUM(billable_input_tokens * input_rate)`
- `cached_numer` continues to hold `SUM(cached_tokens * cached_rate)`
- `output_numer` continues to hold `SUM(output_tokens * output_rate)`
- `charge_cents` continues to be derived from the numerators

This keeps the usage-count column truthful while preserving exact event-time
pricing.

Concretely, this is not a one-line substitution. The rollup and recent-overlay
queries must carry two distinct expressions:

- raw prompt usage for storage in `input_tokens`
- billable input for `input_numer` and `charge_cents`

Pseudo-shape:

```sql
WITH event_with_rate AS (
  SELECT
    e.caller_tenant AS tenant,
    e.model_slug,
    date_trunc('hour', e.ts) AS hour,
    e.prompt_tokens AS prompt_input_tokens,
    CASE
      WHEN r.cents_per_million_cached = 0
        THEN e.prompt_tokens
      ELSE GREATEST(e.prompt_tokens - e.cached_tokens, 0::bigint)
    END AS billable_input_tokens,
    e.cached_tokens,
    e.completion_tokens AS output_tokens,
    r.cents_per_million_input,
    r.cents_per_million_cached,
    r.cents_per_million_output
  FROM TokenUsageEvent e
  CROSS JOIN LATERAL GetTokenRateCents(e.model_slug, e.ts, e.caller_tenant) r
)
SELECT
  SUM(prompt_input_tokens) AS input_tokens,
  SUM(billable_input_tokens * cents_per_million_input) AS input_numer,
  ...
```

Without that split, changing `input_tokens` to prompt totals would silently
change charges.

Note that the recent-overlay query and the verifier each effectively contain
the billable-input `CASE` twice: once for the displayed/stored usage count and
once inside billing math. Only the usage-count occurrence changes. The billing
occurrence must remain unchanged.

## Why This Design

### 1. Usage columns should be rate-independent

A token-count column should not change meaning when pricing changes.

### 2. Mid-hour rate changes still price correctly

If a token rate changes in the middle of an hour, the hourly row can still be
priced exactly because billing is already accumulated from per-event rated
numerators before the hourly aggregation.

### 3. UI/API semantics become simpler

Once `input_tokens` always means raw prompt tokens, the token usage page can
show a stable definition:

- `Input Tokens` or preferably `Prompt Tokens`
- `Cached Tokens`
- `Output Tokens`

Any derived billing-oriented token view can be computed separately, for example:

- `non_cached_tokens = input_tokens - cached_tokens`

This is only a usage-oriented view. It is not a billing substitute when the
cached-token rate is `0`, because billable input in that case remains the full
prompt.

## Required Changes

### 1. Hourly rollup SQL

Update the hourly aggregation jobs so that:

- raw prompt usage and billable input are computed as separate expressions
- `input_tokens = SUM(prompt_input_tokens)`
- `input_numer = SUM(billable_input_tokens * input_rate)`

instead of the current rate-dependent logic.

This applies to:

- `k8s1/billing-sql-scripts-configmap.yaml`
- the range/backfill variants that must first be added to `k8s1/` during
  Phase 0

### 2. Recent 3-hour overlay

Update the live overlay query in `ListTokenUsageHourly` so that recent rows also
carry two separate expressions:

- `SUM(prompt_input_tokens) AS input_tokens`
- billing continues to use `billable_input_tokens` for `input_numer` and
  `charge_cents`

This keeps recent hours and historical hours aligned.

Concrete edit site:

- `ixshare/src/audit.rs` in `ListTokenUsageHourly`

Implementation note:

- the overlay currently contains the rate-aware billable-input `CASE` twice
- the usage-count occurrence at the displayed `input_tokens` aggregation changes
  to raw prompt totals
- the billing occurrence inside `charge_cents` must remain rate-aware

### 3. Reconciliation verifier

Update `verify_token_usage_hourly_state` so its expected hourly usage count is
recomputed with the new semantics:

- expected `input_tokens = SUM(prompt_tokens)`

It must no longer compare the stored hourly usage count against the old
rate-dependent billable-input expression.

Its billing expectations must stay unchanged. In particular, expected
`input_numer` must continue to be computed from the existing rate-aware
billable-input expression.

This change must land with the rollup change; otherwise the verifier will flag
every changed row immediately.

Concrete edit site:

- `verify-token-usage-hourly.sql` added to `k8s1/` in Phase 0

Implementation note:

- the verifier effectively carries the same billable-input logic twice
- expected `input_tokens` changes to `SUM(prompt_tokens)`
- expected `input_numer` stays computed from the existing rate-aware
  billable-input expression

### 4. Schema comments and docs

Update comments and design docs that currently describe
`TokenUsageHourly.input_tokens` as billable input.

### 5. UI labels

If the API field name remains `input_tokens`, the UI should preferably display
it as `Prompt Tokens` to avoid ambiguity with billable input.

In this repo, the current render sites include:

- `dashboard/templates/admin.html`
- `dashboard/templates/_admin_token_usage.html`

Resolved UI decision:

- the analytics views will relabel `Input Tokens` to `Prompt Tokens`
- the billing table in `dashboard/templates/admin.html` will also relabel
  `Input Tokens` to `Prompt Tokens`
- after this change, that billing-table column will intentionally show raw
  prompt usage rather than a billing-derived token count

### 6. Historical backfill

Existing `TokenUsageHourly.input_tokens` rows were written using the old
semantics and must be backfilled from `TokenUsageEvent`:

- recompute each hour as `SUM(prompt_tokens)`

without changing:

- `input_numer`
- `cached_numer`
- `output_numer`
- `charge_cents`

That invariant must be verified explicitly.

## Compatibility Impact

This change is semantically breaking for any downstream consumer that currently
interprets hourly `input_tokens` as billable non-cached input.

However, that existing semantic is already problematic because it is not stable
across pricing configurations.

Consumers that need billing-oriented values should rely on:

- `charge_cents`
- `input_numer`
- `cached_numer`
- `output_numer`

or on an explicitly named derived field added later, such as:

- `billable_input_tokens`

## Rollout Plan

### Phase 0: Bring `k8s1/` to parity

Before changing hourly token semantics, make `k8s1/` capable of running the
same token-billing backfill and verification flow that already exists in `k8s/`.

Required parity work:

1. Append these scripts to `k8s1/billing-sql-scripts-configmap.yaml`:
   - `token-hourly-aggregate-range.sql`
   - `verify-token-usage-hourly.sql`
2. Port the missing token-billing steps into
   `k8s1/billing-hourly-backfill-job.yaml`:
   - invoke `/scripts/migrate-token-billing.sql`
   - run `/scripts/token-hourly-aggregate-range.sql`
   - run `/scripts/verify-token-usage-hourly.sql`
3. Replace the hardcoded backfill dates in
   `k8s1/billing-hourly-backfill-job.yaml` with:
   - `REPLACE_WITH_START_TIMESTAMP`
   - `REPLACE_WITH_END_TIMESTAMP`
4. Run the verifier under current semantics and confirm `k8s1/` reconciles
   cleanly before the semantics migration starts.

Verified on July 22, 2026 against the live billing DB: the current data already
reconciles cleanly under existing semantics. All verifier mismatch counters are
zero, and per-tenant `token_used_cents * 1000000 + token_carry_numer` matches
the summed hourly numerators exactly. Phase 0 step 4 still needs to run through
the real `k8s1/` verifier path so that any failure points to the port, not to
the data.

That baseline matters. If the semantics change and the verifier are both new in
`k8s1/`, a failure is ambiguous between a bad migration and unproven parity
tooling.

### Phase 1: Semantics migration

1. Suspend the `billing-hourly-aggregate` CronJob before deploying any
   semantics change.

   Rationale:
   - `billing-hourly-aggregate` continues to write rows on its schedule
   - if it fires after the new config is deployed but before historical
     backfill completes, recent hours can be rewritten with new semantics while
     older hours still carry old semantics
   - that is repairable, but the rollout should avoid creating the mixed state
     in the first place

2. With the CronJob already suspended, capture a snapshot of the existing
   hourly billing outputs keyed by `(tenant, model_slug, hour)`:
   - `input_numer`
   - `cached_numer`
   - `output_numer`
   - `charge_cents`

   This can be done with a temporary table or a dumped CSV. The existing
   verifier checks hourly rows against raw events; it does not by itself prove a
   before/after invariant on stored numerators. The snapshot must be taken only
   after the CronJob is suspended so the capture cannot race an overlapping
   hourly rewrite.

3. Update the four concrete Phase 1 edit sites together:
   - `token-hourly-aggregate.sql` in `k8s1/billing-sql-scripts-configmap.yaml`
   - `token-hourly-aggregate-range.sql` ported into `k8s1/` during Phase 0
   - `verify-token-usage-hourly.sql` ported into `k8s1/` during Phase 0
   - `ixshare/src/audit.rs` in `ListTokenUsageHourly`
4. In the two hourly rollup SQL files, split the current billable-input `CASE`
   into two expressions:
   - `prompt_input_tokens = e.prompt_tokens`
   - `billable_input_tokens = CASE ... END`
   Then:
   - store `SUM(prompt_input_tokens)` into `input_tokens`
   - keep pricing `input_numer` from `SUM(billable_input_tokens * input_rate)`
5. In the overlay and verifier, change only the usage-count occurrence of the
   billable-input logic:
   - overlay `input_tokens` becomes `SUM(e.prompt_tokens)`
   - verifier expected `input_tokens` becomes `SUM(prompt_tokens)`
   - overlay `charge_cents` and verifier expected `input_numer` stay unchanged
6. Backfill historical `TokenUsageHourly.input_tokens` from
   `TokenUsageEvent.prompt_tokens` immediately after the logic change.
7. Verify as a release gate that:
   - the event-vs-hourly verifier passes under the new semantics
   - the before/after snapshot diff shows `input_numer`, `cached_numer`,
     `output_numer`, and `charge_cents` are byte-identical for every
     `(tenant, model_slug, hour)`
   - only stored `input_tokens` changed
8. Resume the `billing-hourly-aggregate` CronJob after the backfill and release
   gate succeed.
9. Update schema comments and docs.
10. Apply the UI changes:
   - relabel analytics views from `Input Tokens` to `Prompt Tokens`
   - relabel the billing-table token-count column to `Prompt Tokens`

The table must not sit in a mixed-semantics state longer than the rollout
window.

## Implementation Checklist

Items marked `[x]` are the in-repo code/config changes, all landed together so
the PR carries the final target state. Items left `[ ]` are live-cluster
operational steps for the rollout operator (apply, snapshot, backfill, verify,
suspend/resume the CronJob); they cannot be done from the repo and must be run
at deploy time in the documented order. Because the SQL scripts already carry
the new prompt-token semantics, the Phase 0 "verifier under current semantics"
gate is superseded by the Phase 1 new-semantics verifier run.

### Phase 0 checklist

- [x] Append `token-hourly-aggregate-range.sql` to
      `k8s1/billing-sql-scripts-configmap.yaml`
- [x] Append `verify-token-usage-hourly.sql` to
      `k8s1/billing-sql-scripts-configmap.yaml`
- [x] Port `/scripts/migrate-token-billing.sql` invocation into
      `k8s1/billing-hourly-backfill-job.yaml`
- [x] Port `/scripts/token-hourly-aggregate-range.sql` invocation into
      `k8s1/billing-hourly-backfill-job.yaml`
- [x] Port `/scripts/verify-token-usage-hourly.sql` invocation into
      `k8s1/billing-hourly-backfill-job.yaml`
- [x] Replace hardcoded `BACKFILL_START` with
      `REPLACE_WITH_START_TIMESTAMP` in
      `k8s1/billing-hourly-backfill-job.yaml`
- [x] Replace hardcoded `BACKFILL_END` with `REPLACE_WITH_END_TIMESTAMP` in
      `k8s1/billing-hourly-backfill-job.yaml`
- [ ] Apply the `k8s1` configmap/job changes in the target cluster
- [ ] Run the `k8s1` verifier under current semantics
- [ ] Confirm all verifier mismatch counters are zero before Phase 1

### Phase 1 checklist

- [x] Relabel the billing-table token-count column in
      `dashboard/templates/admin.html` to `Prompt Tokens`
- [ ] Suspend the `billing-hourly-aggregate` CronJob
- [ ] Capture a pre-change snapshot keyed by `(tenant, model_slug, hour)` of:
      `input_numer`, `cached_numer`, `output_numer`, `charge_cents`
- [x] Update `token-hourly-aggregate.sql` in
      `k8s1/billing-sql-scripts-configmap.yaml`
- [x] Update `token-hourly-aggregate-range.sql` in
      `k8s1/billing-sql-scripts-configmap.yaml`
- [x] Update `verify-token-usage-hourly.sql` in
      `k8s1/billing-sql-scripts-configmap.yaml`
- [x] Update `ixshare/src/audit.rs` `ListTokenUsageHourly` recent-overlay logic
- [x] Ensure rollup SQL stores `SUM(prompt_input_tokens)` into
      `TokenUsageHourly.input_tokens`
- [x] Ensure rollup SQL still computes `input_numer` from
      `billable_input_tokens * input_rate`
- [x] Ensure overlay `input_tokens` changes to `SUM(e.prompt_tokens)`
- [x] Ensure overlay `charge_cents` remains rate-aware and unchanged in meaning
- [x] Ensure verifier expected `input_tokens` changes to `SUM(prompt_tokens)`
- [x] Ensure verifier expected `input_numer` remains rate-aware and unchanged in
      meaning
- [ ] Deploy the Phase 1 code/config changes
- [ ] Run manual historical backfill over the full retained hourly range with:
      `BACKFILL_START=2026-07-04T21:00:00Z` and `BACKFILL_END` set to an
      end-exclusive timestamp past `2026-07-22T17:00:00Z` using the current
      hour at run time
- [ ] Run the event-vs-hourly verifier under new semantics
- [ ] Diff post-change hourly rows against the pre-change snapshot
- [ ] Confirm only `input_tokens` changed
- [ ] Confirm `input_numer`, `cached_numer`, `output_numer`, and
      `charge_cents` are byte-identical for every `(tenant, model_slug, hour)`
- [ ] Resume the `billing-hourly-aggregate` CronJob
- [x] Update schema comments and design docs
- [x] Update analytics UI labels from `Input Tokens` to `Prompt Tokens`
- [x] Relabel the billing-table token-count column in
      `dashboard/templates/admin.html` to `Prompt Tokens`

## Non-Goals

This proposal does not require:

- changing raw `TokenUsageEvent`
- changing billing numerators
- changing event-time pricing behavior
- recomputing spend from hourly token counts
- changing the pass-1 debit rollup, which prices events into numerators but does
  not store hourly token-count semantics

## Data Retention

Verified on July 22, 2026 against the live billing DB:

- `TokenUsageEvent` covers `2026-07-04 21:47Z` to `2026-07-22 16:47Z`
- `TokenUsageHourly` covers `2026-07-04 21:00Z` to `2026-07-22 16:00Z`
- the ranges match, with 50 hourly rows and no historical pruning

That means the entire current hourly history is backfillable from raw events,
so no cutoff or forced new-column migration is required for this rollout.

Re-confirm this if retention policy changes before rollout.

## Recommended Follow-Up

If we later want both usage and billing token views in the API, add a separate
derived field with an explicit name, for example:

- `billable_input_tokens`

But the core fix should be to make the stored hourly token counts always reflect
actual model-reported usage.

An alternative migration strategy is to add a new `prompt_tokens` column and
deprecate `input_tokens` later. That is a larger schema/API migration, but it
avoids reusing a column whose historical meaning changed over time. This
proposal keeps the in-place redefinition because it is the smaller operational
change. Revisit that tradeoff only if downstream consumers turn out to depend
on the old `input_tokens` semantics more heavily than expected.
