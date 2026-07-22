# Token-Based Billing for Shared Instances — Requirements & Design

> Status: design spec for review. No code written yet.
> This supersedes the earlier exploratory notes; the design below reflects the
> final decisions reached during design discussion, all grounded against the
> current code (file/line refs inline).

---

## 1. Requirement

InferX currently bills by **GPU time** (dedicated, tenant-owned instances,
metered as `UsageTick` — start/periodic/final ticks × `interval_ms`). We want to
add **token-based billing** for **shared instances**: one platform-owned model
deployment serving many tenants, billed per token.

Concretely:

- Keep GPU-time billing for dedicated instances **unchanged**.
- Add a **direct, OpenAI-compatible token-billed API** for shared models, so
  tenants can point an OpenAI SDK at InferX and be billed per token.
- Reuse the **existing shared-serving** machinery (the `inferx/endpoint`
  platform namespace already used by the OpenRouter path) so OpenRouter traffic
  and direct traffic **share the same physical instance**.
- Meter tokens per **caller tenant**, persist per-request usage, and rate it
  against a **per-token price book** (separate input / output / cached rates).

### Why shared (not dedicated) for token billing

Token billing on a dedicated instance makes the provider absorb the tenant's
idle risk for free — a losing trade. Token billing only pays off once the GPU is
**shared**, because that is where the fixed GPU cost is amortized across tenants
(higher aggregate occupancy). Two-tier offering:

| Tier | Serving | Billing | For |
|---|---|---|---|
| Dedicated + scale-to-zero | tenant-owned instance | **GPU-time** (`UsageTick`) | bursty / private / isolated tenants |
| **Shared-resident (hot)** | platform `inferx/endpoint` | **token** (new) | the public "hit our hosted model" endpoint |

(Detailed cost reasoning — GPU-time vs token, cold-start effects — is in
Appendix A.)

---

## 2. What already exists (grounded)

Much of the pipeline is already present; this shrinks the work substantially.

- **Shared serving EXISTS.** `/v1/chat/completions` → `OpenRouterChatCompletions`
  (`http_gateway.rs:4249`) takes the model from the request body, and rewrites
  to the platform-owned shared func `/funccall/inferx/endpoint/{model}/…`
  (`PLATFORM_TENANT="inferx"`, `PLATFORM_SHARED_NAMESPACE="endpoint"`,
  `http_gw.rs:47-48`), dispatched via `FuncCall1` with the caller's `AccessToken`
  preserved. One platform-owned model, many caller tenants. Admission already
  discriminates these: `IsPlatformSharedFunc` (`admission.rs:472`).
- **`usage` extraction EXISTS.** `req_token.rs` (`FuncCallWithTokenTracking`,
  wired on `/modelcall/*rest`) parses the OpenAI `usage` object for both
  non-streaming (`:221`) and streaming (`:157`, final chunk), and auto-injects
  `stream_options.include_usage=true` when missing (`:80-95`). It currently only
  `error!`-logs the counts — no persistence, no tenant, no `cached_tokens`.
- **Caller attribution EXISTS on the request path.** The caller `AccessToken`
  (with `restrictTenant`/`defaultTenant`) is in scope wherever the request is
  handled; the shared func already computes a `consumer_tenant` for the
  `endpoint` namespace (`func_worker.rs:354`) and `UsageTick` distinguishes
  `tenant` (owner) vs `caller_tenant`.
- **Billing tables + credit/quota machinery EXIST.** `UsageTick`, `BillingRate`,
  `TenantCreditHistory`, `TenantQuota`, `TenantBillingAdminSummary` (`audit.rs`),
  plus an async audit-agent pattern (`UsageTickAuditAgent`, `audit.rs:420-492`).

### The genuinely-new piece

**Rating is intrinsically GPU-time based.** `BillingRate.AddBillingRate`
(`audit.rs:889`) hardcodes `rate_cents_per_hour: i32`; charge = `rate ×
elapsed_time`. Tokens are priced **per million, split input/output/cached** — a
different rating dimension that the current schema cannot express. This needs a
**parallel token-rate schema**, not a new `usage_type` row.

---

## 3. Design

### 3.1 Two public URLs, one shared core

Path determines tier determines billing. Both URLs resolve to the **same
physical** shared func so OpenRouter and direct traffic multiplex on one hot
instance.

| Public URL | Tier | Gate | Rating |
|---|---|---|---|
| `POST /v1/chat/completions` (existing, OpenRouter) | shared | *(no serving gate — `or_listed` dropped; see 3.3)* | **internal per-token rate** (`TokenRate`), same as direct — OR is a comped-credit tenant (3.4) |
| `POST /endpoints/v1/chat/completions` (**new, direct**) | shared | `inferx/endpoint/{model}.published` | **internal per-token rate** |
| `POST /funccall/{tenant}/{ns}/{model}/…` (existing) | dedicated | existing | GPU-time (`UsageTick`) |

- **Model in the body, not the path.** `base_url = …/endpoints/v1` is stable
  across all shared models; the SDK sends `model` in the body and can switch
  freely. SDK-drop-in requires only that the route ends in `/chat/completions`
  and the body/response follow the OpenAI shape — not the literal `/v1` prefix.
  Model-from-body resolution already exists (`http_gateway.rs:4265`).

### 3.2 Shared dispatch core

Both entry points are thin wrappers over one function; they share the **same
physical rewrite target** and differ only in **who is allowed to call it**, what
gate runs before dispatch, and the **rating source**:

```
shared_endpoint_dispatch(req, target_prefix, gateway_request_id, caller_tenant, source):
    model = req.body.model
    request_ts = now()                            # REQUEST time, captured on the gateway BEFORE dispatch
    rewrite uri → {target_prefix}/{model}/v1/chat/completions   # v1: chat only (#1); to add /completions later, preserve incoming remainPath instead of hardcoding
    resp = FuncCall1(token, gw, req)              # both paths converge here
    extract_and_emit_usage(resp, gateway_request_id, client_request_id, caller_tenant, model, source, request_ts)
    return resp                                   # emitted event.ts = request_ts (NOT DB insert time)

OpenRouterChatCompletions(req):        # /v1/chat/completions
    # target = PHYSICAL inferx/endpoint. DROP the or_listed serving gate (no
    # per-request DB). OR is trusted; usage is metered regardless; an unpriced
    # func just isn't billed (row left unprocessed + alert, self-heals) — an
    # exception path that should not fire in normal flow (3.3/3.5).
    gateway_request_id = mint_gateway_request_id()
    shared_endpoint_dispatch(req, target = "/funccall/inferx/endpoint",
                                  gateway_request_id = gateway_request_id,
                                  caller_tenant = resolve_caller_tenant(token),  # OR key resolves to a tenant, 3.2b
                                  source = OpenRouter)

SharedEndpointCompletions(req):        # NEW  /endpoints/v1/chat/completions
    caller_tenant = resolve_caller_tenant(token)  # 3.2b
    if !inferx/endpoint/{model}.published: return 404
    gateway_request_id = mint_gateway_request_id()
    # target = PHYSICAL inferx/endpoint; direct callers are admitted by the
    # shared-endpoint auth rule in 3.2a, not by ordinary inferx/endpoint RBAC.
    shared_endpoint_dispatch(req, target = "/funccall/inferx/endpoint",
                                  gateway_request_id = gateway_request_id,
                                  caller_tenant = caller_tenant,
                                  source = Direct)
```

Both paths rewrite to the **same physical target** `inferx/endpoint/{model}` so
they converge on the **same agent / queue / lease key**. This is the key design
decision that makes "shared instance" true: unlike the virtual
`{caller_tenant}/endpoints/{model}` path, it does **not** partition pooling by
logical tenant (`func_agent_mgr.rs:129,296,534,565`).

- **Direct** rewrites to the physical shared func too, but its access rule is
  **special**: once token-billed shared serving exists, `inferx/endpoint`
  becomes a public shared-serving namespace for **authenticated tenant users
  with inference scope**. Direct calls do **not** require tenant keys to hold
  ordinary `inferx/endpoint` RBAC. Instead, `SharedEndpointCompletions`
  resolves `caller_tenant` from the token (3.2b), gates on
  `funcstatusMgr.Get(inferx, endpoint, model).published`, and then dispatches to
  the physical shared func. Billing/quota attribution remains `caller_tenant`
  even though pooling is shared.
- **OpenRouter** also rewrites to the **physical** `inferx/endpoint/{model}`, and
  **drops the per-request `or_listed` serving gate** (removes the
  `GetEndpointForListing` SQL lookup, `http_gateway.rs:4274`). Decided because
  **reachability and accounting are independent**: even if OR reaches a
  deployed-but-unlisted func, the request still flows through
  `extract_and_emit_usage`, so **its usage is always metered and attributed to
  `caller_tenant`** — the gate only controls *whether it serves*, not *whether it
  is counted*. OR is a trusted partner scoped by RBAC to `inferx/endpoint`
  (`FuncCall1` `IsNamespaceInferenceUser`, `:2090`; key scoped, `:4287`) and only
  calls models from its `/v1/models` catalog (which **is** `or_listed`). `or_listed`
  is dropped **only as a serving gate**; it still defines the `/v1/models` catalog
  (`ListListedEndpoints … WHERE or_listed = true`, `secret.rs:1316`).

  **Pricing = detect, not admission-prevent.** Consequence of dropping the gate:
  an unpriced func *could* serve OR before it's priced. Not a silent hole — the
  usage event is still recorded (metered), and at rollup a no-rate event is
  **skipped (not billed), `processed_at` set, and alerted** (3.5). Since OR only
  calls catalog (`or_listed`, list-time priced) models, this is essentially
  theoretical. Accepted in exchange for zero per-request DB on the OR path.

### 3.2a Physical pooling is resolved; direct-path auth has 2 implementation options

The earlier virtual-route design was dropped because it broke cross-tenant
pooling: virtual endpoint routes are keyed by **logical** identity, so
`tenant-a/endpoints/foo` and `tenant-b/endpoints/foo` land in different
agent/lease pools (`func_agent_mgr.rs:129,296,534,565`) even though they map to
the same physical func.

The resolved pooling design is:

1. **Both OpenRouter and direct shared traffic dispatch to the physical path**
   `/funccall/inferx/endpoint/{model}/...`, so they converge on the same
   agent/queue/lease pool. This fixes the pooling requirement. ✓
2. **No agent-manager key rewrite is needed for sharing.** Both sources use the
   same physical path → same pool under the current physical keying. ✓
3. **Direct shared traffic needs explicit auth treatment — REQUIRES A CODE
   CHANGE (not free).** `FuncCall1` authorizes against the *parsed path's*
   tenant/namespace: `IsNamespaceInferenceUser(tenant, namespace)`
   (`http_gateway.rs:2090`). On the physical rewrite that is `inferx`/`endpoint`,
   so as written **every direct caller would need `inferx/endpoint` inference
   permission**. "Authenticated tenant users may call without ordinary
   `inferx/endpoint` RBAC" is therefore **not** achievable by simply calling
   `FuncCall1` unchanged on the physical path.

There are 2 viable implementation options:

1. **Special-case `inferx/endpoint` inside `FuncCall1`.**
   For the shared physical namespace, replace the normal
   `IsNamespaceInferenceUser(inferx, endpoint)` requirement with a narrow direct
   shared-endpoint admission rule:
   authenticated caller + inference scope + resolved `caller_tenant` +
   `published=true`.

   Pros: small, localized change; both routes still use the same entrypoint.
   Cons: broadens generic `/funccall/*` semantics, so it needs tighter security
   review. Raw `/funccall/inferx/endpoint/*` calls would now depend on this
   special case being perfectly scoped.

2. **Do auth in `/endpoints/v1/*`, then call `FuncCall1` with an internal auth
   bypass parameter.**
   `SharedEndpointCompletions` resolves `caller_tenant`, checks inference scope,
   checks `published=true`, then dispatches physically to
   `inferx/endpoint/{model}` while telling `FuncCall1` not to repeat the normal
   namespace RBAC check for this trusted internal call. This bypass must be an
   internal function parameter / trusted code path, **not** a client-controlled
   header or request field.

   Pros: keeps the exception attached to the product surface that needs it
   (`/endpoints/v1/*`) instead of changing generic funccall semantics. Easier to
   reason about, test, and security-review. Raw `/funccall/inferx/endpoint/*`
   can stay under normal rules unless reached through the trusted path.
   Cons: requires slightly more factoring in `FuncCall1` / dispatch flow.

**DECIDED: option 2.** It keeps the trust boundary narrower — the special rule
belongs to the **direct shared API surface**, not to generic `FuncCall1`
semantics — so raw `/funccall/*` behavior is untouched and the exception is
reachable only through the vetted `/endpoints/v1/*` wrapper. (Option 1 recorded
as the fallback if a single entrypoint ever matters more than minimizing policy
spread.) Still requires security review of the bypass parameter.

Rejected approach: broadly granting tenant inference users ordinary
`inferx/endpoint` RBAC. That would make the direct wrapper's `published` gate
non-load-bearing.

Also note `is_blocked_public_endpoint_inference` (`http_gateway.rs:2072`)
already blocks inference for the *virtual* `public/endpoints` path — evidence
the codebase deliberately gates public-endpoint inference. Either option above
must preserve that intent rather than silently reopening it.

Implementation note: this promotes `inferx/endpoint` from a
"provider-key-only namespace" to a **public shared-serving surface** for tenant
inference users — an intentional, security-reviewable change, not a config tweak.

### 3.2b Tenant context for `/endpoints/v1/*` (codex Medium)

The direct route has **no tenant in the URL**, so `caller_tenant` must be
resolved from the token — and this can fail. Reuse the existing rule
(`resolve_skill_calling_tenant`, `http_gateway.rs:362-377`):

1. if `token.restrictTenant` set → use it;
2. else if `token.defaultTenant` set, exists, and `IsTenantUser` → use it;
3. else **reject** ("no tenant context: use a tenant-restricted API key or set a
   default tenant").

So `/endpoints/v1/*` **requires** a tenant-restricted API key or a resolvable
`defaultTenant`; ambiguous user tokens with neither are rejected (do **not** fall
back to the empty-string helper at `session.rs:485`). This resolved tenant is
the direct route's auth/billing subject: it drives tenant-user admission,
quota/credit attribution, and the emitted `caller_tenant` on the token-usage
event.

**Metering is inline in the shared core, NOT middleware.** Because both entry
points already funnel through one `shared_endpoint_dispatch`, that function is
the single choke point on both request and response — the `usage` extraction goes
there directly. A tower `Layer` would only be worth it if we later want the same
meter on routes that do *not* share this core (e.g. `/modelcall`); until then,
inline is less code. Reuse `req_token.rs`'s extraction + streaming-tee
(`:117-209`) as a shared helper `extract_and_emit_usage`.

### 3.2c `gateway_request_id` source (codex Medium)

`gateway_request_id` is the idempotency key (`TokenUsageEvent` PK + `ON CONFLICT DO
NOTHING`). **Verified: no reusable per-request id exists on the path** —
`FuncCall1`/`OpenRouterChatCompletions` establish none; the only candidates are
unsuitable:
- `X-Request-Id` (`skill_chain.rs:1303`) is **client-supplied** and only
  forwarded — optional, spoofable, may be duplicated. Never the PK.
- `func_worker.rs` `session_id` (`:621`, a `Uuid`) is the **GPU-billing session**
  id and is **per-worker-session, not per-request** — it spans many requests, so
  it cannot key a per-request row.

So the gateway mints its own. Define it explicitly:

**Deterministic rule (no ambiguity):**
- **`gateway_request_id` (the PK) is ALWAYS a gateway-minted UUID**, generated at the
  start of `shared_endpoint_dispatch` *before* dispatch, and threaded through to
  `extract_and_emit_usage`. It is never conditional and never client-derived.
  This is the sole idempotency key (guards double-*insert* of one request's
  usage; the insert is safely retryable), **not** client-level dedup.
- **Client `X-Request-Id` → `client_request_id` column (correlation only, NOT
  the PK).** When the caller sends `X-Request-Id`, store it in a nullable
  `client_request_id` column for support/traceability — it is the one id the
  customer knows and can quote in a dispute. It is **never** the dedup key:
  client-supplied ids are spoofable/duplicable, and using one as the PK would let
  a client reuse a value to make `ON CONFLICT DO NOTHING` silently drop (and thus
  not bill) a later request. So `gateway_request_id` (gateway UUID) keys billing;
  `client_request_id` is a lookup attribute only.
- **No upstream/`chatcmpl` id is stored for v1.** vLLM does return an OpenAI
  `id` (`chatcmpl-…`), but InferX does not parse it today (`req_token.rs` reads
  only `usage`) and it is not needed for billing — skip it; trivial add-later
  migration if response correlation is ever wanted.

### 3.3 Gating summary

Each path's gate follows from its caller type (3.2):

| Path | Target | Gate | Trust boundary |
|---|---|---|---|
| Direct `/endpoints/v1` | **physical** `inferx/endpoint` | resolved `caller_tenant` + inference scope + `published` (via `funcstatusMgr.Get`, informer, no DB) | authenticated tenant inference user on the shared-serving surface |
| OpenRouter `/v1` | **physical** `inferx/endpoint` | **none** (`or_listed` serving gate dropped) | RBAC scope to `inferx/endpoint` (`:2090`, `:4287`) |

- Direct: gate is explicit in the wrapper; `published` defaults `false` for
  shared funcs (`admission.rs:468`) — safe default.
- OpenRouter: **no per-request serving gate, no per-request DB** — relies on RBAC
  scope. Usage is still metered+attributed regardless of reachability;
  unpriced-func serving is caught post-facto at rollup (no-rate → skip + alert,
  3.5), not prevented at admission. `or_listed` still defines the `/v1/models`
  catalog (`secret.rs:1316`), just not per-request serving.

### 3.4 Shared metering, unified rating (no fork)

**Both sources resolve a `caller_tenant` and are token-counted on the InferX
side.** The metering path is fully shared, and — per the decision — *both* OR and
direct traffic attribute to a real InferX tenant (the tenant the request's
`AccessToken` resolves to, §3.2b; the OR provider key resolves to a tenant just
like a direct tenant key). `caller_tenant` is therefore **always set** on both
paths (`resolve_caller_tenant(token)` in both wrappers).

```
extract_and_emit_usage → token event {
    gateway_request_id, caller_tenant, model, source,
    ts = request_ts,                              -- gateway request time; picks the rate (3.5)
    prompt_tokens, completion_tokens, cached_tokens
}
```

**Both sources are handled identically — no billing fork.** Every event (direct
and OR) is rolled up per (caller_tenant, model, hour), rated against `TokenRate`,
and **debits `TenantQuota`** the same way. OpenRouter is modeled as an ordinary
tenant whose credit is topped up with comped/"fake" credit (via the existing
`TenantCreditHistory` top-up path); the OR marketplace settlement is reconciled
out-of-band against that credit, not via a separate code path. This removes the
"double-charge" concern and keeps the metering/rating/debit code **branch-free**.

```
extract_and_emit_usage → token event {
    gateway_request_id, caller_tenant, model, source,   -- source = audit/analytics only
    ts = request_ts,                                    -- gateway request time; picks the rate (3.5)
    prompt_tokens, completion_tokens, cached_tokens
}
  → rollup per (caller_tenant, model, hour) → rate (TokenRate) → debit TenantQuota
```

`source` is still recorded on the event (so reports can separate OR vs direct),
but it **does not change the billing logic**. It is passed as a dispatch argument
(the route knows which it is) — not via a header. (Note: the dormant
`X-Inferx-Model` / `X-Inferx-Model-Call` headers, set but never read, are left in
place for now — they are not the source mechanism.)

### 3.5 Schema — all NEW tables

Tokens are **discrete per-request events**, not a level to sample, and are
per-token priced — neither fits the existing GPU-time tables (`UsageTick`,
`BillingRate`, `UsageHourlyByFunc` are all interval/per-hour). So token billing
adds **new** tables; the existing ones are untouched. Only the final *charge in
cents* flows into the shared, unit-agnostic credit tables (`TenantQuota`,
`TenantCreditHistory`). DDL below follows `dashboard/sql/billing.sql`
conventions (SERIAL PK, two-state `processed_at` marker like `UsageTick`, `effective_from/to`,
per-tenant override, `idx_*` naming).

**Rate key = endpoint slug, not abstract model.** On the shared path the
caller's `model` field == the endpoint slug == the deployed func
(`inferx/endpoint/{slug}`, `http_gateway.rs:4265-4291`). The *same* model may be
deployed multiple times with different economics (quantization, GPU class), so
the price must key on the **slug** (the concrete endpoint), not the abstract
model. `model_slug IS NULL` = global default; `tenant` set = per-tenant override
(mirrors `GetBillingRateCents`'s fallback ordering).

```sql
-- Raw per-request token usage (append-only, idempotent). Parallels UsageTick,
-- but token-shaped instead of GPU-interval-shaped.
CREATE TABLE TokenUsageEvent (
    gateway_request_id         VARCHAR(128) PRIMARY KEY,  -- ALWAYS a gateway-minted UUID; idempotency: ON CONFLICT DO NOTHING
    client_request_id  VARCHAR(128),              -- caller's X-Request-Id if present; correlation/support ONLY, never the dedup key; NULL if absent
    caller_tenant      VARCHAR NOT NULL,          -- tenant the request's AccessToken resolves to (both direct AND OR); always set
    model_slug         VARCHAR NOT NULL,          -- inferx/endpoint func name (= request `model`)
    source             VARCHAR(16) NOT NULL,      -- 'direct' | 'openrouter'
    prompt_tokens      BIGINT NOT NULL DEFAULT 0,
    completion_tokens  BIGINT NOT NULL DEFAULT 0,
    cached_tokens      BIGINT NOT NULL DEFAULT 0, -- subset of prompt_tokens (prompt_tokens_details)
    ts                 TIMESTAMPTZ NOT NULL,      -- REQUEST time captured on the gateway (NOT DB insert time; NO DEFAULT NOW() — see below); the rate is chosen by this ts
    gateway_id         BIGINT,
    processed_at       TIMESTAMPTZ                -- NULL = not yet billed; set by Pass-1 when debited. Two-state, exactly like UsageTick (billing.sql:24). A row with no rate stays NULL + alerts, and bills on a later run once a rate exists (self-healing — see 3.5).
);
CREATE INDEX idx_tokenevent_tenant      ON TokenUsageEvent(caller_tenant, ts);
CREATE INDEX idx_tokenevent_model       ON TokenUsageEvent(model_slug, ts);
CREATE INDEX idx_tokenevent_unprocessed ON TokenUsageEvent(ts) WHERE processed_at IS NULL;

-- Per-token price book (per endpoint slug). Parallels BillingRate, but the unit
-- is per-million-tokens and the key is model_slug (BillingRate keys on usage_type).
CREATE TABLE TokenRate (
    id                        SERIAL PRIMARY KEY,
    model_slug                VARCHAR,               -- NULL = global default; set = per-endpoint
    cents_per_million_input   BIGINT NOT NULL,
    cents_per_million_output  BIGINT NOT NULL,
    cents_per_million_cached  BIGINT NOT NULL DEFAULT 0,  -- discounted cached-input rate
    effective_from            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    effective_to              TIMESTAMPTZ,           -- NULL = currently active
    tenant                    VARCHAR,               -- NULL = default; set = per-tenant override
    created_at                TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    added_by                  VARCHAR NOT NULL
);
CREATE INDEX idx_tokenrate_lookup ON TokenRate(model_slug, effective_from);

-- Rollup target (per tenant × endpoint × hour). Parallels UsageHourlyByFunc.
-- Stores EXACT undivided numerators (like inference_numer), not rounded cents,
-- so the invoice can SUM(numer) across hours and divide ONCE (see below).
CREATE TABLE TokenUsageHourly (
    id                 SERIAL PRIMARY KEY,
    tenant             VARCHAR NOT NULL,       -- caller_tenant
    model_slug         VARCHAR NOT NULL,
    hour               TIMESTAMPTZ NOT NULL,
    input_tokens       BIGINT NOT NULL DEFAULT 0,   -- raw prompt tokens (usage truth); billing lives in input_numer
    cached_tokens      BIGINT NOT NULL DEFAULT 0,
    output_tokens      BIGINT NOT NULL DEFAULT 0,
    request_count      BIGINT NOT NULL DEFAULT 0,
    -- Exact numerators = Σ(tokens × cents_per_million), UNDIVIDED. Cents come from
    -- SUM(numer)/1_000_000 at the final invoice sum — never rounded per hour.
    input_numer        BIGINT NOT NULL DEFAULT 0,
    output_numer       BIGINT NOT NULL DEFAULT 0,
    cached_numer       BIGINT NOT NULL DEFAULT 0,
    charge_cents       BIGINT NOT NULL DEFAULT 0,   -- convenience/display only; authoritative = numerators
    UNIQUE(tenant, model_slug, hour)
);
CREATE INDEX idx_tokenhourly_tenant ON TokenUsageHourly(tenant, hour);
```

Rate lookup mirrors `GetBillingRateCents` and **must be event-time-bound**
(codex Medium): rate the usage at the price active **when the request happened**
(`TokenUsageEvent.ts`), not "latest".

**`ts` = gateway-captured request time, written explicitly — NOT the DB default
(codex Medium).** The invariant only holds if `ts` is the *request* time, not the
*insert* time. Because events are buffered async and inserts can be retried or
backfilled, `INSERT`-time can drift **past a price change**, which would misprice
the usage. So:
- The gateway captures `request_ts` in `shared_endpoint_dispatch` (before
  dispatch) and threads it to `extract_and_emit_usage`; the emitted event sets
  `ts = request_ts`.
- `TokenUsageEvent.ts` has **NO `DEFAULT NOW()`** — the column is `NOT NULL` and
  always written explicitly by the emitter. Relying on the DB default would break
  the pricing invariant.
- The rollup then joins each event to the rate active at *its* `ts`, so delayed
  rollups / backfills reprice correctly. Lookup:

```sql
-- pass the event's ts as p_ts; NOT NOW()
WHERE (model_slug = p_slug OR model_slug IS NULL)
  AND (tenant     = p_tenant OR tenant IS NULL)
  AND effective_from <= p_ts
  AND (effective_to IS NULL OR p_ts < effective_to)   -- time-bound, like GetBillingRateCents
ORDER BY (tenant IS NOT NULL) DESC,
         (model_slug IS NOT NULL) DESC,
         effective_from DESC
LIMIT 1
```

**Precision — exact numerators, divide-with-carry at the debit (decided; matches
GPU exactly).** Rounding each event (or each hour) to cents floors sub-cent
charges to 0 and compounds. GPU avoids this by pricing each tick into an **exact
undivided numerator**, then dividing **once, carry-aware, in the debit pass**
(`update-and-check-quota.sql`): `delta_cents = (carry + Σbatch_numer)/3600000`,
`new_carry = (…) % 3600000` — the remainder is **kept on `TenantQuota`**, not
lost. Tokens do the identical thing with divisor `1_000_000`. Two things to keep
straight (they mirror GPU's two passes, #15):

- **Debit pass (money):** reads `TokenUsageEvent` directly, prices to exact
  numerators, and divides **with carry** into `used_cents`: `delta =
  (token_carry_numer + Σ numer)/1_000_000`, `token_carry_numer = (…) % 1_000_000`.
  The divide-and-carry happens **here**, on `TenantQuota` — **not** via
  `TokenUsageHourly`. This is where the sub-cent correctness lives (`token_carry_
  numer`, exactly parallel to `inference_carry_numer`).
- **Aggregate pass (reporting only):** sums the exact `input/output/cached_numer`
  into `TokenUsageHourly` per (tenant, model, hour), undivided — for
  analytics/invoicing display. **Not** on the debit path; it never touches
  `TenantQuota` or the carry.

So per-event: store raw **counts**, no cents. The **carry on `TenantQuota` is
owned by the debit pass** (`update-and-check-quota.sql`), and it is what prevents
any sub-cent loss in the live credit subtraction.

**Carry the remainder across debit cycles — REQUIRED, mirrors
`inference_carry_numer` (grounded).** InferX debits `TenantQuota` incrementally
(not one end-of-month invoice), so the divide-to-cents happens **every debit
cycle**, and each cycle leaves a sub-cent remainder. GPU billing already solves
this: `TenantQuota` carries `inference_carry_numer` / `standby_carry_numer`
("Exact … numerator remainder / 3600000", `billing.sql:39-40`) forward so no
sub-cent is lost across cycles. Token billing debits the **same `TenantQuota`**
and has the **same** `× cpm / 1_000_000` remainder, so it needs the **same
carry** — add **`token_carry_numer`** to `TenantQuota` (parallel to
`inference_carry_numer`), and a cumulative **`token_used_cents`** (parallel to
`inference_used_cents` / `standby_used_cents`, `billing.sql:37-38`) so token spend
is tracked as its own ledger line, not merged into GPU spend. On each token debit:
`whole = (token_carry_numer + Σ new numer) / 1_000_000`; then `used_cents +=
whole`, `token_used_cents += whole`, and `token_carry_numer = (…) mod 1_000_000`.
Without the carry, every debit cycle drops <1¢ per tenant (systematic
under-bill) **and** the token path would round differently than the GPU path on
the same table.

**Why the carry lives on `TenantQuota`, not `TokenUsageHourly` (answers a natural
question).** The carry is a **single per-tenant running remainder** across debit
cycles — it needs one accumulator per tenant. `TokenUsageHourly` has *many* rows
per tenant (per model × hour), so there is no single place there to carry a
remainder forward. `TenantQuota` is one row per tenant — the correct home, exactly
as GPU puts `inference_carry_numer` there and not on `UsageHourlyByFunc`. The two
are complementary but on **different paths**: the debit pass prices
`TokenUsageEvent` → `TenantQuota` (carry lives here); `TokenUsageHourly.*_numer`
is the **reporting** breakdown, not the debit's input. So the hourly numerators
serve reporting, and the carry on `TenantQuota` (updated by the debit pass) is
what guarantees sub-cent correctness of the actual charge.

Do the `tokens × cpm` multiply in `i64`/`BIGINT` (huge headroom) and divide last.
This is also why `TokenRate` is an effective-dated **table**, not a mutable
`Endpoints` column: price lives O(price changes), events carry no price (no
duplication), and rating joins each event to the rate active at its `ts`.

**Quota enforcement = EVENTUAL, not a hard ceiling (decided; matches GPU-time,
codex Medium).** Be precise about the guarantee: the token path reuses the
existing `enforce_tenant_quota_for_request` (`http_gateway.rs:944`) precheck —
before dispatch, reject if `tenant.status.quota_exceeded && !quota_exempt`, keyed
on the resolved **`caller_tenant`** (the paying tenant), not physical `inferx`.
But this precheck reads a **quota flag that flips only *after* usage is debited**,
and the debit is **async** (event insert → rollup → `RecalculateTenantQuota`). So:

- **This is eventual enforcement, not a spend ceiling.** A tenant can **overshoot**
  its remaining credit with in-flight / concurrent requests before the flag flips,
  and — because event emit uses `try_send` and tolerates drop-on-overflow/crash
  (3.6) — some usage may never be debited at all (one-directional: under-bill).
- **This exactly matches the current GPU-time behavior** (same async debit + flag
  precheck), so token billing is consistent with what InferX already ships — not a
  regression, but also not stronger.
- **If a strict, no-overshoot spend cap is ever required**, this design does not
  provide it; that needs a **reservation/pre-auth** (decrement a hold before
  serving, reconcile after) or a **synchronous debit** on the request path. Out of
  scope for v1 — recorded as a limitation, not a hidden assumption.

OR needs no branch: as a comped-credit tenant (3.4) the same precheck applies.

**No-rate handling = guard every reachability action + a serving-time backstop
(decided).** Note there is **one `publish`** per endpoint (not a separate
"publish as shared") — it makes the endpoint reachable on **both** the dedicated
and the direct-shared surfaces at once. So `PublishEndpoint` fully covers the
direct path. The gap is **OpenRouter**, which is reachable *without* `published`
(decoupled). So the rate must be guaranteed on **each independent reachability
action**:

- **Direct path** reachability = `published`. **The token price is an input to
  the publish/deploy action** (decided) — the admin sets input/output/cached
  cents in the same form that deploys+publishes, and the handler **writes the
  `TokenRate` row atomically with (or immediately before) flipping
  `published = true`**. This resolves the first-publish chicken-and-egg: a new
  endpoint may not exist (hence has no price) until publish creates it, so the
  price rides along with the publish rather than being a precondition that can't
  be satisfied yet. Guard: `PublishEndpoint` (`http_gw.rs:233`) must not reach
  `published = true` (`:273`) without a `TokenRate` present — but since the form
  supplies it, the normal path always has one. (Note both layers: the raw gateway
  `PublishEndpoint` requires the func to already exist — `GetPlatformEndpointFunc`
  errors `NotExist`, `:552-564` — while the dashboard **catalog** publish/activate
  flow deploys the func first; the price-with-publish rule applies at the
  admin-facing catalog action so it covers the create-then-publish case.)
- **OpenRouter path** — the serving gate is **dropped** (no per-request DB, 3.3),
  so listing is *not* an admission boundary for OR. Optionally still require a
  `TokenRate` at `ListOnOpenRouter` (`http_gw.rs:384`) as a best-effort nudge
  (a listed model normally has a price), but the **authoritative** guarantee for
  OR is the serving-time backstop below — OR pricing is **detect, not prevent**.
- **No-rate handling = leave `processed_at` NULL + alert; it self-heals (decided;
  NO extra state column).** In normal operation the admission guards guarantee a
  price exists: a direct model can't be `published` without a rate, and OR only
  calls its `or_listed`+priced catalog. So an event reaching the debit with no
  active `TokenRate` is a **misconfiguration** (price-effective-date gap,
  unpublish/de-list race) or the dropped-OR-gate edge — a bug, not a normal
  outcome to model. Handling is deliberately minimal: Pass 1 simply **skips such a
  row (leaves `processed_at` NULL) and raises an alert.** No debit, no special
  state, no new column. Because the row stays unprocessed, once an operator adds
  the missing rate a **later Pass 1 run picks it up and bills it correctly** (the
  rate is chosen by the event's `ts`, so back-dating works) — it **self-heals**.

  > **Note for reviewers (why two-state `processed_at` is sufficient here):** an
  > earlier draft added a `bill_status`/`rating_state` column to distinguish
  > "debited" from "skipped-no-rate." That is **intentionally rejected** as
  > over-engineering. The no-rate case is a should-never-happen misconfiguration,
  > and the correct response to it is *not to permanently mark it* but to leave it
  > unprocessed + alert so it bills once fixed. GPU billing uses a plain two-state
  > `processed_at` for the same reason (its ticks are always rateable); tokens do
  > the same. The only guard against a *permanently* unpriceable row looping is the
  > **alert** (ops fix), not a schema state — which is correct, because a
  > row that can never be priced is a bug to fix, not a billing state to persist.

This replaces the earlier claim that "block at publish" alone guarantees no free
serving. Direct path **prevents** unpriced serving at publish (admission); the OR
path relies on the rollup no-rate exception (skip + alert, usage still metered) —
different guarantees, both non-silent (codex High + Medium).

Because rating is keyed on event time, the rollup joins each `TokenUsageEvent`
to the rate active at its `ts` before summing `charge_cents` into
`TokenUsageHourly` (i.e. rate-then-aggregate, or aggregate within a single rate
window — do not aggregate across a price-change boundary and then rate once).

### 3.6 Metering agent, persistence semantics

- Emit `TokenUsageEvent` via a **new** async mpsc agent (`TokenUsageAuditAgent`),
  modeled on `UsageTickAuditAgent` (`audit.rs:420-492`) — do **not** overload the
  GPU-time `UsageTick`. `gateway_request_id` PK + `ON CONFLICT DO NOTHING` → idempotent.
- At InferX's target **< 1k req/s**, plain Postgres handles per-request inserts
  easily; batched multi-row INSERT is a later optimization, not required now.
- **Durability:** in-memory buffering can lose the un-flushed batch on a hard
  crash — bounded (≤ one batch) and one-directional (only ever *under*-bill).
  Accept this + flush-on-shutdown to start; `gateway_request_id` idempotency lets
  durability be raised later (local WAL) without reworking the path. The existing
  `Audit()` uses `try_send` (drops on full channel, `audit.rs:450`) — inheriting
  that is acceptable but be deliberate.
- **Aborted requests — current extractor CANNOT meter them (codex Medium).** The
  `req_token.rs` extractor emits usage only when it observes the final `usage`
  object, and its streaming tee returns early if the client half goes away
  (`req_token.rs:121`). So on a client disconnect / cancel there is **no final
  `usage` chunk and no partial count** — the metering path produces *nothing*,
  not a partial `aborted` event. Consequences for this design:
  - As built, only **completed** requests (those that produced a final `usage`)
    are emitted at all; aborted/disconnected ones produce no event. So there is no
    stored "aborted" row to reason about in v1 (no `status` column needed — every
    persisted event is a completed one).
  - To bill aborted requests would require a real mechanism the current code
    lacks: e.g. request `stream_options.continuous_usage_stats` (per-chunk usage,
    already a field in `StreamOptions`, `req_token.rs:23-28`) so the last
    *observed* chunk carries a running count, **and** keep the response-tap task
    running to record that last-seen count even after the client drops (instead
    of the early `return`). This is extra work. **DECIDED: out of scope for v1 —
    aborted/disconnected requests are not billed** (v1 emits an event only when a
    final `usage` is observed, i.e. completed requests; aborted ones produce no
    event, so there is no `status` column). Revisit only if under-billing on aborts proves
    material.
- **No rating fork (3.4):** **all** rows (both sources) are rolled up per
  `caller_tenant`, rated against `TokenRate` into `TokenUsageHourly`, and debit
  `TenantQuota` identically. OpenRouter is an ordinary tenant with comped credit;
  its marketplace settlement is reconciled out-of-band. `source` is retained on
  the event for reporting only.

---

## 4. Implementation checklist

> **Status (server side landed).** Items 1–12, 14, 14a, 15, 15a are implemented and
> the gateway crate compiles: routing/dispatch/metering (`http_gateway.rs`,
> `req_token.rs` — the direct surface strips its `/endpoints` prefix so both sources
> reach the same `/v1/...` upstream), the `TokenUsageAuditAgent` + `SqlAudit`
> inserts/queries (`audit.rs`), the schema (`dashboard/sql/billing.sql` +
> `migrate_token_billing.sql`, also mirrored as the `migrate-token-billing.sql`
> configmap key), the token-rate admin API + publish guard (`http_gateway.rs`,
> `http_gw.rs`), and the two-pass SQL rollup: token debit folded into
> `update-and-check-quota.sql`, new `token-hourly-aggregate.sql` +
> `token-no-rate-alert.sql`. The schema migration runs **once** via a one-shot Job
> (`billing-token-billing-migrate-job.yaml`, mirroring the carry-migration
> `billing-tenant-quota-backfill-job`) — applied at rollout, not on any schedule.
> Neither cron re-runs DDL: the hourly-aggregate job runs Pass-2 + the no-rate
> alert, and `quota-check` runs the token-folded Pass-1 (`k8s/`+`k8s1/billing-cronjobs.yaml`);
> both assume the migrate Job has run, exactly like `quota-check` already assumes
> the carry migration.
> Gateway read APIs (`/admin/usage/tokens`, `/usage/tokens/:tenant`,
> `/billing/token-rates`, `/billing/token-rate/:slug`) and their dashboard proxies
> also landed. The dashboard UI (#17/#18/#18a/#19) is implemented on the endpoint
> detail + publish pages: a shared-serving info block + curl, an admin token-price
> section, an admin token-spend table, and price fields on the publish form that
> set the rate before publishing; prices are entered in dollars-per-million and
> converted to the cents-per-million the API stores.
> **Remaining:** #13 (drain exists but no shutdown hook triggers it — see item),
> #16 (manual `usage`-emission verification), and #3a (operational OR-key tenant
> provisioning). The GPU carry *verify* scripts were extended to carry
> `token_used_cents` as a trusted stored term in their used/balance/quota
> expectations, so they no longer false-fire (or error) for token-billed tenants
> while still verifying the GPU carry math exactly.

**Serving / routing**
- [x] **1.** Add routes: `POST /endpoints/v1/chat/completions` → `SharedEndpointCompletions`
   (the chat/dispatch+metering handler), and `GET /endpoints/v1/models` → a
   **separate** models-list handler (`SharedEndpointModels`, per #7 — NOT
   `SharedEndpointCompletions`). **`/completions` (legacy) is OUT of v1 scope**
   (codex Medium): the dispatch/pseudocode is chat-specific. It can be added later
   the way OR already supports both — by preserving the incoming `remainPath`
   during rewrite (`http_gateway.rs:4254`) rather than hardcoding
   `/v1/chat/completions` — but only after confirming `usage` semantics hold for
   it too (#16).
- [x] **2.** Extract `shared_endpoint_dispatch(req, target_prefix, gateway_request_id,
   caller_tenant, source)` from the current `OpenRouterChatCompletions` body
   (parse model → rewrite → `FuncCall1` → meter). Both sources now use the same
   physical target; what differs is the caller admission/gate before dispatch
   (3.2/3.2a).
- [x] **3.** `OpenRouterChatCompletions` → thin wrapper: `source=OpenRouter`, target =
   physical `/funccall/inferx/endpoint`, **drop the `or_listed` serving gate**
   (`http_gateway.rs:4273-4285`) — removes the per-request `GetEndpointForListing`
   DB lookup. OR is trusted (RBAC-scoped); usage is metered regardless, and the
   (theoretical) unpriced case is caught at rollup (no-rate → skip + alert,
   3.3/3.5). `or_listed` still gates **`/v1/models`** (catalog only).
   **Also resolve `caller_tenant` from the OR key's token** (`resolve_caller_
   tenant`, 3.2b) and pass it to the meter — OR traffic is token-counted on the
   InferX side, attributed to that tenant (3.4). The token is already in scope
   (`http_gateway.rs:4250`) but unused for attribution today.
- [ ] **3a.** **Provision + validate the OR provider key's tenant (codex Medium — PREREQ
   for #3).** OR requests attribute/debit only if the provider key resolves to a
   real tenant via `resolve_caller_tenant` (3.2b). Operational step: issue the OR
   key with `restrictTenant` set to the comped OR tenant, and **validate at
   startup/onboarding** that `resolve_caller_tenant` succeeds for it — otherwise
   every OR request rejects at 3.2b ("no tenant context"). Do not ship #3 without
   this.
- [x] **4.** `SharedEndpointCompletions` → thin wrapper: `source=Direct`, target =
   physical `/funccall/inferx/endpoint`. Resolve `caller_tenant` per 3.2b
   (reject if no tenant context), gate on `funcstatusMgr.Get(...).published`,
   **call `enforce_tenant_quota_for_request(token, gw, caller_tenant, …)` and
   short-circuit if it rejects** — **eventual** enforcement (flag flips after
   async debit; allows overshoot; matches GPU-time, 3.5), not a hard ceiling.
   Mint `gateway_request_id`, then dispatch. Same precheck in the OR wrapper (#3),
   keyed on the OR key's resolved `caller_tenant`.
- [x] **5.** **Direct-path auth — implement option 2 (DECIDED & security-reviewed;
   3.2a).** `SharedEndpointCompletions` authenticates the caller
   (authenticated + inference scope + resolved `caller_tenant` + `published`),
   then dispatches to the physical `inferx/endpoint/{model}` path passing an
   **internal auth-bypass parameter** so `FuncCall1` does not re-run its normal
   `IsNamespaceInferenceUser(inferx, endpoint)` check (`http_gateway.rs:2090`)
   for that trusted call. Constraints for the reviewer:
   - the bypass is an **internal function parameter / trusted code path**, never
     a client-controlled header or request field;
   - it applies **only** to the shared `inferx/endpoint` namespace reached via
     `/endpoints/v1/*` — generic `/funccall/*` auth is unchanged;
   - it must not reopen the `is_blocked_public_endpoint_inference` block
     (`http_gateway.rs:2072`).
   Rejected: broadly granting tenant users `inferx/endpoint` RBAC (makes the
   `published` gate non-load-bearing).
   **Effort note:** `FuncCall1(token, gw, req)` has **no bypass param today**
   (`http_gateway.rs:2038`) — adding one changes its signature and touches its
   **6 existing call sites** (`tokenizer.rs:625,833,925`, `req_token.rs:106`,
   `http_gateway.rs:2027,4305`), which must pass the no-bypass default. Small and
   mechanical, but it's the most invasive item — everything else is additive.
   Alternative that avoids the signature change: factor the post-auth body of
   `FuncCall1` into an inner fn the shared dispatch calls directly, leaving
   `FuncCall1`'s public signature untouched.
- [x] **6.** Factor tenant resolution into one shared `resolve_caller_tenant(token)`
   (`restrictTenant` → `defaultTenant` → reject) reused by **both** wrappers
   (#3 OR, #4 direct) — `resolve_skill_calling_tenant` (3.2b) is the model. (This
   is the single implementation of the resolution #3/#4 both invoke.)
- [x] **7.** `GET /endpoints/v1/models`: enumerate the shared funcs under
   `inferx/endpoint` and filter to `funcstatusMgr.Get(...).published == true`
   (the same informer source the direct serving gate uses, 3.3 — no DB). Emit a
   **distinct, plain OpenAI `/v1/models`** response — **not** the OpenRouter
   catalog.
   Do **not** reuse `BuildOpenRouterModelEntry` (`http_gw.rs:2592`): that is the
   OpenRouter provider schema (emits `pricing`, `openrouter.slug`,
   `discount_to_user`, `is_ready`, `quantization`, … and is gated on `or_listed`)
   — leaking those into the direct tenant API would repeat the OR-vs-direct
   conflation. Emit only the standard OpenAI shape:
   `{ "object":"list", "data":[ {"id":"<slug>","object":"model","created":<ts>,
   "owned_by":"inferx"}, … ] }`. The two `/…/models` endpoints stay separate:
   OpenRouter `/v1/models` (OR schema, `or_listed`) vs direct `/endpoints/v1/models`
   (OpenAI schema, `published`).

**Metering**
- [x] **8.** In `shared_endpoint_dispatch`, mint a gateway-owned **UUID**
   `gateway_request_id` (always; the PK) **and capture `request_ts` = request
   time on the gateway** (before dispatch); thread both to the emit. Set
   `TokenUsageEvent.ts = request_ts` explicitly — **the column has no
   `DEFAULT NOW()`**; insert time must never be used, or event-time rating breaks
   under async/retry/backfill (3.5). Capture the caller's `X-Request-Id` (if
   present) into `client_request_id` for correlation only — never the dedup key
   (3.2c).
- [x] **9.** Factor `req_token.rs`'s `usage` extraction + streaming-tee into a reusable
   `extract_and_emit_usage(resp, gateway_request_id, client_request_id, caller_tenant, model, source, request_ts)`
   helper; **add** `prompt_tokens_details.cached_tokens` parsing (not read today)
   and `caller_tenant` attribution (replace the `error!` log with an event emit).
   v1 emits an event only when a final `usage` is observed (completed requests);
   no event for aborted ones (no `status` column). **Leave a `TODO` at the
   early-return / no-final-`usage` branch** noting aborted/disconnected requests
   are intentionally not billed for now (3.6) — a visible, revisitable marker at
   the site where partial-usage metering would later hook in.
- [x] **10.** Call the helper inline at the end of `shared_endpoint_dispatch` (covers both
    sources).

**Persistence**
- [x] **11.** New tables `TokenUsageEvent`, `TokenRate`, `TokenUsageHourly` (3.5) in
    `dashboard/sql/billing.sql` (or a migration). **One existing table changes:**
    add **`token_carry_numer`** and **`token_used_cents`** (`BIGINT NOT NULL
    DEFAULT 0`) to `TenantQuota` — parallel to `inference_carry_numer` /
    `inference_used_cents` (`billing.sql:37-39`): sub-cent carry across token debit
    cycles + a separate cumulative token-spend line (3.5). All other existing
    tables untouched.
- [x] **12.** New `TokenUsageAuditAgent` (mpsc + per-request insert into `TokenUsageEvent`,
    modeled on `UsageTickAuditAgent`). `gateway_request_id` PK, `ON CONFLICT DO NOTHING`.
- [ ] **13.** Flush-on-shutdown hook (agent already has a `Close()`/`closeNotify` model).
    PARTIAL: `TokenUsageAuditAgent::Close()` drains the buffer on `closeNotify`
    (`audit.rs`), but nothing calls it — the gateway has no graceful-shutdown path
    (`axum::serve(...).await.unwrap()`, no signal handler), and the sibling
    `USAGE_TICK_AGENT` is unwired the same way. Wiring requires adding graceful
    shutdown to both the plain and TLS serve paths and closing the agents there.

**Rating** — `TokenRate` is an effective-dated table (NOT an `Endpoints`
column): it gives price history, `effective_from`/`effective_to` scheduling,
per-tenant override, and event-time-bound rating with **no per-event price
duplication** (events store counts only). Mirror the existing GPU-rate flow
(`BillingRate`), not the OR-marketplace pricing flow.
- [x] **14.** Admin API to set `TokenRate` rows (per `model_slug`, optional tenant
    override), **modeled on `AddBillingRate` (`audit.rs:889`)** — same
    effective-dated insert pattern, sibling to GPU rates. Plus a
    `GetTokenRateCents`-style lookup fn mirroring `GetBillingRateCents` —
    **event-time-bound** (rate at `event.ts`, not `NOW()`; 3.5).
- [x] **14a.** **Rate guard: direct at publish, OR at rollup (3.5; codex High).** Direct
    and OR guarantee pricing differently:
    - **Direct path** — the price is supplied by the **catalog publish/deploy
      form** (the admin-facing action that deploys-then-publishes, `app.py`
      `AdminCatalogPublish`/`set_catalog_entry_active` → catalog deploy) and
      written atomically with going live (3.5, #17). Enforcement backstop in the
      gateway `PublishEndpoint` (`http_gw.rs:233`): must not reach `published=true`
      (`:273`) without a `TokenRate` present. So the normal path always carries a
      rate (form supplies it); the gateway check is the safety net, not a blocking
      precondition. (Raw `PublishEndpoint` also requires the func to already
      exist, `GetPlatformEndpointFunc` → `NotExist`, `:552-564`; the catalog flow
      creates it first.)
    - **OR path has no admission gate** (`or_listed` serving gate dropped, 3.3);
      OR only calls its `or_listed`+priced catalog, so this is normally a non-issue.
      Optionally require a `TokenRate` at `ListOnOpenRouter` (`http_gw.rs:384`) as
      a best-effort nudge.
    - **No-rate handling (exception, self-healing, no extra column):** if Pass 1
      finds no active `TokenRate` for a row at its `ts`, **leave `processed_at`
      NULL, do not bill, and alert.** The row bills on a later run once the rate is
      added (self-heals). No `bill_status`/state column — two-state `processed_at`
      like `UsageTick` (3.5).
- [x] **15.** Rollup — **extend the existing billing SQL cron/scripts, do NOT write a
    bespoke read-then-update loop** (codex High ×2). Mirror the GPU pipeline's
    **two independent passes** over its usage table (verified in
    `k8s1/billing-sql-scripts-configmap.yaml`) — do NOT merge them:

    **Pass 1 — token quota-debit — FOLD INTO `update-and-check-quota.sql`, do NOT
    make a parallel script (design decision).** Today that script is hardcoded to
    `UsageTick` (`FROM UsageTick … WHERE processed_at IS NULL`, `configmap:33`) and
    does GPU-only math; it does **not** see token data. Because `used_cents` /
    `balance_cents` / `quota_exceeded` must be computed from **inference + standby
    + token together in one expression** (#15a), a *separate* token script would
    race the GPU script on the same `TenantQuota` fields (each recomputes balance
    from its own view and clobbers the other). So extend the **one** script: add a
    second `SKIP LOCKED` batch CTE `FROM TokenUsageEvent WHERE processed_at IS NULL`,
    a token `agg` (÷ 1_000_000), and fold the token delta into the existing
    `calc`/`updated` CTEs so both usage types debit `TenantQuota` in a single atomic
    statement. Steps below detail the token half; they extend, not replace, the GPU
    logic:
    - **Seed `TenantQuota` from token tenants too (codex Medium):** the bootstrap
      inserts tenants seen in `UsageTick ∪ TenantCreditHistory` (`configmap:11-19`)
      — a **token-only** caller has **no `TenantQuota` row**, so the debit's
      `UPDATE` matches zero rows and silently drops the charge. Extend the
      bootstrap `UNION` with `SELECT caller_tenant FROM TokenUsageEvent WHERE
      processed_at IS NULL` (`ON CONFLICT DO NOTHING`).
    - **Concurrency-safe claim (High):** claim events with `WHERE processed_at IS
      NULL … FOR UPDATE SKIP LOCKED` in a single atomic `WITH` that **sets
      `processed_at`** in the same statement (exact pattern at `configmap:24,37,
      148`). A "SELECT then UPDATE" double-bills on overlap.
    - **Debit + carry (rows WITH an active rate):** claim only events that have an
      active `TokenRate` at `ts`; `whole = (token_carry_numer + Σ numer) /
      1_000_000`; `used_cents += whole`, `token_used_cents += whole`,
      `token_carry_numer = (…) mod 1_000_000` (3.5); recompute
      `balance_cents`/`quota_exceeded` incl. the token term (**#15a**); set
      `processed_at = now()`.
    - **No-rate rows: leave `processed_at` NULL (do not claim/debit) + alert.**
      They self-heal — a later run bills them once a rate is added (3.5). **No
      `bill_status` column.**
    - Uniform, no `source` branch (3.4); OR is a comped-credit tenant.

    **Pass 2 — token hourly aggregate** (reporting; counterpart of
    `hourly-aggregate.sql`, `configmap:817`):
    - **Recomputes price from raw events — does NOT read the debit's output.** Like
      the GPU aggregate (which calls `GetBillingRateCents` over raw `UsageTick`,
      `configmap:833`, independent of `update-and-check-quota.sql`), Pass 2 reads
      raw `TokenUsageEvent` and calls **`GetTokenRateCents(model_slug, ts, tenant)`
      itself** to build `input/output/cached_numer`. It reuses the same rate
      *function*, not any value the debit wrote. Both passes agree because price is
      a pure function of (slug, `ts`, tenant) via the effective-dated `TokenRate` —
      no shared intermediate to sync.
    - Input rule matches the **real** GPU aggregate: filter by a **self-healing
      time window** on `ts` (GPU: `tick_time >= date_trunc('hour', NOW() - INTERVAL
      '3 hours') AND < date_trunc('hour', NOW())`, `configmap:835-836`) — **not**
      `processed_at`. (The earlier `configmap:547` citation was wrong: that is
      `verify-tenant-quota-live.sql`, not the aggregate.) A no-rate row: the rate
      lookup returns nothing, so it contributes no numerator and is simply absent
      until a rate is added and a later window picks it up — same self-healing as
      the debit, with no coordination between the two passes.
    - Upsert into `TokenUsageHourly` with **`ON CONFLICT (tenant, model_slug,
      hour) DO UPDATE SET … = EXCLUDED.…`** — i.e. **REPLACE the bucket, not add**
      (each run recomputes the whole hour from raw events and overwrites). This is
      what makes the overlapping self-healing window idempotent — matches GPU
      (`configmap:882-890`, `SET numer = EXCLUDED.numer`). **Adding would
      double-count** on window overlap. Store `input/output/cached_numer`
      undivided (3.5).
    - Does **not** touch `TenantQuota` or `processed_at` — pure reporting,
      decoupled from the debit, exactly as GPU separates them.
- [x] **15a.** **Close the quota-enforcement loop — REQUIRES editing the recompute SQL
    (codex; High/Medium).** Admission reads `tenant.status.quota_exceeded`
    (`http_gateway.rs:780,944`), not `TenantQuota.used_cents`. The GPU path bridges
    them: recompute `used_cents`/`balance_cents`/`quota_exceeded` in SQL, then sync
    changed flags into tenant status (`update-and-check-quota.sql` +
    `sync-tenant-quota-exceeded.sh`, `configmap:8,168,207`).
    **This is NOT automatic — it is a concrete required SQL change.** The recompute
    (`configmap:119-134`) derives all three fields from **only** `inference` +
    `standby` terms:
    ```sql
    used_cents     = inference_used_cents + standby_used_cents + Δinference + Δstandby
    balance_cents  = total_credits − (inference + standby + deltas)
    quota_exceeded = (total_credits − (inference + standby + deltas)) <= threshold
    ```
    There is **no `token_used_cents` term.** As-is, this recompute would **overwrite
    `used_cents` with a value that excludes token spend** — token debits would be
    erased on the next recompute and `quota_exceeded` would never reflect token
    usage. **Required:** edit the recompute SQL so `token_used_cents` (+ its delta)
    is added into the `used_cents`, `balance_cents`, and `quota_exceeded`
    expressions, exactly parallel to `inference`/`standby`. Then the status-sync
    step already propagates the flag unchanged. Do NOT treat this as "verify" — it
    will not work without the SQL edit.

**Verify before promising features**
- [ ] **16.** **`usage` presence is a go/no-go for billing, not just a cache detail (codex
    Medium).** Metering emits only from the final `usage` object (#9), and
    `usage` is **not guaranteed** — the OR provider doc records it (G10,
    `openrouter-provider-listing.md:119`: "`usage` not guaranteed … Token
    accounting / billing breaks"; `include_usage` is best-effort). Before shipping:
    (a) **confirm the shared `inferx/endpoint` engine (vLLM) always emits `usage`
    for supported models** with our injected `stream_options.include_usage=true`;
    if any supported model/path can omit it, add a **fallback** (e.g. gateway-side
    tokenizer count) or **scope-restrict** shared serving to engines/models that
    guarantee `usage`. A request that serves but yields no `usage` is currently
    **unbilled** — decide explicitly whether that's acceptable or must be blocked.
    (b) Separately, confirm `prompt_tokens_details.cached_tokens` is populated
    before promising a cache discount (vLLM APC does not always set it).

**Dashboard / admin UI** (Python/Flask in `dashboard/`, `app.py` — a separate
codebase from the gateway; grounded below). The gateway already exposes the
admin API pattern to reuse: GPU rates are set via `POST /billing/rates` →
`AddBillingRate` (`http_gateway.rs:1294,5143`), and usage via
`/admin/usage/endpoints` (`:1321`), which `app.py` already proxies
(`app.py:9325,9364`).
- [x] **17.** **Token-rate admin — on the endpoint detail page (DECIDED placement).** GPU
    rate is **global**, so it lives on the global **Admin | Billing Admin** tab
    ("Add Billing Rate", `templates/admin.html:1548`). Token rate is **per-endpoint
    / model** (`TokenRate.model_slug`), so it belongs on the **endpoint detail
    page** as an **InferX-admin-only** section (set/update the model's token
    price), next to the price's subject — not on the global billing page.
    - Backend: gateway endpoint mirroring `AddBillingRate`
      (`POST /billing/token-rates` per slug, + history GET).
    - Dashboard: an admin-gated price section on `endpoint_detail.html`, rendered
      only when `dashboard_is_inferx_admin` (`app.py:122`, `is_inferx_admin_user()`
      `:1089`) — the same admin flag/pattern already used elsewhere. Used to
      **update** the price on an already-live endpoint.
    - **First price is set at publish (DECIDED):** the token price is an input to
      the publish/deploy form, written atomically with going live (3.5) — so a new
      endpoint is never published rateless, and this endpoint-detail section is for
      subsequent edits. The publish form needs input/output/cached price fields.
- [x] **18.** **Token usage / spend display** — the existing `/admin/usage/endpoints` shows
    endpoint (GPU) usage; add token/spend views over `TokenUsageHourly`
    (input/output/cached tokens + `charge_cents` per tenant × model × period) for
    both the admin cross-tenant view and the per-tenant view. New gateway usage
    endpoint(s) + dashboard proxy/render.
- [x] **18a.** **Shared-endpoint info on the endpoint detail page** — the page already
    renders the **dedicated** URL `{base_url}/funccall/{tenant}/endpoints/{slug}/v1`
    (`app.py:5341`, `templates/endpoint_detail.html`). **There is no separate
    "publish as shared" — the same `published` flag makes an endpoint reachable
    on both surfaces** (one publish; `funcstatus.published` gates both the
    dedicated virtual path and the shared direct path, `http_gateway.rs:861`). So
    on the **same** published condition the page already uses for the dedicated
    URL, also show the **shared** serving info *alongside* it: the shared base URL
    `{base_url}/endpoints/v1` (OpenAI-compatible, **model in body** — no
    per-tenant/slug in the path, 3.1), the token price (`TokenRate` for the slug),
    and an SDK/curl snippet. No extra shared-publish state to check. Do this
    **after** the serving/metering path lands (it describes a live capability).
- [x] **19.** **Publish/List rate-guard error surfacing** — the new "no `TokenRate` → reject
    publish/or-list" guards (#14a) return errors; the endpoint publish and
    OpenRouter-list admin screens must surface them clearly ("set a token price
    before publishing/listing").

(No separate direct-API onboarding UI for v1: showing the shared `/endpoints/v1`
base URL + snippet alongside the dedicated URL on the endpoint detail page —
#18a — is sufficient.)

---

## 5. Open questions

- **Failed / cancelled / aborted requests** — bill prefill done? (GPU work
  happened even if the client disconnected.) **Note (codex Medium):** the current
  extractor cannot observe partial usage on abort at all (no final `usage` chunk;
  tee returns early, `req_token.rs:121`). So "bill aborted" is not just a policy
  choice — it needs new mechanism (`continuous_usage_stats` + keep the tap alive
  after client drop; see 3.6). v1 default: don't bill aborted.
- **OpenRouter settlement reconciliation** — rating is **not** forked: OR events
  are rated against `TokenRate` and debit `TenantQuota` exactly like direct
  (3.4). OR is modeled as a tenant topped up with comped credit; the open item is
  purely *operational* — how the OpenRouter marketplace payment is reconciled
  against that comped credit out-of-band (finance/ops, not the billing code
  path). This does not change schema, rollup, or quota logic.
- **Strict spend cap (if ever required)** — v1 quota enforcement is **eventual**
  (async debit, precheck flag flips after the fact → in-flight overshoot
  possible; some usage may go undebited on `try_send` drop/crash — one-directional
  under-bill). This matches current GPU-time behavior. A true no-overshoot ceiling
  would need a **reservation/pre-auth** (hold before serving, reconcile after) or
  **synchronous debit** — out of scope for v1 (3.5).
- **OR unpriced-serving is detect, not prevent (accepted)** — dropping the
  `or_listed` serving gate (for zero per-request DB, 3.3) means an unpriced
  deployed func *could* serve OR before it's priced. This is accepted: usage is
  always metered+attributed, and the case is caught at rollup (no-rate → skip +
  alert, `processed_at` set, unbilled — 3.5), not silent. If admission-time
  prevention for OR is ever wanted, re-add the gate as a **cached** `or_listed`
  check (informer, like `published`) — restores prevention without the DB hit.
- **Per-tenant fairness on the shared instance** — one caller with huge-context
  / high-QPS traffic can starve co-tenants (head-of-line blocking, KV eviction).
  Rate-limit / quota / fair scheduling at the gateway is likely still new
  control-plane work; scope it separately.
- **Cross-tenant prefix-cache policy** on the shared model — likely disable
  cross-tenant KV reuse (content-leak risk), allow intra-tenant.
- **`X-Inferx-Model*` headers** — left dormant for now (authored elsewhere / by
  the OpenRouter integration; write-only, no reader). Not removed.

---

## Appendix A — cost reasoning (background)

- **GPU-time vs token, same workload:** a wash — token price is GPU-$/sec ÷
  (tokens/sec at an assumed utilization). The billing unit only decides **who
  bears utilization risk**: GPU-time → tenant; token → provider.
- **Dedicated vs shared:** sharing raises aggregate occupancy (peers fill idle
  gaps), amortizing fixed GPU cost 2–4× — a real efficiency gain, gated by
  **VRAM/KV-cache ceiling** first and **interference/tail-latency** second.
- **InferX cold start caveat:** scale-to-zero already reclaims idle *temporally*
  (frees the GPU between requests), so the occupancy advantage of sharing
  shrinks. Deciding variable becomes **cold-start cost vs inter-request gap**:
  bursty/low-duty-cycle → dedicated+scale-to-zero wins; sustained/high-density →
  shared+token wins. Token billing therefore targets the **sustained, hot,
  shared** workloads specifically.

## Appendix B — `usage` field semantics

- `completion_tokens` = **output only**; `total_tokens = prompt_tokens +
  completion_tokens` (no overlap).
- Streaming: `usage` is populated only in the **final chunk**, and only with
  `stream_options.include_usage=true` (`req_token.rs` already injects it).
- **Cached tokens** are a **subset of prompt tokens**: OpenAI/vLLM report
  `prompt_tokens_details.cached_tokens` (subset of `prompt_tokens`); billable
  input = `(prompt_tokens − cached_tokens)` @ full rate + `cached_tokens` @ cache
  rate. (Anthropic accounts differently — separate top-level cache read/write
  fields — noted only for cross-provider normalization.)
