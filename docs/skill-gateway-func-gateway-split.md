# SkillGateway / FuncGateway Split Design

## Background

InferX skills currently execute entirely inside the InferX gateway. The gateway handles two distinct responsibilities in one codebase:

1. **Skill chain execution** — the agentic loop: calling the model, detecting `call_skillep` tool calls, recursively invoking child skills, streaming trace events.
2. **Function routing** — resolving a skill to a physical function pod and dispatching the HTTP request to it (InferX-internal infrastructure).

Both live in the same binary today. The model calls in the skill chain loop go through `dispatch_func_call` / `HttpGateway`, which is tightly coupled to InferX's pod routing internals.

## Motivation

We want to open-source the skill chain execution component so the community can use it independently — plugging in their own model backends (OpenAI, OpenRouter, local Ollama, etc.) without needing InferX's proprietary function infrastructure.

This requires a clean separation:

- **SkillGateway** — open source. Owns the skill chain loop, `call_skillep` tool definition, child skill routing, and trace. Has no knowledge of InferX pod management. Works with any OpenAI-compatible model endpoint.
- **FuncGateway** — InferX proprietary. Owns pod lifecycle, function routing, tenant billing, InferX-specific auth. Remains closed source.

## Current Architecture

```
Client
  │  (Keycloak JWT or API key)
  ▼
InferX Gateway (single binary)
  ├── Keycloak auth middleware
  ├── SkillCall handler        ← skill DB lookup, tenant resolution, quota check
  │     └── handle_skill_call_chain(gw: &HttpGateway, route: FuncRouteTarget, ...)
  │           └── execute_skill_chain
  │                 ├── model turn: dispatch_func_call(HttpGateway, FuncRouteTarget, ...)
  │                 │         └── InferX function pod (127.0.0.1:PORT/v1/chat/completions)
  │                 └── child skill turn: HTTP POST 127.0.0.1:PORT/skills/{owner}/{ns}/{name}/...
  │                           └── re-enters full gateway (auth + quota + skill chain)
  │
  └── http_gateway.rs / func_agent_mgr.rs  ← pod routing (InferX-internal)
```

### What FuncGateway Does Before Calling skill_chain.rs

The `SkillCall` handler in `http_gateway.rs` does the following before calling `handle_skill_call_chain`:

| Step | Notes |
|---|---|
| Keycloak auth | `Extension(token): Extension<Arc<AccessToken>>` — JWT or InferX API key |
| Scope check | `token.CheckScope("inference")` |
| Skill lookup | `gw.sqlSecret.GetSkill(owner, ns, name)` — SQL DB |
| Visibility check | `skill.is_published \|\| token.IsNamespaceInferenceUser(...)` |
| Cache readiness | `skill.cache_status != "ready"` → 503 |
| System prompt load | `load_skill_prefix(&skill)` — reads from `/opt/inferx/skills/` on disk |
| Func route resolution | `gw.objRepo.GetFunc(...)` → `FuncRouteTarget` — pod routing |
| Calling tenant resolution | `resolve_skill_calling_tenant(&token, &skill, hint)` — billing target |
| Quota check | `tenant_quota_state(&gw, payer)` — GPU quota enforcement |
| Billing logic | `logical_tenant` / `logical_funcname` computation (owner vs caller billing) |

Everything in that table is InferX-internal and stays in FuncGateway.

### Coupling Points Inside skill_chain.rs

Three coupling points bind `skill_chain.rs` to InferX internals:

**1. Public handler signatures** — typed on `HttpGateway` and `FuncRouteTarget`:
```rust
// skill_chain.rs:1893
pub(super) async fn handle_skill_call_chain(
    gw: &HttpGateway,
    route: FuncRouteTarget,
    calling_tenant: String,
    logical_funcname: String,
    ...
```

**2. Model turn** — calls `dispatch_func_call` which drives InferX pod routing:
```rust
// execute_skill_chain ~line 1691
let mut model_resp = dispatch_func_call(gw, model_req, route.clone(), ...).await;
```

**3. Child skill URL** — uses `GATEWAY_CONFIG.gatewayPort` to call back into the same gateway:
```rust
// ~line 1042
let child_url = format!(
    "http://127.0.0.1:{}/skills/{}/{}/{}/v1/chat/completions",
    GATEWAY_CONFIG.gatewayPort, ...
);
```

Note: today child calls go to the same gateway that runs FuncGateway, so each child re-runs the full per-child access control (scope, visibility, cache readiness, calling-tenant resolution, quota). This is intentional and must be preserved.

## Proposed Design

### Deploy SkillGateway as a Separate Service — Even in InferX

The key principle: **InferX must use the same code path as open-source users.** If SkillGateway is embedded in the FuncGateway binary with a hidden fast path, bugs accumulate in the open-source path invisibly. Deploying SkillGateway as a separate service in InferX forces both environments to exercise identical code.

```
Client
  │  (Keycloak JWT or API key)
  ▼
FuncGateway  (InferX proprietary, closed source)
  ├── Keycloak auth middleware
  ├── SkillCall handler: skill DB lookup, tenant resolution, quota, billing
  │     └── HTTP POST → SkillGateway /skills/{owner}/{ns}/{name}/v1/chat/completions
  │           Authorization: <original-caller-token>   ← forwarded to child calls
  │           X-Skill-Service-Key: <internal-secret>   ← SkillGateway service auth
  │
  └── /v1/chat/completions for InferX-hosted models ← child calls and model turns land here
             ▲                ▲
             │ model turn     │ child call_skillep (re-enters FuncGateway for per-child auth)
             │                │
SkillGateway  (open source, separate service)
  ├── X-Skill-Service-Key auth middleware
  ├── skill store (OSS only: system prompt + ModelEndpoint per skill, looked up by skill ID)
  ├── execute_skill_chain loop
  │     │   (system_prompt + ModelEndpoint come from per-request config in InferX,
  │     │    or from skill store lookup in OSS)
  │     ├── model turn  → ModelEndpoint.url
  │     └── call_skillep → skills_base_url/skills/{owner}/{ns}/{name}/...
  └── trace SSE stream
```

### Three Configuration Parameters That Replace the InferX Coupling

| Parameter | Replaces | InferX value | Open-source value |
|---|---|---|---|
| `ModelEndpoint { url, headers }` per skill | `dispatch_func_call` + `FuncRouteTarget` | FuncGateway's `/v1/chat/completions` for the skill's func | External API URL (OpenRouter, OpenAI, etc.) |
| `skills_base_url` | `GATEWAY_CONFIG.gatewayPort` in child URL | FuncGateway's base URL — child calls re-enter FuncGateway for per-child auth | SkillGateway's own base URL (users provide their own auth layer) |
| SkillGateway HTTP API | `handle_skill_call_chain` / `handle_skill_debug_call` typed on `HttpGateway` + `FuncRouteTarget` | FuncGateway calls SkillGateway over HTTP after resolving all InferX concerns | Community calls SkillGateway directly |

### Preserving Per-Child Access Control

This is the critical invariant. Today every `call_skillep` child call goes to `127.0.0.1:{PORT}/skills/...`, which is FuncGateway, re-running scope check, visibility, cache readiness, calling-tenant resolution, and quota for the child skill.

In the proposed design, `skills_base_url` **must point to FuncGateway** in InferX deployments, not to SkillGateway itself. Child calls carry the original inbound `Authorization` header (via `copy_forwarded_child_headers`), so FuncGateway can fully validate each child.

```
Parent skill (running in SkillGateway)
  │ call_skillep → skills_base_url/skills/acme/default/pricing/v1/chat/completions
  │                (Authorization: <original caller token>)
  ▼
FuncGateway  ← full per-child access control runs here
  └── calls SkillGateway again for the child skill's chain
```

For open-source deployments without FuncGateway, `skills_base_url = SkillGateway URL`. Users who need per-child access control place a reverse proxy or auth middleware in front of SkillGateway.

### Auth Header Rules

There are three flows with different header requirements. They must not be mixed.

#### SkillGateway uses `X-Skill-Service-Key` for all inbound auth

`X-Skill-Service-Key` is SkillGateway's API key. All callers — FuncGateway and direct OSS clients alike — must present it. It is not a service-to-service-only mechanism; it is simply the configured secret that gates access to SkillGateway.

`Authorization` is not used by SkillGateway for its own auth. It is exclusively a pass-through: forwarded unchanged to child `call_skillep` calls (where FuncGateway uses it for per-child access control), and stripped before any model turn. This separation is required because if SkillGateway's own auth rode in `Authorization`, child calls would replay the wrong credential to FuncGateway.

```
FuncGateway → SkillGateway:
  X-Skill-Service-Key: <shared-secret>            ← SkillGateway validates this
  Authorization: Bearer <original-caller-token>   ← forwarded to child calls, not inspected by SkillGateway

OSS client → SkillGateway:
  X-Skill-Service-Key: <configured-secret>        ← same mechanism, same header
  Authorization: Bearer <anything>                ← optional; forwarded to child calls if present
```

#### Child skill calls — forward inbound headers, conditionally inject service key

`copy_forwarded_child_headers` forwards `Authorization`, `X-Request-Id`, and `traceparent` from the inbound request. The inbound `Authorization` is the original caller's token, so FuncGateway receives it and can enforce per-child access control (scope, visibility, quota) for the child skill.

Whether SkillGateway also injects `X-Skill-Service-Key` into child calls depends on the target. SkillGateway config has a `child_service_key: Option<String>` field:

- **Set** (OSS deployment, `skills_base_url` = SkillGateway itself): inject `X-Skill-Service-Key` so the child call passes SkillGateway's own auth middleware.
- **Not set** (InferX deployment, `skills_base_url` = FuncGateway): do not inject. FuncGateway has its own auth and does not expect or validate `X-Skill-Service-Key`.

```rust
// child call construction
let child_req = copy_forwarded_child_headers(&chain.headers, child_req); // correlation headers
let child_req = if let Some(key) = &config.child_service_key {           // conditional service auth
    child_req.header("X-Skill-Service-Key", key)
} else {
    child_req
};
```

| Caller | Target | `Authorization` | `X-Skill-Service-Key` |
|---|---|---|---|
| FuncGateway | SkillGateway (root call) | original caller token | FuncGateway's configured key |
| SkillGateway | FuncGateway (child, InferX, `child_service_key` unset) | original caller token | not sent |
| SkillGateway | SkillGateway (child, OSS, `child_service_key` set) | original caller token | SkillGateway's own key |

#### Model turn — header policy depends on endpoint trust level

`ModelEndpoint` carries a `trust` field that controls which inbound headers are forwarded to the model backend.

```rust
enum ModelEndpointTrust {
    Internal,   // InferX FuncGateway — trusted, forward correlation headers
    External,   // third-party provider — untrusted, send only ModelEndpoint.headers
}

struct ModelEndpoint {
    url: String,
    headers: HeaderMap,
    trust: ModelEndpointTrust,
}
```

**`Internal` endpoints** (FuncGateway for InferX-backed skills): merge inbound headers with `ModelEndpoint.headers`. Correlation headers (`X-Request-Id`, `traceparent`) are forwarded for distributed tracing and routing. `ModelEndpoint.headers` wins on any collision, ensuring its `Authorization` replaces the caller's token.

```rust
// Internal model turn
let mut model_headers = chain.headers.clone();          // 1. start from inbound
model_headers.remove("Authorization");                  // 2. strip caller auth
model_headers.remove(SKILL_CHAIN_CHILD_HEADER);         // 3. strip skill-chain internals
model_headers.remove(SKILL_CHAIN_DEPTH_HEADER);
model_headers.remove(SKILL_TRACE_HEADER);
model_headers.remove("X-Skill-Trace-Content");
model_headers.remove("X-Skill-Service-Key");            // 4. strip service auth
for (k, v) in model_endpoint.headers.iter() {           // 5. overlay ModelEndpoint.headers
    model_headers.insert(k, v.clone());
}
model_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
```

**`External` endpoints** (OpenRouter, OpenAI, etc.): use only `ModelEndpoint.headers` and `Content-Type`. No inbound headers are forwarded. This prevents leaking `X-Request-Id`, internal trace IDs, or any other InferX metadata to third-party providers.

```rust
// External model turn
let mut model_headers = model_endpoint.headers.clone(); // 1. only ModelEndpoint.headers
model_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
// nothing from chain.headers
```

FuncGateway sets `trust: Internal` for InferX-backed skills and `trust: External` for external-backend skills when constructing `ModelEndpoint`.

### SkillGateway HTTP API (New Service Boundary)

`handle_skill_call_chain` and `handle_skill_debug_call` currently take `HttpGateway` and `FuncRouteTarget` as parameters. These must become proper HTTP route handlers in SkillGateway's own axum router, typed on SkillGateway's state — not on InferX types.

SkillGateway exposes:

```
POST /skills/{owner}/{namespace}/{name}/v1/chat/completions
     ← standard skill invocation, called by FuncGateway or directly by open-source clients

POST /internal/skills/debug/{owner}/{namespace}/{name}
     ← debug invocation, only reachable on the internal network
```

The request body is the existing OpenAI-format JSON. How SkillGateway obtains the skill's `system_prompt` and `ModelEndpoint` depends on the deployment:

- **InferX**: FuncGateway resolves both per-request (system prompt from disk/DB, `ModelEndpoint` from func route or external backend config with secrets resolved) and passes them to SkillGateway as part of the forwarded request. SkillGateway has no skill store in this path.
- **OSS**: SkillGateway looks up its built-in skill store by `owner/namespace/name` to get `{ system_prompt, ModelEndpoint }`. Users register skills in the store before invoking them.

This matches the existing behavior: `SkillCall` in `http_gateway.rs` already resolves `prefix` (system prompt) and `FuncRouteTarget` on every request before calling `handle_skill_call_chain`. The split continues that pattern — FuncGateway prepares and passes, SkillGateway executes.

### RunSkillDebug Access Control

Admin-only enforcement (`token.IsNamespaceAdmin()`) stays in FuncGateway before it forwards to `/internal/skills/debug/...`. SkillGateway's debug endpoint is bound to an internal interface only (not exposed on the public port), so no additional admin check is needed inside SkillGateway — it trusts the internal caller.

### Tenant Concept in SkillGateway

SkillGateway has a **lightweight** tenant concept:

- `skillep_id = owner_tenant/namespace/skillname` — the three-part addressing scheme is intrinsic to `call_skillep` routing and cannot be removed.
- `owner_tenant` is a namespace label for logging and routing only. SkillGateway does not enforce billing, quota, or ownership rules against it.
- Complex tenant logic (calling tenant vs owner tenant, GPU billing target, quota enforcement) lives entirely in FuncGateway and never crosses the boundary.

### Skill Definition

The skill definition gains a `backend` field:

```json
// InferX-hosted (default)
{ "backend": "inferx" }

// External provider
{
  "backend": "external",
  "endpoint": "https://openrouter.ai/api/v1/chat/completions",
  "api_key_ref": "secret:my-openrouter-key"
}
```

FuncGateway resolves `api_key_ref` from the tenant secret store and constructs `ModelEndpoint.headers` before forwarding to SkillGateway. SkillGateway receives a fully-resolved `ModelEndpoint` and has no knowledge of `api_key_ref` or the secret store.

## Component Ownership After Split

| Component | Owner | Description |
|---|---|---|
| `skill_chain.rs` HTTP handlers + loop | Open source (SkillGateway) | Agentic loop, tool handling, trace |
| Skill store (OSS only) | Open source (SkillGateway) | System prompt + ModelEndpoint per skill; not used in InferX path |
| `X-Skill-Service-Key` auth | Open source (SkillGateway) | Inbound request validation |
| `http_gateway.rs` / `func_agent_mgr.rs` | InferX (FuncGateway) | Pod routing, Keycloak auth, quota |
| Skill DB (InferX side) | InferX (FuncGateway) | Skill metadata, secret resolution, billing |
| `ModelEndpoint` construction | InferX (FuncGateway) | Resolves func route or external endpoint |
| Per-child access control | InferX (FuncGateway) | Re-runs on every `call_skillep` because `skills_base_url` → FuncGateway |
| `RunSkillDebug` admin check | InferX (FuncGateway) | `token.IsNamespaceAdmin()` before forwarding to SkillGateway internal endpoint |

## Migration Steps

1. Define `ModelEndpoint { url, headers, trust }` and `skills_base_url` as SkillGateway config parameters.
2. Remove `HttpGateway`, `FuncRouteTarget`, `dispatch_func_call`, `GATEWAY_CONFIG` from `skill_chain.rs`.
3. Model turn: apply header policy based on `ModelEndpoint.trust` — merge with inbound for `Internal`, use only `ModelEndpoint.headers` for `External`. Always strip `Authorization`, skill-chain internals, and `X-Skill-Service-Key` from the inbound side.
4. Child skill URL: use `format!("{}/skills/{}/{}/{}/v1/chat/completions", skills_base_url, ...)`. After `copy_forwarded_child_headers`, inject `X-Skill-Service-Key` only if `child_service_key` is set in config.
5. Replace `handle_skill_call_chain` / `handle_skill_debug_call` with axum route handlers typed on SkillGateway's own state (no `HttpGateway` or `FuncRouteTarget` parameters).
6. Add `X-Skill-Service-Key` middleware to SkillGateway's public router. All callers (FuncGateway and direct OSS clients) must present it. Bind the debug endpoint to an internal-only interface.
7. In FuncGateway's `SkillCall` handler: construct `ModelEndpoint` with `trust: Internal` for InferX-backed skills or `trust: External` for external-backend skills. Set `skills_base_url` = FuncGateway base URL. Send `X-Skill-Service-Key` + original caller's `Authorization` to SkillGateway.
8. Deploy SkillGateway as a separate service in InferX staging and validate end-to-end before removing the in-process path.

## Open Questions

- **API key storage**: For external-backend skills, where do provider API keys live in InferX? (existing tenant secret store, or a new provider-credentials table?)
- **Model name passthrough**: For external backends the `model` field in the request body must name the external model (e.g. `openai/gpt-4o`). Enforce at skill definition time or pass through as-is?
- **Billing**: External model usage is not metered by InferX. Is this the tenant's own cost, or do we need usage tracking?
- **Per-request config wire format**: How does FuncGateway pass `system_prompt` and `ModelEndpoint` to SkillGateway per-request — as request body extensions, custom headers, or a pre-call registration endpoint? Needs to be defined before implementation.
