# Billing Phase 2 and Phase 4 Runbook

This is the standalone operational runbook for the billing authority fix
(Phase 2) and analytics consistency backfill (Phase 4).

The examples below use the `k8s1/` manifests. If your environment uses `k8s/`
instead, substitute the matching files there.

## Phase Order

Run these in order:

1. Phase 2: `TenantQuota` carry backfill
2. Resume `billing-quota-check`
3. Phase 4: `UsageHourlyByFunc` numerator backfill
4. Rebuild and redeploy both `gateway` and `dashboard`

Do not deploy the analytics code that reads numerator fields until the Phase 4
backfill has completed successfully.

## Phase 2: TenantQuota Carry Backfill

### 1. Apply the updated SQL ConfigMap and CronJob manifest

```bash
kubectl apply -f k8s1/billing-sql-scripts-configmap.yaml
kubectl apply -f k8s1/billing-cronjobs.yaml
kubectl get cronjob billing-quota-check -o jsonpath='{.spec.suspend}{"\n"}'
```

The shared ConfigMap also carries the Phase 4 hourly aggregation changes. The
live `billing-hourly-aggregate` CronJob is safe after this apply because it now
runs the numerator-column migration idempotently before the upgraded
`hourly-aggregate.sql`.

### 2. Confirm no in-flight quota-check Job is still running

Suspending the CronJob stops future schedules, but it does not stop a Job that
is already running.

```bash
kubectl get jobs -o name | grep '^job.batch/billing-quota-check-' || true
kubectl get pods -o name | grep '^pod/billing-quota-check-' || true
```

Wait for any existing `billing-quota-check-*` Job/Pod to finish before
continuing.

### 3. Choose a UTC `BACKFILL_CUTOFF`

Choose a cutoff only after:

- `billing-quota-check` is suspended
- no in-flight quota-check Job/Pod remains
- any delayed/backdated `UsageTick` writers have been drained or stopped

Important behavior:

- ticks with `tick_time < BACKFILL_CUTOFF` are recomputed exactly by the backfill
- included unprocessed ticks are also marked processed
- ticks with `tick_time >= BACKFILL_CUTOFF` are left for normal quota-check
  after resume
- exactness assumes no new `UsageTick` rows with `tick_time < BACKFILL_CUTOFF`
  arrive after the backfill starts

Get the current UTC timestamp in the expected format:

```bash
date -u +"%Y-%m-%dT%H:%M:%SZ"
```

Example output:

```text
2026-04-07T23:24:43Z
```

### 4. Set `BACKFILL_CUTOFF` in the Job manifest

Update `BACKFILL_CUTOFF` in
[k8s1/billing-tenant-quota-backfill-job.yaml](../k8s1/billing-tenant-quota-backfill-job.yaml).

Example in-place replacement:

```bash
CUT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
sed -i "s/REPLACE_WITH_CUTOFF_TIMESTAMP/${CUT}/" k8s1/billing-tenant-quota-backfill-job.yaml
```

Verify the manifest:

```bash
rg -n "BACKFILL_CUTOFF|REPLACE_WITH_CUTOFF_TIMESTAMP" k8s1/billing-tenant-quota-backfill-job.yaml
```

Keep the same cutoff if you need to rerun the backfill.

### 5. Run the one-shot Phase 2 backfill job

```bash
kubectl delete job billing-tenant-quota-backfill --ignore-not-found
kubectl apply -f k8s1/billing-tenant-quota-backfill-job.yaml
kubectl logs -f job/billing-tenant-quota-backfill
kubectl wait --for=condition=complete job/billing-tenant-quota-backfill --timeout=1h
```

That job runs:

- `migrate-tenant-quota-carry.sql`
- `backfill-tenant-quota-carry.sql`
- `verify-tenant-quota-carry.sql`

After verification, it also runs `sync-tenant-quota-exceeded.sh` once to push
corrected `quota_exceeded` states from `TenantQuota` into gateway/etcd before
resume.

### 6. Resume `billing-quota-check`

Only do this after the backfill job succeeds.

```bash
kubectl patch cronjob billing-quota-check -p '{"spec":{"suspend":false}}'
```

If the backfill job fails, do not resume the CronJob. Fix the failure and rerun
the job with the same cutoff.

### Optional: Manual Gateway Reconciliation

If you suspect gateway/etcd drift later, run the one-shot reconciliation Job:

```bash
kubectl delete job billing-tenant-quota-reconcile --ignore-not-found
kubectl apply -f k8s1/billing-tenant-quota-reconcile-job.yaml
kubectl logs -f job/billing-tenant-quota-reconcile
kubectl wait --for=condition=complete job/billing-tenant-quota-reconcile --timeout=1h
```

## Phase 4: UsageHourlyByFunc Numerator Backfill

### 1. Apply the updated SQL ConfigMap

```bash
kubectl apply -f k8s1/billing-sql-scripts-configmap.yaml
```

### 2. Choose a UTC `[BACKFILL_START, BACKFILL_END)` range

Important behavior:

- the job truncates both values to hour boundaries
- after hour truncation, `BACKFILL_START` must still be strictly earlier than
  `BACKFILL_END`
- exact backfill requires raw `UsageTick` rows for the selected range
- if old raw ticks are no longer present, exact backfill is only possible from
  backup data

Practical recommendation:

- choose exact hour-boundary timestamps yourself
- `BACKFILL_START` should be the earliest UTC hour you want to recompute
- `BACKFILL_END` should be the first UTC hour after the last hour you want to
  include, because the range is end-exclusive
- if you want to repair all retained history, a common choice is:
  `BACKFILL_START = oldest retained UsageTick hour`
  `BACKFILL_END = current UTC hour`

Find the oldest retained tick hour:

```sql
SELECT date_trunc('hour', MIN(tick_time)) AS oldest_retained_hour
FROM UsageTick;
```

Get the current UTC hour boundary:

```bash
date -u +"%Y-%m-%dT%H:00:00Z"
```

Example:

- if you want to repair hours `2026-04-01T00:00:00Z` through
  `2026-04-07T22:00:00Z`, set:
  `BACKFILL_START="2026-04-01T00:00:00Z"`
  `BACKFILL_END="2026-04-07T23:00:00Z"`

### 3. Set `BACKFILL_START` and `BACKFILL_END` in the Job manifest

Update
[k8s1/billing-hourly-backfill-job.yaml](../k8s1/billing-hourly-backfill-job.yaml)
before applying it.

The manifest ships with sentinel placeholders. The job exits immediately if:

- placeholders are not replaced
- the truncated range is empty
- the truncated range is reversed

### 4. Run the one-shot Phase 4 backfill job

```bash
kubectl delete job billing-hourly-backfill --ignore-not-found
kubectl apply -f k8s1/billing-hourly-backfill-job.yaml
kubectl logs -f job/billing-hourly-backfill
kubectl wait --for=condition=complete job/billing-hourly-backfill --timeout=1h
```

That job runs:

- `migrate-usage-hourly-by-func-numer.sql`
- `hourly-aggregate-by-func-range.sql`
- `verify-usage-hourly-by-func-numer.sql`

It fails fast on SQL errors. Verification checks both:

- row self-consistency
- completeness against `UsageTick`-derived hourly keys for the selected range

### 5. Rebuild and redeploy both `gateway` and `dashboard`

This follow-up requires redeploying both services:

- `gateway`, because the hourly usage APIs now return exact top-level totals and
  per-hour numerator fields
- `dashboard`, because the analytics UI now aggregates those numerator fields
  when building tenant totals and daily/weekly drill-down rollups

### 6. Validate billing summary vs analytics on the same tenant and same time window

After the redeploy, validate a sample of tenants and time windows against exact
raw-tick recomputation before considering Phase 4 complete.

That validation should include:

- value checks: recompute numerator totals from `UsageTick` and confirm they
  match `UsageHourlyByFunc`
- completeness checks: confirm no expected grouped hourly rows are missing from
  `UsageHourlyByFunc` for the selected backfill range

If the hourly backfill job fails, do not rely on the new analytics totals yet.
Fix the failure and rerun the job for the same range.
