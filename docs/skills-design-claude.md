# Skills System Design

## Overview

Skills are cross-tenant callable resources hosted by InferX. A skill wraps an InferX-managed model with a prefix (system prompt / context) that gets prepended to every user request. Any authenticated user can call any published skill regardless of which tenant owns it.

Skills differ from regular funccall endpoints in three ways:
- Cross-tenant accessible (any authenticated user can call)
- InferX owns the underlying model snapshot (no user/owner deployments)
- Separate billing attribution between caller and skill owner

---

## Rollout Phases

| Phase | Scope | Key additions |
|---|---|---|
| **R1** | Free skills, no cache, dedicated instance | Routing, access control, `is_published` flag |
| **R2** | Non-free skills, no cache, dedicated instance | Per-run charging, gateway request counter buffer, `OwnerCreditLedger` |
| **R3** | Skills with cache, dedicated instance | Producer/consumer functions, LMCache/NodeAgent integration, `SkillCacheRetentionCharge` |
| **R4** | Skill versioning | Multi-revision API, promote/rollback, `X-Skill-Revision` header, optional `status` field |
| **R5** | Shared instance | FuncAgentMgr key changes, deferred |

---

## What is a Skill

```
Skill = prefix_content + InferX-managed model + billing config
```

When a user calls a skill:
1. Gateway looks up the skill record → gets prefix, model, billing config
2. Gateway injects `prefix_content` as the leading system message (merging with any caller-supplied system messages)
3. Dispatches to the InferX-managed physical model
4. Bills caller and owner according to billing config

The skill owner defines the prefix and billing. InferX manages all infrastructure.

---

## URL Structure

```
POST /skills/{owner_tenant}/{namespace}/{skillname}/v1/chat/completions
POST /skills/{owner_tenant}/{namespace}/{skillname}/v1/completions
```

- OpenAI-compatible path (`/v1/chat/completions`) appended after skill identity
- `owner_tenant` always explicit in the URL
- Serving mode (shared/dedicated) is a skill property — NOT in the URL

To target a specific revision (owner testing only), pass a header — **R4**:

```
X-Skill-Revision: 2
```

Using a header rather than a query parameter avoids URL-construction issues with OpenAI-compatible clients that append `/v1/chat/completions` to a configured base URL. The gateway reads and strips `X-Skill-Revision` before forwarding to vLLM — the model never sees it.

**R1**: owner tests by calling the skill while `is_published=false` (owner-only access). No revision promotion needed — there is exactly one revision per skill. When ready, set `is_published=true`.

### Routing

```
-- funcname  = has_cache ? consumer_funcname  : normal_funcname   (from SkillTemplate)
-- func_rev  = consumer_revision when has_cache = true; omitted entirely when has_cache = false
-- billing   = gpu_billing_target on the Skill record ('caller' or 'owner')

-- has_cache = false (R1, R2):
logical  (shared):                    skills/{owner_tenant}/{func_tenant}.{func_namespace}.{funcname}
logical  (dedicated, billing=caller): skills/{calling_tenant}/{func_tenant}.{func_namespace}.{funcname}
logical  (dedicated, billing=owner):  skills/{owner_tenant}/{func_tenant}.{func_namespace}.{funcname}.{calling_tenant}

-- has_cache = true (R3+):
logical  (shared):                    skills/{owner_tenant}/{func_tenant}.{func_namespace}.{funcname}.{func_rev}
logical  (dedicated, billing=caller): skills/{calling_tenant}/{func_tenant}.{func_namespace}.{funcname}.{func_rev}
logical  (dedicated, billing=owner):  skills/{owner_tenant}/{func_tenant}.{func_namespace}.{funcname}.{func_rev}.{calling_tenant}

physical (always):    {func_tenant}/{func_namespace}/{funcname}
```

The logical key branches on `gpu_billing_target`, not `earning_type` — the two are orthogonal. `{calling_tenant}` is always the last segment when present. Gateway parses `caller_tenant` from this suffix when needed; no other layer sees or parses the logical key.

**Default `gpu_billing_target` when not specified in the create request:**
- `earning_type = 'free'` → `'caller'` (caller pays GPU)
- `earning_type = 'per_run'` → `'owner'` (owner pays GPU, recovers cost from per-run fee)

Owner can override either default explicitly.

The `skills/` prefix is a fixed top-level identifier — not a tenant or namespace name — so it cannot collide with any user-created resource paths. No namespace reservation needed.

The logical path determines which `FuncAgentMgr` instance the gateway uses. The scheduler never sees the logical path — it only receives the physical FuncPath and is responsible for resuming standby pods when it gets a LeaseWorker request.

- **Shared**: one `FuncAgentMgr` per `skills/{owner_tenant}/...` — different skills from the same owner on the same model share one instance; different owners are isolated so `UsageTick.tenant = owner_tenant` is naturally correct
- **Dedicated (billing=caller)**: one `FuncAgentMgr` per `skills/{calling_tenant}/...` — `UsageTick.tenant = calling_tenant`; caller pays GPU
- **Dedicated (billing=owner)**: one `FuncAgentMgr` per `skills/{owner_tenant}/...{calling_tenant}` — `UsageTick.tenant = owner_tenant`; owner pays GPU; per-caller pool isolation comes from the `{calling_tenant}` suffix in the funcname, not from the tenant segment

The scheduler and pod layer are identical in both modes — same physical FuncPath, same standby pods. GPU billing correctness is maintained at the tenant level via `UsageTick.tenant`, not at the per-skill level.

For `billing=caller`, the logical key is `skills/{calling_tenant}/{physical_target}` — multiple skills from the same caller on the same backing function share one worker pool. For `billing=owner`, the key is `skills/{owner_tenant}/{physical_target}.{calling_tenant}` — each caller gets a distinct pool, but multiple skills from the same caller on the same backing function still share one pool. In both cases GPU time cannot be attributed exactly to a specific `skill_id` from `UsageTick`; per-skill accounting must use request-level counting (`SkillRequestCount`), not GPU-time ticks.

---

## Serving Modes

| | Shared | Dedicated |
|---|---|---|
| `FuncAgentMgr` (gateway) | One instance per (owner tenant + physical target) | billing=caller: per (calling tenant + physical target); billing=owner: per (owner tenant + physical target + calling tenant suffix) |
| Request queue isolation | Skills from same owner on same model share one queue; different owners isolated | billing=caller: each caller isolated by tenant; billing=owner: each caller isolated by funcname suffix |
| Scheduler / pods | Same physical FuncPath — no difference | Same physical FuncPath — no difference |
| GPU billing | Owner always pays GPU time | billing=caller: caller pays; billing=owner: owner pays |
| `UsageTick.tenant` | owner_tenant | billing=caller: calling_tenant; billing=owner: owner_tenant |

Switching `serving_mode` only changes the logical key used for `FuncAgentMgr` lookup on the gateway. The scheduler and standby pods are unaffected — no migration, no restart. The change takes effect on the next request.

Serving mode change is deferred to R5 (shared instance support). R1–R4 are dedicated only. R5 requires three small gateway changes — API gate removal, logical key branch, and enforcing `gpu_billing_target = 'owner'` for shared — since the billing infrastructure from R2 already handles shared GPU attribution correctly.

### Branch Summary

All gateway branching on a skill request collapses to three cases. Every field in each row is determined before dispatch.

| Case | `logical.tenant` | `logical.funcname` suffix | `UsageTick.tenant` | `caller_tenant` | Quota check target |
|---|---|---|---|---|---|
| dedicated, billing=caller | calling_tenant | _(none)_ | calling_tenant | NULL | calling_tenant |
| dedicated, billing=owner | owner_tenant | `.{calling_tenant}` | owner_tenant | calling_tenant | owner_tenant |
| shared | owner_tenant | _(none)_ | owner_tenant | NULL | owner_tenant |

Notes:
- `logical.funcname` base is always `{func_tenant}.{func_namespace}.{funcname}[.{func_rev}]` before any suffix
- Shared always has `gpu_billing_target = 'owner'`; `billing=caller` is rejected at create time for shared
- Quota check skipped for InferX admin tokens and quota-exempt tenants
- `earning_type` does not affect routing or billing target — it only controls whether a per-run fee is charged (R2+)

### Billing Tenant for Dedicated Instances (R2 — Code Change Required)

`UsageTick.tenant` is set from `FuncWorkerInner.tenant`, which is the tenant segment of the logical key. For paid dedicated the logical key is `skills/{owner_tenant}/...{calling_tenant}`, so `FuncWorkerInner.tenant = owner_tenant` — no extra field needed. The existing tick functions (`insert_start_billing_tick`, `insert_periodic_billing_tick`, `insert_final_billing_tick`) in `func_worker.rs` already use `tenant: self.tenant.clone()` and require no change.

**Changes to skill request handler (`http_gateway.rs`):**

When constructing `FuncRouteTarget` for a paid dedicated skill, use `owner_tenant` as the logical tenant and append `calling_tenant` to the logical funcname:
```rust
let (logical_tenant, logical_funcname) = if skill.earning_type == "free" {
    (calling_tenant.clone(),
     format!("{}.{}.{}", skill.func_tenant, skill.func_namespace, physical_funcname))
} else {
    (skill.owner_tenant.clone(),
     format!("{}.{}.{}.{}", skill.func_tenant, skill.func_namespace, physical_funcname, calling_tenant))
    // with cache: format!("{}.{}.{}.{}.{}", ..., consumer_revision, calling_tenant)
};
```

For shared mode `logical.tenant = owner_tenant` with no `calling_tenant` suffix — no change to existing shared-mode code.

---

## Access Control

- `is_published = false` — skill is in draft/testing; only the `owner_tenant` token can call; non-owners get 404 (don't leak existence)
- `is_published = true` — any authenticated token can call; `AllowTenant()` is skipped

The `/skills` handler skips `AllowTenant()` for published skills — skills bypass tenant isolation by design via a dedicated route entry point. Existing `AllowTenant` logic is untouched.

**Publish gate for cached skills**: if `has_cache = true` and `cache_status != 'ready'`, the publish request is rejected with 409. Owner must wait for cache generation to complete before publishing.

Allowlist access control (per-tenant grants) is deferred to a future release.

---

## File Storage

```
/opt/inferx/skills/{owner_tenant}/{owner_namespace}/{skillname}/{version}/skill.data
/opt/inferx/kvcache/tenants/{owner_tenant}/{owner_namespace}/{skillname}/{version}/{hash}.data

/opt/inferx/kvcache/hash/{template_id}/{hash} → symlink to tenant cache folder (dedup index)
```

- `skill.data` — system prompt (prefix content)
- `{hash}.data` — KV cache produced by the producer; hash is `sha256(system_prompt)`
- **Cache loading**: NodeAgent uses the known path pattern to locate the cache file and loads it into LMCache on the consumer pod (at pod start or when a new cache is ready)
- **Cache lookup at inference time**: gateway injects the skill prefix as the system message (same as request flow step 6) and dispatches normally; LMCache on the consumer pod matches the prefix by content hash automatically — no cache path is passed by the gateway
- Hash dedup index: before running producer, check if symlink exists under `hash/{template_id}/{hash}`; if yes, reuse existing cache instead of running producer
- Delete skill revision → remove tenant cache folder; remove symlink from hash index if no other skill references the same hash

---

## Prefix Cache

- Disk-persisted KV prefix cache for the skill's system prompt
- Tied to `SkillRevision.template_id` (specifically `producer_revision` / `consumer_revision`) — invalid if either changes
- `has_cache` boolean on `SkillRevision` determines whether cache is used
- Cache retention fee: $1/skill/month (future: size-based tiers)
- Cache retention charged to owner when owner has billing relationship with InferX

---

## Versioning

End users always get the active revision. Owner-selectable revision targeting is deferred to R4.

```
All callers (R1+):  /skills/{owner_tenant}/{ns}/{skillname}/v1/chat/completions  → active_revision_id
Owner testing (R4):  same URL + X-Skill-Revision: 2 header             → SkillRevision.version=2
```

**R1**: versioning is internal only — not exposed to skill owners. Each skill always has exactly one `SkillRevision` (version=1). To update a skill, the owner deletes and recreates it (which creates a new version=1 under the new skill). The `SkillRevision` table and `{version}` segment in storage paths are kept version-aware so R4 can add multi-revision support with no schema or storage migration.

**R4**: expose versioning to owners. All work is additive API — no schema or data migration needed:
1. `POST /skills/{owner}/{ns}/{skillname}/revisions` — create a new revision on an existing skill (increments version); original create endpoint unchanged
2. `GET  /skills/{owner}/{ns}/{skillname}/revisions` — list all revisions
3. Promote endpoint to update `Skill.active_revision_id` (roll forward or roll back)
4. `X-Skill-Revision: N` header (owner-only) for testing a specific revision without affecting live traffic
5. Optional: add `status` field (draft/deprecated/yanked) to `SkillRevision`

---

## Billing Model

### Dedicated Instance

GPU billing is controlled by `gpu_billing_target` on the Skill:
- `'caller'` — GPU billed to end user (`UsageTick.tenant = calling_tenant`). Natural default when `earning_type = 'free'`.
- `'owner'`  — GPU billed to owner (`UsageTick.tenant = owner_tenant`). Natural default when `earning_type = 'per_run'`.

Owner can override the default — e.g. a free skill where the owner subsidizes GPU, or a non-free skill where the user still pays GPU on top of the royalty.

| | `gpu_billing_target = caller` | `gpu_billing_target = owner` |
|---|---|---|
| **User pays** | GPU time + owner earning (if not free) | Owner earning only (if not free) |
| **Owner pays** | Cache retention (if cached) | GPU time + cache retention (if cached) |
| **Owner earns** | Nothing (if free) / earning fee (if not free) | Nothing (if free) / earning fee (if not free) |
| **InferX earns** | GPU time + retention | GPU time + retention − owner's share (if not free) |

### Shared Instance

Owner pays GPU time. Only usage-based earning models are viable — fixed-fee models (monthly/one-time) create margin risk when the owner has variable GPU costs.

| | Skill is free | Skill is not free |
|---|---|---|
| **User pays** | Nothing extra | Per-run royalty |
| **Owner pays** | GPU time + cache retention (if cached) | GPU time + cache retention (if cached) |
| **Owner earns** | Nothing | Royalty minus InferX cut |
| **InferX earns** | GPU time + retention | GPU time + retention + cut of royalty |

### Earning Model Applicability

| Earning model | Dedicated | Shared | Rollout phase |
|---|---|---|---|
| Per-run (per request) | ✓ | ✓ | R2 |
| Token-based royalty | ✓ | ✓ (recommended) | Deferred |
| Monthly license | ✓ | ✗ (variable GPU cost risk) | Deferred |
| One-time fee | ✓ | ✗ (variable GPU cost risk) | Deferred |

**R1**: `earning_type = 'free'` only — no per-run charging, no `OwnerCreditLedger` writes.
**R2**: adds `earning_type = 'per_run'`; `user_price_microcents` is the per-request charge.

### Revenue Flow (R2 — Credit-Based)

```
User pays InferX per-run fee (user_price_microcents) per request
InferX keeps platform cut (inferx_revenue_share_pct, default 0%)
InferX credits remainder to OwnerCreditLedger
Owner uses credits to offset their own GPU bill
```

Deferred: token-based royalty, license models, cash payouts via Stripe Connect.

### GPU Rates

Skills use the existing `gpu_standard` rate. A separate `gpu_skill_dedicated` premium rate is deferred.

---

## Schema

### Core Tables

```sql
-- Managed by InferX admin. Presented as preset options for skill owners to select.
CREATE TABLE SkillTemplate (
    template_id         BIGSERIAL PRIMARY KEY,

    -- Template's own identity: <tenant, namespace, display_name>
    tenant              VARCHAR NOT NULL,   -- InferXTenant
    namespace           VARCHAR NOT NULL,   -- InferXNamespace
    display_name        VARCHAR NOT NULL,
    UNIQUE(tenant, namespace, display_name),

    description         TEXT,

    -- InferX-managed functions backing this template
    func_tenant         VARCHAR NOT NULL,
    func_namespace      VARCHAR NOT NULL,
    normal_funcname     VARCHAR NOT NULL,    -- used when has_cache = false
    producer_funcname   VARCHAR,             -- R3: used to generate KV cache; NULL for no-cache templates
    producer_revision   BIGINT,             -- R3: KV cache is invalid if this changes
    consumer_funcname   VARCHAR,             -- R3: used at inference when has_cache = true
    consumer_revision   BIGINT,             -- R3: must match revision used during cache generation
    -- producer and consumer fields must be set together or not at all:
    CHECK ((producer_funcname IS NULL) = (producer_revision IS NULL)),
    CHECK ((consumer_funcname IS NULL) = (consumer_revision IS NULL)),

    is_active           BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMP DEFAULT NOW()
);

CREATE TABLE Skill (
    skill_id         BIGSERIAL PRIMARY KEY,
    owner_tenant     VARCHAR NOT NULL,
    owner_namespace  VARCHAR NOT NULL,
    skillname        VARCHAR NOT NULL,
    description      TEXT,

    serving_mode     VARCHAR NOT NULL DEFAULT 'dedicated',
    CHECK (serving_mode IN ('shared', 'dedicated')),
    -- API must reject 'shared' until R5; only 'dedicated' is valid in R1–R4

    -- 'free' = no charge; 'per_run' = charge per request
    earning_type     VARCHAR NOT NULL DEFAULT 'free',
    CHECK (earning_type IN ('free', 'per_run')),

    user_price_microcents         INTEGER,
    -- per-run charge; required when earning_type = 'per_run', NULL when 'free'
    CHECK (earning_type = 'free' OR user_price_microcents IS NOT NULL),

    -- who absorbs GPU cost
    gpu_billing_target VARCHAR NOT NULL DEFAULT 'caller',
    CHECK (gpu_billing_target IN ('owner', 'caller')),
    -- 'owner'  = UsageTick.tenant → owner_tenant
    -- 'caller' = UsageTick.tenant → calling_tenant (dedicated only; shared ignores this)
    -- API sets default based on earning_type: 'caller' for free, 'owner' for per_run
    inferx_revenue_share_pct     DECIMAL(5,2) NOT NULL DEFAULT 0.0,
    -- InferX cut of owner's per-run earning; default 0; admin-managed only, not owner-mutable

    active_revision_id BIGINT,  -- FK to SkillRevision, set after first revision

    is_published     BOOLEAN NOT NULL DEFAULT FALSE,
    -- FALSE = draft/testing, only owner_tenant can call
    -- TRUE  = any authenticated user can call
    published_at     TIMESTAMP,   -- audit: when first published
    published_by     VARCHAR,

    UNIQUE(owner_tenant, owner_namespace, skillname)
);

-- R1: always one row per skill (version=1), not exposed to owners.
-- Schema and storage paths kept version-aware for future multi-version support.
CREATE TABLE SkillRevision (
    revision_id    BIGSERIAL PRIMARY KEY,
    skill_id       BIGINT NOT NULL REFERENCES Skill(skill_id),
    version        INTEGER NOT NULL,

    template_id    BIGINT NOT NULL REFERENCES SkillTemplate(template_id),

    has_cache      BOOLEAN NOT NULL DEFAULT FALSE,
    cache_status   VARCHAR NOT NULL DEFAULT 'none',
    CHECK (cache_status IN ('none', 'pending', 'ready', 'failed')),
    cache_ready_at TIMESTAMP,

    created_at     TIMESTAMP DEFAULT NOW(),
    created_by     VARCHAR NOT NULL,

    UNIQUE(skill_id, version)
);

-- Add FK after SkillRevision exists
ALTER TABLE Skill ADD CONSTRAINT fk_active_revision
    FOREIGN KEY (active_revision_id) REFERENCES SkillRevision(revision_id);

```

SkillAccessGrant (allowlist access control) deferred to future release.

### License / Subscription Tables

Deferred to future release (monthly_license, one_time_fee earning models not in R1–R3).

### Owner Credit Ledger

Not written per-request. A monthly background job aggregates per-skill request counts and writes one settlement row per skill per period. Owner's current balance = `SUM(amount_microcents)`.

**Request counting (R2)**: `UsageTick` tracks GPU time, not request count. The gateway maintains an in-memory counter per `skill_id` and flushes to a `SkillRequestCount` table periodically (e.g., every minute). Pricing fields are snapshotted at flush time. When billing fields change via PUT, the gateway forces an immediate flush of the current counter before applying the new values — ensuring no window ever spans two prices.

```sql
-- R2: flushed from gateway in-memory counter periodically.
-- Pricing fields snapshotted at flush time so settlement is auditable after price changes.
CREATE TABLE SkillRequestCount (
    id                  BIGSERIAL PRIMARY KEY,
    skill_id            BIGINT NOT NULL REFERENCES Skill(skill_id),
    request_count       INTEGER NOT NULL,
    fee_microcents      INTEGER NOT NULL,       -- snapshot of user_price_microcents at flush time
    revenue_share_pct   DECIMAL(5,2) NOT NULL,  -- snapshot of inferx_revenue_share_pct (0–100) at flush time
    window_start        TIMESTAMP NOT NULL,
    window_end          TIMESTAMP NOT NULL
);
```

```sql
CREATE TABLE OwnerCreditLedger (
    ledger_id         BIGSERIAL PRIMARY KEY,
    owner_tenant      VARCHAR NOT NULL,
    skill_id          BIGINT REFERENCES Skill(skill_id),
    source            VARCHAR NOT NULL,
    -- 'skill_revenue' | 'adjustment'  ('license_revenue' deferred to future release)
    amount_microcents INTEGER NOT NULL,
    -- positive = credit, negative = deduction; settlement uses FLOOR rounding (avoids over-crediting owner)
    request_count     INTEGER,   -- requests in this period
    period_start      DATE,
    period_end        DATE,
    created_at        TIMESTAMP DEFAULT NOW()
);
```

Future: cash payouts via Stripe Connect.

### Cache Retention Billing

```sql
CREATE TABLE SkillCacheRetentionCharge (
    charge_id    BIGSERIAL PRIMARY KEY,
    skill_id     BIGINT NOT NULL REFERENCES Skill(skill_id),
    owner_tenant VARCHAR NOT NULL,
    period_start DATE NOT NULL,
    amount_cents INTEGER NOT NULL DEFAULT 100,  -- $1 = 100 cents
    charged_at   TIMESTAMP
);
```

### UsageTick Extensions

```sql
ALTER TABLE UsageTick
    ADD COLUMN caller_tenant VARCHAR;          -- who called; set only on paid dedicated skill requests (NULL = free skill or direct funccall)
```

### BillingRate Extensions

```sql
INSERT INTO BillingRate (usage_type, ...) VALUES
    ('skill_cache_retention', ...);  -- R3; 'gpu_skill_dedicated' premium rate deferred
```

---

## Request Flow

```
POST /skills/{owner_tenant}/{ns}/{skillname}/v1/chat/completions
Authorization: Bearer <calling_tenant_apikey>
1. Validate token — any authenticated token, skip AllowTenant check
2. Look up Skill by (owner_tenant, owner_namespace, skillname)
3. Check access:
   - is_published = false → caller must be owner_tenant, else 404 (don't leak existence)
   - is_published = true → any authenticated token allowed
4. Load active SkillRevision via active_revision_id → get template_id, has_cache, cache_status
   - if has_cache = true and cache_status != 'ready' → 503 Service Unavailable
   - R4: X-Skill-Revision: N header (owner only) targets a specific revision instead
5. Load SkillTemplate by template_id → get func_tenant, func_namespace,
   normal_funcname, consumer_funcname
6. Inject prefix into messages (`v1/chat/completions`):
   - Collect skill prefix + all caller-supplied `"role":"system"` messages → join with `\n\n`
   - Reconstruct messages as `[{role:system, content:<combined>}, ...non_system_messages]`
   - For `v1/completions`: prepend prefix directly to the `prompt` string
7. Construct FuncRouteTarget using the canonical logical key (see Routing section), branching on `gpu_billing_target`:
   - dedicated, billing=caller: tenant=calling_tenant, funcname={func_tenant}.{func_namespace}.{funcname}[.{func_rev}]
   - dedicated, billing=owner:  tenant=owner_tenant,   funcname={func_tenant}.{func_namespace}.{funcname}[.{func_rev}].{calling_tenant}
   - shared:                    tenant=owner_tenant,   funcname={func_tenant}.{func_namespace}.{funcname}[.{func_rev}]
   - physical: {func_tenant}/{func_namespace}/{funcname}
8. Dispatch to physical target; LMCache on consumer pod matches prefix by content hash automatically (R3 only); no cache path passed by gateway
9. Record UsageTick:
   - tenant        = logical.tenant (owner_tenant when billing=owner or shared; calling_tenant when billing=caller)
   - caller_tenant = calling_tenant if (dedicated AND billing=owner); NULL otherwise
10. Increment in-memory skill request counter (R2, if earning_type = 'per_run'); gateway flushes to SkillRequestCount table periodically
```

---

## Background Jobs

- **Monthly skill settlement (R2)**: aggregate `SkillRequestCount` per skill → compute `FLOOR(SUM(request_count × fee_microcents × (1 - revenue_share_pct / 100.0)))` → write `OwnerCreditLedger` row
- **Cache retention billing (R3)**: monthly charge to owner for each active skill with `has_cache = true`

---

## Implementation Checklist

### R1 — Free skills, no cache, dedicated instance

**Database**
- [ ] Create `SkillTemplate` table
- [ ] Create `Skill` table
- [ ] Create `SkillRevision` table
- [ ] `ALTER TABLE UsageTick ADD COLUMN caller_tenant VARCHAR`

**File storage**
- [ ] On skill create: write prefix to `/opt/inferx/skills/{owner_tenant}/{owner_namespace}/{skillname}/1/skill.data`
- [ ] On skill delete: remove skill storage directory

**Gateway — request handler**
- [ ] Add `/skills/{owner_tenant}/{ns}/{skillname}/v1/...` route
- [ ] Validate token; skip `AllowTenant` check
- [ ] Enforce owner-only access when `is_published = false`; return 404 to non-owners (don't leak existence)
- [ ] Load `Skill` + `SkillRevision` + `SkillTemplate` on each request
- [ ] Read `skill.data`; inject as system message: collect prefix + caller system messages → join with `\n\n` → single `{role:system}` at front; non-system messages follow unchanged
- [ ] Construct logical key by branching on `skill.gpu_billing_target` (R1 is no-cache, so no `{func_rev}`):
  - `billing=caller` → `skills/{calling_tenant}/{func_tenant}.{func_namespace}.{normal_funcname}`
  - `billing=owner`  → `skills/{owner_tenant}/{func_tenant}.{func_namespace}.{normal_funcname}.{calling_tenant}`
- [ ] Dispatch to physical target `{func_tenant}/{func_namespace}/{normal_funcname}`
- [ ] Emit `UsageTick`; set `caller_tenant = calling_tenant` when `billing=owner`, NULL when `billing=caller`

**Gateway — skill management API**
- [ ] `POST   /skills/{owner_tenant}/{ns}/{skillname}` — create skill + revision; accepted body fields: `template_id`, `prefix`, `serving_mode`, `has_cache`, `gpu_billing_target` (persisted as-is; defaults to `'caller'` if omitted); validate:
  - `serving_mode` must be `'dedicated'` (reject `'shared'` until R5)
  - `has_cache` must be `false` (reject `true` until R3)
  - `earning_type` is not an accepted field in R1; if present in the body, reject 400 with `"earning_type is not supported in R1"`; R2 will add it
- [ ] `GET    /skills/{owner_tenant}/{ns}/{skillname}` — get skill
- [ ] `DELETE /skills/{owner_tenant}/{ns}/{skillname}` — delete skill + revision; order: `UPDATE Skill SET active_revision_id = NULL` → `DELETE SkillRevision` → `DELETE Skill` (circular FK requires nulling first)
- [ ] `POST   /skills/{owner_tenant}/{ns}/{skillname}/publish` — set `is_published = true`, `published_at = NOW()`, `published_by = calling_tenant`
- [ ] `POST   /skills/{owner_tenant}/{ns}/{skillname}/unpublish` — set `is_published = false`
- [ ] `GET    /skilltemplates` — list active templates for owner selection; returns `Vec<SkillTemplate>` (full struct, same as `secret.rs`); UI requires at minimum `template_id`, `display_name`, `description`, `normal_funcname` from each row

**Admin**
- [ ] Seed `SkillTemplate` rows for R1 (normal functions under `inferx/skills`)

---

### R1.1 — Dashboard Skills tab

**`SkillSummary` struct** (new, `secret.rs`) — used by both list endpoints; avoids repeating template columns across every skill row:

```rust
pub struct SkillSummary {
    pub skill_id: i64,
    pub owner_tenant: String,
    pub owner_namespace: String,
    pub skillname: String,
    pub serving_mode: String,
    pub earning_type: String,
    pub user_price_microcents: Option<i32>,
    pub gpu_billing_target: String,
    pub is_published: bool,
    pub published_at: Option<chrono::NaiveDateTime>,
    pub active_revision_id: Option<i64>,
    pub version: i32,
    pub has_cache: bool,
    pub cache_status: String,
    pub template_display_name: String,   // only template field needed for list UI
}
```

SQL: `SELECT s.*, sr.version, sr.has_cache, sr.cache_status, st.display_name AS template_display_name FROM Skill s JOIN SkillRevision sr ON sr.revision_id = s.active_revision_id JOIN SkillTemplate st ON st.template_id = sr.template_id WHERE <filter>`

**Gateway — fix existing R1 `CreateSkill` handler to honor request body fields**
- [ ] Add `gpu_billing_target: Option<String>` field to `SkillCreateRequest` struct in `http_gateway.rs`; deserialize with `#[serde(default)]`
- [ ] In the `CreateSkill` handler, replace the hardcoded `"caller"` argument with `req.gpu_billing_target.as_deref().unwrap_or("caller")`; validate the value is `"caller"` or `"owner"`, else reject 400
- [ ] `earning_type`: add `earning_type: Option<String>` to `SkillCreateRequest`; if `Some(_)` → reject 400 with `"earning_type is not supported in R1"`; handler continues to pass `"free"` to `CreateSkill`

**Gateway — `secret.rs` SQL methods**
- [ ] Add `#[derive(Serialize, Deserialize, Debug, Clone, FromRow)]` to `SkillSummary` struct
- [ ] `SqlSecret::ListSkillsByNamespace(owner_tenant, namespace, show_all: bool) -> Result<Vec<SkillSummary>>` — `show_all=true` omits `is_published` filter; `show_all=false` adds `AND s.is_published = TRUE`
- [ ] `SqlSecret::ListPublishedSkills() -> Result<Vec<SkillSummary>>` — `WHERE s.is_published = TRUE`, no tenant/namespace filter
- [ ] `SqlSecret::ListAllSkillTemplates() -> Result<Vec<SkillTemplate>>` — no `is_active` filter (admin view; distinct from existing `ListActiveSkillTemplates`)
- [ ] `SqlSecret::CreateSkillTemplate(tenant, namespace, display_name, description, func_tenant, func_namespace, normal_funcname, is_active) -> Result<SkillTemplate>` — INSERT; on unique violation of `(tenant, namespace, display_name)` return `Err` that maps to 409
- [ ] `SqlSecret::DeactivateSkillTemplate(template_id) -> Result<()>` — `UPDATE SkillTemplate SET is_active = FALSE WHERE template_id = $1`; 404 if rows_affected == 0
- [ ] `SqlSecret::ActivateSkillTemplate(template_id) -> Result<()>` — `UPDATE SkillTemplate SET is_active = TRUE WHERE template_id = $1`; 404 if rows_affected == 0
- [ ] `SqlSecret::UpdateSkillTemplate(template_id, ...) -> Result<SkillTemplate>` — UPDATE all editable fields; 404 if not found; on unique violation of `(tenant, namespace, display_name)` return `Err` that maps to 409; caller must pre-check `IsSkillTemplateReferenced` before changing func fields
- [ ] `SqlSecret::DeleteSkillTemplate(template_id) -> Result<()>` — DELETE; 404 if not found; caller must pre-check `IsSkillTemplateReferenced` and reject if true
- [ ] `SqlSecret::IsSkillTemplateReferenced(template_id) -> Result<bool>` — returns true if any `SkillRevision` row references this template; used to guard func-field edits and deletion

**Gateway — route registrations (`http_gateway.rs`)**
- [ ] `GET  /skills` → `ListPublishedSkills` handler
- [ ] `GET  /skills/:owner_tenant/:namespace` → `ListSkillsByNamespace` handler
- [ ] `GET    /admin/skilltemplates` → `AdminListSkillTemplates` handler
- [ ] `POST   /admin/skilltemplates` → `AdminCreateSkillTemplate` handler
- [ ] `PUT    /admin/skilltemplates/:template_id` → `AdminUpdateSkillTemplate` handler; validates function exists via `objRepo.GetFunc`; rejects func-field changes if template is referenced
- [ ] `DELETE /admin/skilltemplates/:template_id` → `AdminDeleteSkillTemplate` handler; rejects if template is referenced
- [ ] `POST   /admin/skilltemplates/:template_id/activate` → `AdminActivateSkillTemplate` handler
- [ ] `POST   /admin/skilltemplates/:template_id/deactivate` → `AdminDeactivateSkillTemplate` handler

**Gateway — list endpoints**
- [ ] `GET /skills/{owner_tenant}/{namespace}` — list skills in a namespace; returns `Vec<SkillSummary>`; visibility rule: if `token.IsNamespaceAdmin(owner_tenant, namespace)` → return all skills (draft + published); otherwise → return only `is_published = true` skills
- [ ] `GET /skills` — list all published skills across all tenants/namespaces (valid token required); returns `Vec<SkillSummary>` where `is_published = true`

**Access control summary**

| Viewer | Own namespace skills | Others' skills |
|---|---|---|
| Namespace admin (`IsNamespaceAdmin`) | All (draft + published) | Published only |
| Any other authenticated user | Published only | Published only |

**Dashboard — nav**
- [ ] Add **Skills** nav link to `base.html` between Models and Instances
- [ ] Add `nav_skills_active` condition: `request_path_no_slash in ('/listskills',) or request_path.startswith('/skill_create') or request_path.startswith('/skill/')` — explicitly excludes `/admin/skill*` paths, which remain covered by `nav_admin_active`

**Dashboard — routes (`app.py`)**
- [ ] `GET  /listskills` — two sections: (1) for each namespace the user has admin access to, call `GET /skills/{active_tenant}/{namespace}` and merge results into "My Skills" (collect their `skill_id`s into a set); (2) call `GET /skills` and filter out rows whose `skill_id` is already in the "My Skills" set for "Published Skills" — this correctly excludes only skills already shown, not all same-tenant skills; render `skill_list.html`
- [ ] `GET  /skill_create` — fetch active skill templates from `GET /skilltemplates`; render `skill_create.html`
- [ ] `POST /skill_save` — collect form data including selected namespace; POST to gateway `POST /skills/{owner}/{ns}/{name}` with `{template_id, prefix, serving_mode:"dedicated", has_cache:false, gpu_billing_target}`; do not send `earning_type` (R1 API rejects it); on success redirect to `/listskills`
- [ ] `POST /skill/publish/<owner>/<ns>/<name>` — proxy to gateway publish endpoint; redirect back to `/listskills`
- [ ] `POST /skill/unpublish/<owner>/<ns>/<name>` — proxy to gateway unpublish endpoint; redirect back to `/listskills`
- [ ] `POST /skill/delete/<owner>/<ns>/<name>` — proxy to gateway delete endpoint; redirect back to `/listskills`

**Dashboard — `skill_list.html`**

*My Skills section* (all namespaces the user has admin access to — draft + published)
- [ ] Section header "My Skills" + "Create Skill" button
- [ ] Table columns: Namespace, Name, Model (template `display_name`), Status (Draft / Published), Actions
- [ ] Actions per row: Draft → Publish; Published → Unpublish; always → Delete (JS confirm)
- [ ] Empty state when no own skills: "You have no skills yet. Create your first skill."

*Published Skills section* (all published skills globally, excluding any already shown in "My Skills" — filtered client-side by `skill_id` set, not by `owner_tenant`; same-tenant published skills in namespaces the user does not administer are shown here)
- [ ] Section header "Published Skills"
- [ ] Table columns: Owner, Namespace, Name, Model — no action buttons (call-only view)
- [ ] Hidden entirely when result set is empty after dedup

**Dashboard — `skill_create.html`**

Form sections (panel style matching `func_create.html`):

*Identity*
- [ ] Skill name (text input; validate `^[a-z0-9][a-z0-9-]*$` client-side)
- [ ] Namespace (dropdown of namespaces the user has admin access to; default = "default")

*Template*
- [ ] Dropdown populated from `GET /skilltemplates`; option value is `template_id`; option label is `"{display_name} ({tenant}/{namespace})"` always — `tenant/namespace` together form the full template identity prefix, matching the UNIQUE constraint `(tenant, namespace, display_name)`; ensures options are visually distinct for any valid combination of active templates; selecting one reveals `description` and model name (`normal_funcname`) as read-only detail below the dropdown

*System Prompt*
- [ ] Textarea for prefix content — label: "System Prompt — prepended to every user request as the leading system message"

*Billing*
- [ ] In R1: only Free earning is exposed; no radio button needed; `earning_type` is not included in the submit payload (R1 API rejects it)
- [ ] GPU billed to: radio — **Caller** (default) / **Owner**

*Prefix Cache* (shown, disabled in R1)
- [ ] Checkbox "Enable prefix cache" — always disabled in R1 with label "Coming in R3"
- [ ] `has_cache` hardcoded to `false` on submit

- [ ] Submit: "Create Skill"; on success flash "Skill created — publish it when ready" and redirect to `/listskills`; on error re-render form with inline error message

---

### R1.1 (cont.) — Admin: Skill Template Management

InferX admins manage `SkillTemplate` rows through a dedicated page under the Admin tab. Regular users never see this page; `GET /skilltemplates` (owner-facing) already returns only `is_active = true` rows and needs no change.

**Important constraints**
- Editing `func_tenant`, `func_namespace`, or `normal_funcname` is rejected if any `SkillRevision` references the template (`IsSkillTemplateReferenced`). These fields route live traffic and cannot be retargeted while skills exist.
- Deletion is rejected by the same check. The underlying FK (`SkillRevision.template_id → SkillTemplate`) is plain (no CASCADE), so the pre-check surfaces a clean 409 instead of a DB error.
- Non-func fields (`display_name`, `description`, `tenant`, `namespace`, `is_active`) can always be edited. Changes take effect immediately for all skills backed by the template (routing reads these via JOIN at request time).

**Gateway — admin template endpoints**
- `GET    /admin/skilltemplates` — list all templates including inactive; returns `Vec<SkillTemplate>`
- `POST   /admin/skilltemplates` — create; required: `tenant`, `namespace`, `display_name`, `func_tenant`, `func_namespace`, `normal_funcname`; optional: `description`, `is_active` (default `true`); 409 on duplicate `(tenant, namespace, display_name)`
- `PUT    /admin/skilltemplates/:template_id` — edit; same fields as create; validates `GetFunc` exists; rejects func-field changes if template is referenced; 404 if template not found (checked before `GetFunc`)
- `DELETE /admin/skilltemplates/:template_id` — delete; 409 if referenced; 404 if not found
- `POST   /admin/skilltemplates/:template_id/activate` — set `is_active = true`; 404 if not found
- `POST   /admin/skilltemplates/:template_id/deactivate` — set `is_active = false`; hides template from new skill creation; existing skills unaffected; 404 if not found

**Dashboard — routes (`app.py`)**
- `GET  /admin/skilltemplates` — render `admin_skill_templates.html`
- `GET  /admin/skilltemplate_create` — render `admin_skill_template_create.html`
- `POST /admin/skilltemplate_save` — proxy to gateway create; redirect to list on success
- `GET  /admin/skilltemplate/<id>/edit` — fetch current template; render `admin_skill_template_edit.html`
- `POST /admin/skilltemplate/<id>/update` — proxy to gateway PUT; redirect to list on success
- `POST /admin/skilltemplate/<id>/activate` — proxy to gateway activate; redirect to list
- `POST /admin/skilltemplate/<id>/deactivate` — proxy to gateway deactivate; redirect to list
- `POST /admin/skilltemplate/<id>/delete` — proxy to gateway delete; 409 surfaces as user-facing error

**Dashboard — `admin_skill_templates.html`**
- Table columns: ID, Display Name, Description, Backing Model, Cache Support, Active, Actions
- Actions per row: Edit (always); Activate or Deactivate (toggled by `is_active`); Delete (JS confirm warns template cannot be deleted if in use)

**Dashboard — `admin_skill_template_create.html` / `admin_skill_template_edit.html`**

Form sections:

*Identity*
- Tenant (default `inferx`), Namespace (default `skills`), Display name, Description

*Backing Function*
- `func_tenant`, `func_namespace`, `normal_funcname` — edit form shows these but gateway rejects changes if template is referenced

*Cache Functions (R3 — shown, disabled)*
- `producer_funcname` / `producer_revision` — disabled inputs, label "Cache producer — R3"
- [ ] `consumer_funcname` / `consumer_revision` — disabled inputs, label "Cache consumer — R3"
- [ ] All four fields hardcoded to empty/null on submit in R1

- [ ] Submit: "Create Template"; on success redirect to `/admin/skilltemplates`; on error re-render with inline error message (409 → "Template name is already taken")

---

### R2 — Non-free skills, no cache, dedicated instance

**Database**
- [ ] Create `SkillRequestCount` table
- [ ] Create `OwnerCreditLedger` table

**Gateway — skill request handler**
- [ ] Branch logical key on `gpu_billing_target` (same logic as R1, now also covers `earning_type='per_run'` combinations):
  - `billing=caller` → `logical.tenant = calling_tenant`, no funcname suffix
  - `billing=owner`  → `logical.tenant = owner_tenant`, append `.{calling_tenant}` to logical funcname
- [ ] Default `gpu_billing_target` when not set by owner: `'caller'` for free, `'owner'` for per_run
- [ ] Set `caller_tenant = calling_tenant` on `UsageTick` when `dedicated AND billing=owner`; NULL otherwise
- [ ] Charge caller per-run fee: deduct `user_price_microcents` from caller's credit balance (same path as quota check); reject with 402 if insufficient balance
- [ ] Increment in-memory counter per `skill_id`; flush to `SkillRequestCount` periodically (~1 min) with snapshotted `fee_microcents` and `revenue_share_pct`

**Skill management API**
- [ ] `PUT /skills/{owner_tenant}/{ns}/{skillname}` — owner-mutable billing fields: `earning_type`, `user_price_microcents`, `gpu_billing_target`; force-flush in-memory request counter before applying changes; system prompt / template change requires delete + recreate (`inferx_revenue_share_pct` is admin-only, not in this API)

**Background jobs**
- [ ] Monthly settlement: aggregate `SkillRequestCount` → compute `FLOOR(SUM(request_count × fee_microcents × (1 - revenue_share_pct / 100.0)))` → write `OwnerCreditLedger`

---

### R3 — Skills with cache, dedicated instance

**Database**
- [ ] Create `SkillCacheRetentionCharge` table
- [ ] Add `BillingRate` row for `skill_cache_retention`

**Scheduler / NodeAgent**
- [ ] Support `single_use` flag on `LeaseWorker` → `ReturnWorker` kills pod instead of returning to idle
- [ ] Relay `skill_cache_metadata` through `LeaseWorker → ResumePod → NodeAgent`
- [ ] NodeAgent loads KV cache into LMCache on consumer pod via path pattern

**Gateway — create validation (R3 additions)**
- [ ] Lift `has_cache = true` restriction in create API; enforce template has non-null `producer_funcname` / `consumer_funcname`, else 400

**Gateway — publish / request gates**
- [ ] Reject publish (`is_published = true`) with 409 if `has_cache = true` and `cache_status != 'ready'`
- [ ] Reject inference request with 503 if `has_cache = true` and `cache_status != 'ready'`

**Gateway — cache producer workflow**
- [ ] On skill create with `has_cache = true`: spawn `SkillCacheWorker` background task (return 202 immediately)
- [ ] `SkillCacheWorker`: check dedup index at `/opt/inferx/kvcache/hash/{template_id}/{hash}`; skip producer if symlink exists
- [ ] `SkillCacheWorker`: lease producer pod via `LeaseWorker` with `single_use = true` + `skill_cache_metadata`
- [ ] `SkillCacheWorker`: update `cache_status` → `pending` → `ready` / `failed`
- [ ] On producer complete: create dedup symlink at `/opt/inferx/kvcache/hash/{template_id}/{hash}`

**Gateway — cached request routing**
- [ ] Use `consumer_funcname` when `has_cache = true`
- [ ] Logical key follows canonical routing rules: free dedicated → `skills/{calling_tenant}/{func_tenant}.{func_namespace}.{consumer_funcname}.{consumer_revision}`; paid dedicated → `skills/{owner_tenant}/{func_tenant}.{func_namespace}.{consumer_funcname}.{consumer_revision}.{calling_tenant}`; shared → `skills/{owner_tenant}/{func_tenant}.{func_namespace}.{consumer_funcname}.{consumer_revision}`

**File storage**
- [ ] On skill delete: remove `/opt/inferx/kvcache/tenants/{owner_tenant}/{owner_namespace}/{skillname}/{version}/`
- [ ] Remove dedup symlink if no other skill references the same hash

**Background jobs**
- [ ] Monthly cache retention: charge owner per active cached skill → write `SkillCacheRetentionCharge`

---

### R4 — Skill versioning

**Gateway — skill management API**
- [ ] `POST /skills/{owner_tenant}/{ns}/{skillname}/revisions` — create new revision (increments version); validate template `has_cache` consistency (same rules as R3 create)
- [ ] `GET  /skills/{owner_tenant}/{ns}/{skillname}/revisions` — list all revisions
- [ ] `PUT  /skills/{owner_tenant}/{ns}/{skillname}/active_revision` — promote or roll back (`active_revision_id = revision_id`); reject if target revision has `has_cache = true` and `cache_status != 'ready'` (409)
- [ ] `X-Skill-Revision: N` header support in request handler (owner-only; resolve by `WHERE skill_id = ? AND version = N`)

**Optional (add if needed)**
- [ ] Add `status VARCHAR` field to `SkillRevision` (draft/deprecated/yanked); if added — `PUT /active_revision` must reject yanked target (409), and `X-Skill-Revision: N` must reject yanked revision (404)

---

### R5 — Shared instance

Three gateway changes — no schema migration, no new tables, no scheduler changes. The funcname-encoding approach from R2 already sets `logical.tenant = owner_tenant` for paid skills, which is correct for shared as well.

**Gateway**
- [ ] Remove `serving_mode = 'shared'` rejection in create/update API; reject `gpu_billing_target = 'caller'` when `serving_mode = 'shared'` (shared always bills owner for GPU)
- [ ] Branch logical key on `serving_mode`: shared → `skills/{owner_tenant}/...` with no `{calling_tenant}` suffix (all callers share one pool); dedicated → existing per-`gpu_billing_target` key from R2
- [ ] Null `caller_tenant` on `UsageTick` for shared requests (one tick spans multiple callers; attribution is inaccurate)

---

## UsageTick Billing Attribution: Design Options and Decision

### Current Situation (R1)

All R1 skills are `free + dedicated`. The routing branches on `gpu_billing_target`:
- `billing=caller` (default for free): `logical.tenant = calling_tenant`, `UsageTick.tenant = calling_tenant`, `caller_tenant = NULL`
- `billing=owner` (owner subsidizes GPU): `logical.tenant = owner_tenant`, `UsageTick.tenant = owner_tenant`, `caller_tenant = calling_tenant`

No billing bug exists in R1 — the routing correctly reflects `gpu_billing_target` from the first release.

### The Problem (R2+)

For paid dedicated skills the owner charges a per-run fee to callers and should absorb GPU cost from that revenue. But if `logical.tenant = calling_tenant` the GPU tick lands under the caller's account, not the owner's — the caller is double-charged (GPU + per-run fee) and the owner never pays GPU.

For shared mode (R5) the owner clearly pays GPU. All callers share one instance, so attributing GPU to any single `caller_tenant` is inaccurate.

### Options Considered

**Option A — Separate `billing_tenant` field**

Add `billing_tenant: String` to `FuncWorkerInner`. Keep `logical.tenant = calling_tenant` as the worker pool key. Use `billing_tenant = skill.owner_tenant` for `UsageTick.tenant` when the owner pays GPU.

- Worker pool isolation: maintained per caller via `logical.tenant = calling_tenant`
- `UsageTick.tenant`: driven by `billing_tenant`, not `tenant`
- Drawback: `tenant` in `UsageTick` is already the billing field — that is its current meaning everywhere. A parallel `billing_tenant` column duplicates the same concept with a different name. Confusing to query; existing billing SQL already assumes `tenant` = who pays.

Migration: all existing rows are free+dedicated so `billing_tenant = tenant` for every row. Backfill is trivial (`UPDATE UsageTick SET billing_tenant = tenant`). Not a migration risk, but the redundant column remains a schema smell.

**Option B — Funcname encoding (preferred)**

Encode `calling_tenant` into the logical funcname for paid dedicated skills:

```
-- free + dedicated (unchanged):
logical: skills/{calling_tenant}/{func_tenant}.{func_namespace}.{funcname}

-- paid + dedicated:
logical: skills/{owner_tenant}/{func_tenant}.{func_namespace}.{funcname}.{calling_tenant}
```

`logical.tenant = owner_tenant` so `UsageTick.tenant = owner_tenant` — owner pays GPU. Per-caller pool isolation is maintained via the distinct funcname suffix, not via the tenant segment.

- No new fields; `tenant` retains its single meaning (billing target) everywhere
- Worker pools are still isolated per caller (via funcname, not tenant)
- Migration: existing R1 rows are all free+dedicated; their logical key format is unchanged. No backfill needed.

### Decision: Funcname Encoding

`tenant` in `UsageTick` already means "who pays GPU" — that is its purpose in the existing billing SQL and `TenantQuota` logic. Option A introduces `billing_tenant` as a second column with the same meaning, making it unclear which field to use. Option B keeps one field, one meaning, and requires no schema change.

### `caller_tenant` Decision

| Case | `caller_tenant` value | Useful? |
|---|---|---|
| Dedicated, billing=caller | NULL | Redundant — `tenant` already is the caller |
| Dedicated, billing=owner | `calling_tenant` | Necessary — `tenant` is the owner; only way to recover caller identity |
| Shared | NULL | Inaccurate — one tick spans multiple concurrent callers |

The rule is: `caller_tenant = calling_tenant` when `dedicated AND billing=owner`; NULL otherwise. `earning_type` is irrelevant — a free skill with `billing=owner` (owner subsidizes GPU) also needs `caller_tenant` populated.

`caller_tenant` is technically derivable by parsing the funcname suffix, but keeping the explicit column avoids string parsing on queries.

**Decision**: keep `caller_tenant`; populate for any dedicated `billing=owner` skill regardless of `earning_type`; null for `billing=caller` (redundant) and shared (inaccurate).

---

## Open Questions / Deferred

- KV cache file naming and exact path pattern — subject to verification against LMCache/NodeAgent runtime behavior
- Cash payout infrastructure (future — Stripe Connect)
- Cache retention size-based tiers (currently flat $1/skill/month)
- **Function version lifecycle for producer/consumer functions** (deferred): currently InferX removes old function versions on update. Producer/consumer functions pinned by `SkillTemplate` must not be removed while referenced. Future work: add reference check before removing a function version — block deletion if any `SkillTemplate.producer_revision` or `consumer_revision` points to it. Admin workflow: create new template → deprecate old template (`is_active=false`) → wait for skill owners to migrate → delete old template → delete old function version once unreferenced.
