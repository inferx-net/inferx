# Agent Model Selection Design

## Goal

Let the user choose which model (the "agent model" / main model) powers the
agent from a dropdown of **published endpoints**, instead of the model being a
single hardcoded value in the gateway service config. The selected endpoint's
context length must follow the choice so the compaction thresholds are computed
against the correct context window.

## Current state

The agent model is a single global value baked into the deployment config:

- Endpoint resolved by `agent_model_endpoint(tenant)`
  ([`session.rs:515`](../ixshare/src/gateway/session.rs#L515)), sourced from
  `AGENT_MODEL_ENDPOINT` env -> `NODE_CONFIG.agent_model_endpoint`
  ([`node_config.rs:1099`](../ixshare/src/node_config.rs#L1099)) -> loopback
  default. The resolver already accepts a `namespace/modelname` slug and builds
  the funccall URL, so pointing at an arbitrary endpoint is already supported in
  the plumbing — it is just sourced from config today.
- Context length resolved by `agent_model_context_length()`
  ([`session.rs:692`](../ixshare/src/gateway/session.rs#L692)), sourced from
  `AGENT_MODEL_CONTEXT_LENGTH` env -> `NODE_CONFIG.agent_model_context_length`.
  This single value feeds all three compaction thresholds:
  - post-turn / reactive — `postturn_compaction_threshold()`
    ([`session.rs:712`](../ixshare/src/gateway/session.rs#L712))
  - anchored synthesis preflight — `agent_preflight_compaction_ratio()`
    ([`session.rs:724`](../ixshare/src/gateway/session.rs#L724))
  - whole-request first-call preflight — `agent_first_call_compaction_ratio()`
    ([`session.rs:733`](../ixshare/src/gateway/session.rs#L733))

Problems: changing the agent model requires editing config + a gateway restart,
and every tenant/user on the deployment is forced onto the same model and the
same context window.

## Decisions

1. **User selects from published endpoints only.** Not arbitrary deployed
   funcs. This makes the context length reliably available server-side.
2. **The user never sends or edits the context length.** The client sends only
   the endpoint **slug**. The gateway looks up the context length itself.
3. **Selection is bound to the session** (per-session, the selected **slug** is
   stored on `Session`), not to global config and not to a server-side
   per-user/per-tenant settings store. This is the lightweight first step. The
   selected slug is persisted client-side (`localStorage`) so the dashboard can
   re-send it when creating a session.
4. **Only the slug is pinned to the session; context length is looked up live**
   at model-call time, keyed by the slug, so a republish between turns is picked
   up on the next prompt. Context length is **not** cached on the session. The
   design does not enforce revision-level synchronization (the `func_revision`
   guard was removed); it accepts brief threshold staleness from the non-atomic
   publish — see "Where the context length lives".
5. **A model change starts a new session.** The dashboard must not silently
   resume a session bound to a different endpoint (see finding #1).
6. **Published is validated against function status, not DB row presence** (see
   finding #2).
7. **A slug is mandatory; the gateway's agent-model configs are removed.** The
   dashboard owns the default model and always sends a slug, so `CreateSession`
   rejects a slug-less request and `AGENT_MODEL_ENDPOINT` /
   `AGENT_MODEL_CONTEXT_LENGTH` (env + `NODE_CONFIG`) are deleted (see "Removing
   the gateway agent-model configs"). Trade-off: a deployment with no published
   endpoints cannot use the agent — there is no fallback default model.

## Where the context length lives

There is an `Endpoints` table keyed by `slug` with a `context_length BIGINT`
column ([`secret.sql:103-118`](../dashboard/sql/secret.sql#L103-L118)), surfaced
in Rust as `EndpointMetadata.context_length: Option<i64>`
([`secret.rs:86`](../ixshare/src/gateway/secret.rs#L86)). The row is
created/updated on publish via `UpsertEndpointMetadata` / `PublishEndpoint`
([`secret.rs:902`](../ixshare/src/gateway/secret.rs#L902),
[`secret.rs:955`](../ixshare/src/gateway/secret.rs#L955)).

### Working assumption: a published endpoint has a context length

**This design assumes `context_length` is present (non-null) for any published
endpoint, and uses it directly.** This keeps the gateway simple — a single
`GetEndpointMetadata(slug)` lookup, no revision cross-check, no
`--max-model-len` parsing.

If the value is unexpectedly NULL/missing/DB-error on the slug path, the gateway
substitutes a **conservative built-in constant** (a modest window defined in the
gateway), **not** the global `AGENT_MODEL_CONTEXT_LENGTH`. Reason: that global is
the context window of the *configured default model*, so applying it to a
*different* user-chosen endpoint would silently use the wrong window. The
built-in constant keeps compaction working (never `0`, which would disable it)
without borrowing an unrelated model's window — see "Resolution order".

This assumption is **not enforced** today, so record the risk:

- `context_length` is a prefilled-but-editable form field on publish
  ([`app.py:5490`](../dashboard/app.py#L5490)): prefilled from the existing row,
  then the catalog entry, then the func's `--max-model-len`, but the admin can
  submit it blank. The column is nullable.
- It is descriptive metadata, not derived from / validated against the running
  model.
- Consequence if the assumption is violated (published endpoint with NULL or
  wrong `context_length`): the agent computes compaction thresholds against the
  wrong window for that endpoint. This is a **soft, self-correcting** degradation
  (compaction fires early/late) — not a wrong model and not a routing error. If
  this proves to matter in practice, the right fix is to make `context_length`
  required at publish; that is out of scope here.

Note these properties of the `Endpoints` row that the design deliberately does
**not** lean on (they were the basis for an earlier, more complex revision-guard
approach that this assumption removes):

- **Row presence does not mean published.** Unpublish only flips
  `funcstatus.object.published = false`
  ([`http_gw.rs:269`](../ixshare/src/gateway/http_gw.rs#L269)); it does not delete
  the row. The authoritative published check is function status
  ([`http_gateway.rs:846-865`](../ixshare/src/gateway/http_gateway.rs#L846-L865)),
  which the routing path already performs — so we rely on routing for
  published/unpublished, not on the metadata row.
- **Publish is non-atomic** (SQL row written before the status flip,
  [`http_gw.rs:233-266`](../ixshare/src/gateway/http_gw.rs#L233-L266)), so the
  row can briefly be ahead of the routed revision. Under the working assumption
  we accept the resulting brief threshold staleness rather than guarding against
  it with a `func_revision` cross-check (which would have forced resolving a
  routed `func.Version()` on the agent path).

## Data flow

```
Dashboard agent page (active tenant T)
  - dropdown lists tenant T's PUBLISHED endpoints
    (build_endpoint_list_entries(include_unpublished=False, tenant=T))
  - selection persisted to localStorage KEYED BY TENANT, e.g.
    agentEndpoint:<T> = "<slug>" (slug only, NOT context length)
  - resolve selected slug = localStorage[agentEndpoint:<T>], and only use it if
    it exists in T's current dropdown options; otherwise treat as unset
  - on load: GET /agent/sessions/current
      - if a session exists -> ALWAYS resume it (never delete on load). If its
        agent_endpoint matches the selected slug, resume as-is; if it differs,
        resume anyway and sync the dropdown/selection to the session's bound
        model. Deleting a session on load would silently wipe a valid chat on a
        plain refresh whenever the local selection is stale/empty or another tab
        used a different model (`/sessions/current` returns the most recently
        updated session regardless of model), so load is strictly non-destructive.
      - if no session exists -> create one bound to the selected slug (nothing to
        delete, so this is a plain create, not delete-then-create)
  - on dropdown change (and on New Session button) -> start a new session
    (create-first-then-delete). Model changes take effect here, via an explicit
    user action — not by deleting on load.
  - "start a new session" = create-first-then-delete: POST a new session bound to
    the new slug FIRST; only after it succeeds, commit the selection, cancel any
    active stream, clear the transcript, and DELETE the previous session (best
    effort). If the create fails (e.g. the gateway rejects a since-unpublished
    model), the existing session/transcript and active model are left intact and
    the dropdown reverts. (see "New Session button" and "Changing the model
    mid-chat")

Gateway CreateSession
  - if agent_endpoint present: verify it is published (function status) and the
    tenant can access it
  - store ONLY the slug on the Session

Gateway model call (per prompt)
  - resolve endpoint from session.agent_endpoint:
      - routes (published)  -> use it
      - fails (unpublished/unroutable) -> ERROR the prompt; do not fall through
  - look up context_length for the slug (GetEndpointMetadata); assumed present
    for a published endpoint. Resolved once at prompt start and used to compute
    the compaction thresholds for the whole turn
```

## Resolution order

A slug is **always** present: `CreateSession` rejects a slug-less request, so by
the time a prompt runs `session.agent_endpoint` is set. There is no env/config
default path on the gateway anymore (see "Removing the gateway agent-model
configs").

Endpoint (at model-call time):

```
resolve route for session.agent_endpoint (always set):
  success  -> use that endpoint
  FAILURE  -> ERROR the prompt (do NOT fall through to any default model)
```

The slug choice is binding. If dispatch to the slug's endpoint fails — e.g. it
was unpublished or became unroutable after session creation, surfacing as
`"endpoint ... is unpublished"` via the existing dispatch-time check
([`http_gateway.rs:860`](../ixshare/src/gateway/http_gateway.rs#L860)) — the
prompt returns a clear error telling the user to pick a model / start a new
session. There is no default model to fall through to.

Context length (resolved once per turn, NOT cached on the session):

```
GetEndpointMetadata(session.agent_endpoint).context_length
  non-null -> use it                 (assumed present when published)
  NULL / missing / DB-error -> CONSERVATIVE_BUILTIN_CTX (gateway constant)
```

**Explicit rule:** use `Endpoints.context_length` directly when non-null (no
`func_revision` guard). On NULL/missing/DB-error, use a **conservative built-in
constant** baked into the gateway (never `0`, which would disable compaction).
There is no global `AGENT_MODEL_CONTEXT_LENGTH` fallback — that config is removed,
and it would in any case be the wrong model's window.

This changes current behavior: `agent_model_context_length` today falls back to
the global default on the slug path's NULL/missing/error cases
([`session.rs:771-776`](../ixshare/src/gateway/session.rs#L771-L776)); under this
rule those cases use the built-in constant, and the global-default helper
([`session.rs:747-752`](../ixshare/src/gateway/session.rs#L747-L752)) and its
no-slug branch are deleted.

Resolving the context length per turn (rather than caching it at session
creation) means a republish between turns is picked up on the next prompt. The
value is resolved once at prompt start and threaded through the turn so it
cannot change mid-turn (see "Per-turn context length").

## Default agent model (first open) — dashboard-owned

**Problem.** On first open (empty `localStorage`, no prior session) the dropdown
selects the static `<option value="">Default</option>`
([`inferx_agent.html:409`](../dashboard/templates/inferx_agent.html#L409)) and the
session is created with no slug, so the gateway falls back to
`AGENT_MODEL_ENDPOINT` -> `NODE_CONFIG.agent_model_endpoint`
([`session.rs:561-572`](../ixshare/src/gateway/session.rs#L561-L572)). Two issues:
that config defaults to **empty** ([`node_config.rs:1183`](../ixshare/src/node_config.rs#L1183)),
so many deployments have no real default (loopback), and the dashboard cannot
**show** which model is answering — the label is the literal word "Default".

**Decision: move the default agent model from the gateway service config to the
dashboard, as a global env value (option 1).** The dashboard owns the default,
so it inherently knows it and can both preselect and display it on first open.

- Add a dashboard env constant, e.g.
  `DEFAULT_AGENT_MODEL = os.getenv("DEFAULT_AGENT_MODEL", "").strip()`, alongside
  the other module-level config near
  [`app.py:188-201`](../dashboard/app.py#L188-L201) (mirrors `METADATA_GEN_MODEL`).
  Its value is a published-endpoint **slug**.
- Pass it to the agent page as template context (e.g. `default_agent_model`)
  next to `endpoints` / `active_tenant` in `InferXAgent`
  ([`app.py:8218-8223`](../dashboard/app.py#L8218-L8223)).
- Frontend first-open selection becomes: stored slug (validated) -> else
  `DEFAULT_AGENT_MODEL` (if it is in the published list) -> else first published
  endpoint (if any) -> else the empty "Default" option. The chosen slug is
  preselected in the dropdown (showing its real `model_name`) and bound to the
  session at create time, so first open runs on a **named** model, not the opaque
  gateway default.

**Effect on the gateway: the slug becomes mandatory, and both agent-model
configs are removed.** Since the dashboard now always supplies a slug, the agent
path is always slug-driven and the gateway's fallback configs become dead code.
They are removed entirely (see "Removing the gateway agent-model configs"):
`AGENT_MODEL_ENDPOINT`, `AGENT_MODEL_CONTEXT_LENGTH`, and the corresponding
`NODE_CONFIG` fields. `CreateSession` rejects a request with no `agent_endpoint`.

**Empty-list edge case.** If the tenant has no published endpoints and
`DEFAULT_AGENT_MODEL` is unset/unpublished, there is **no model to use** — the
agent cannot start a session. The dashboard must block this case (disabled input
+ a clear "publish or select a model first" message) rather than attempting a
slug-less `POST /agent/sessions`, which the gateway now rejects.

## Removing the gateway agent-model configs

With the default relocated to the dashboard and a slug always supplied, both
gateway config values lose their only readers and are deleted:

- **`AGENT_MODEL_ENDPOINT` env + `NODE_CONFIG.agent_model_endpoint`**
  ([`node_config.rs:1099`](../ixshare/src/node_config.rs#L1099)) — only read by
  the slug-less branch of `agent_model_endpoint`
  ([`session.rs:561-572`](../ixshare/src/gateway/session.rs#L561-L572)). Delete
  that branch; `agent_model_endpoint` becomes "build the funccall URL from the
  (required) slug" — no env, no config, no loopback fallback.
- **`AGENT_MODEL_CONTEXT_LENGTH` env + `NODE_CONFIG.agent_model_context_length`**
  ([`node_config.rs:1105`](../ixshare/src/node_config.rs#L1105)) — only read by
  `agent_model_context_length_default()`
  ([`session.rs:747-752`](../ixshare/src/gateway/session.rs#L747-L752)). Delete
  that function; the slug path uses `Endpoints.context_length` or the conservative
  built-in constant (it never needed the global anyway). No no-slug branch
  remains.

**Mandatory slug at `CreateSession`** ([`session.rs:2936-2961`](../ixshare/src/gateway/session.rs#L2936-L2961)):
the `_ => None` arm becomes a `400` rejection (e.g.
`{"error":"no_agent_model","message":"select a model"}`). With no slug-less
session possible, `agent_model_endpoint`/`agent_model_context_length` always
receive a slug, so deleting the fallbacks is safe.

**Cost (accepted):** a deployment with no published endpoints can no longer use
the agent — there is no default model to fall back to. This is intentional: the
agent requires a model, and the dashboard guides the user to publish/select one.

## Gateway changes

1. **New reader** in [`secret.rs`](../ixshare/src/gateway/secret.rs), beside
   `UpsertEndpointMetadata`:
   `GetEndpointMetadata(slug) -> Option<EndpointMetadata>`
   (`SELECT ... FROM Endpoints WHERE slug = $1`). Used at prompt start to read
   `context_length`. **This is not the published-endpoint validator** — published
   state comes from the routing path (see #4).

2. **`CreateSessionRequest`**
   ([`session.rs:338`](../ixshare/src/gateway/session.rs#L338)): add
   `agent_endpoint: Option<String>` (slug). No context-length field — the client
   never sends it.

3. **`Session`** ([`session.rs:31`](../ixshare/src/gateway/session.rs#L31)): add
   `agent_endpoint: Option<String>`. Unlike `skill_api_key`, this field is
   **serialized to the client** (no `serde(skip)`) so the dashboard can compare
   the resumed session's endpoint against the selected slug (finding #1). Do
   **not** store `agent_context_length` on the session — it is looked up live.

4. **`CreateSession` handler**
   ([`session.rs:2763`](../ixshare/src/gateway/session.rs#L2763)): when
   `agent_endpoint` is present, validate it using the **same published check used
   at routing** — verify function status `published == true` (reuse the
   `resolve_funccall_target` path / status lookup at
   [`http_gateway.rs:846-865`](../ixshare/src/gateway/http_gateway.rs#L846-L865)),
   not `Endpoints` row presence. Confirm the slug belongs to / is accessible by
   the calling tenant. Reject unpublished or inaccessible slugs here so failures
   surface at session creation, not on the first model call (finding #2). Store
   the slug on the session.

5. **Resolution functions:**
   - `agent_model_endpoint(tenant, session.agent_endpoint)` — session slug first,
     else the existing env -> config -> loopback chain. Callers at
     [`session.rs:1493`](../ixshare/src/gateway/session.rs#L1493) and
     [`session.rs:1810`](../ixshare/src/gateway/session.rs#L1810). When a slug is
     set but no longer routes (unpublished/unroutable), the prompt errors — it
     does not fall through to the env/config default (see #6 and the endpoint
     resolution-order note).
   - `agent_model_context_length(session.agent_endpoint)` — if a slug is set,
     return `GetEndpointMetadata(slug).context_length` (assumed present for a
     published endpoint); if NULL/missing/DB-error, use the **conservative
     built-in constant** (not the global `AGENT_MODEL_CONTEXT_LENGTH`, which is a
     different model's window). If no slug is set, the global env -> config.
     Change current code at
     [`session.rs:771-776`](../ixshare/src/gateway/session.rs#L771-L776) so the
     slug-path fallbacks use the constant; the global default stays only on the
     no-slug branch ([`session.rs:782`](../ixshare/src/gateway/session.rs#L782)).
     **Resolved exactly once per turn** (see #6), not on every threshold check. No
     `func_revision` cross-check — see the working assumption.

6. **Per-turn context length and endpoint failure (one lookup, threaded through
   the turn).**

   The agent path resolves the endpoint as a URL string via
   `agent_model_endpoint()`
   ([`session.rs:515`](../ixshare/src/gateway/session.rs#L515),
   [`session.rs:1810`](../ixshare/src/gateway/session.rs#L1810)) and resolves
   `ctx` separately at prompt start
   ([`session.rs:1857`](../ixshare/src/gateway/session.rs#L1857)). Two changes:

   **(a) Surface endpoint failure as a prompt error.** A session-bound slug can
   become unpublished/unroutable after session creation. The prompt path must
   detect this — the dispatch to the resolved endpoint already fails for an
   unpublished endpoint (`"endpoint ... is unpublished"`,
   [`http_gateway.rs:860`](../ixshare/src/gateway/http_gateway.rs#L860)) — and
   return a **clear error** instructing the user to pick a model / start a new
   session. It must **not** fall through to `AGENT_MODEL_ENDPOINT` or any default,
   which would route the conversation to a different model. (No `func.Version()`
   resolution is required for this; the published check happens at dispatch as it
   does today.)

   **(b) Resolve `ctx` once and thread it through the turn.**
   `HandlePromptStream` already resolves `let ctx = ...` at prompt start
   ([`session.rs:1857`](../ixshare/src/gateway/session.rs#L1857)) and uses it for
   the first-call preflight. Make that the single
   `agent_model_context_length(slug)` lookup, then **thread the same `ctx` value
   through the whole turn** so one prompt does at most one metadata read and the
   window cannot change mid-turn:
   - first-call preflight — already uses `ctx`
     ([`session.rs:1896`](../ixshare/src/gateway/session.rs#L1896)).
   - synthesis preflight — currently re-derives the ratio at
     [`session.rs:2332`](../ixshare/src/gateway/session.rs#L2332); pass the
     turn's `ctx` in instead of recomputing.
   - post-turn compaction — `postturn_compaction_threshold()`
     ([`session.rs:712`](../ixshare/src/gateway/session.rs#L712)) is called from
     several response paths in one turn
     ([`session.rs:1681`](../ixshare/src/gateway/session.rs#L1681),
     [`session.rs:2472`](../ixshare/src/gateway/session.rs#L2472),
     [`session.rs:2708`](../ixshare/src/gateway/session.rs#L2708)) and internally
     re-reads `agent_model_context_length()`. Change it to take `ctx` as a
     parameter: `postturn_compaction_threshold(ctx)`, fed the turn's value.

   `HandlePrompt` (non-stream, [`session.rs:1478`](../ixshare/src/gateway/session.rs#L1478))
   resolves `ctx` once the same way and passes it to its post-turn check at
   [`session.rs:1681`](../ixshare/src/gateway/session.rs#L1681).

## Dashboard changes

- Agent page ([`inferx_agent.html`](../dashboard/templates/inferx_agent.html),
  served by [`InferXAgent`](../dashboard/app.py#L8200)): add an endpoint dropdown
  populated from the **active tenant's** published endpoints
  ([`app.py:5728`](../dashboard/app.py#L5728)); persist the selected **slug** to
  `localStorage` **keyed by tenant** (e.g. `agentEndpoint:<tenant>`).
- **Tenant scoping (finding #1 of the second review).** Session creation is
  bound to the current active tenant
  ([`AgentCreateSession`, app.py:8211](../dashboard/app.py#L8211)) and the
  endpoint set is tenant-dependent. The saved selection must therefore be
  per-tenant, and before sending `agent_endpoint` the dashboard must confirm the
  resolved slug is present in the current tenant's dropdown options. A stale slug
  from another tenant is treated as unset (fall back to no selection / global
  default) rather than sent — this avoids a `POST /agent/sessions` failure after
  a tenant switch.
- **Session lifecycle (`initSession`,
  [`inferx_agent.html:644`](../dashboard/templates/inferx_agent.html#L644)):**
  - On load, **always resume** the `current` session — load is non-destructive.
    When its `agent_endpoint` matches the `localStorage` slug, resume as-is; when
    it differs, resume anyway and sync the dropdown/selection to the session's
    bound model (`syncModelSelect`). Because `/agent/sessions/current`
    ([`session.rs:2733`](../ixshare/src/gateway/session.rs#L2733)) returns the
    most recently updated session regardless of model, deleting on a mismatch
    would silently wipe a valid chat on a plain refresh (stale `localStorage`,
    empty endpoint list, or another tab using a different model). Model changes
    therefore take effect only through an explicit dropdown change / New Session,
    not by deleting on load (refines finding #1).
  - Changing the dropdown starts a new session (a "new chat"), since model choice
    is per-session and existing transcript/compaction state belongs to the prior
    model. Surface this to the user (e.g. clears the chat).
- Send the slug as `agent_endpoint` in
  [`AgentCreateSession`](../dashboard/app.py#L8209).
- No context-length UI.

## Changing the model mid-chat

The model is bound to the session, and the session **is** the conversation state
(`messages` + the compacted `model_messages` working context + compaction
summaries), all built for one model's context window
([`session.rs:31-46`](../ixshare/src/gateway/session.rs#L31-L46)). There is no
in-place model swap.

**Decision: changing the model starts a new session (fresh chat).** When the
user picks a different model in the dropdown, the dashboard creates a new session
bound to the new slug and clears the chat; the prior conversation is not carried
into it. Rationale:

- The previous session's `model_messages`/compaction state was computed against
  the previous model's window; reusing it under a different model would be
  incoherent. A fresh session guarantees compaction is always computed against
  the active model's window.
- It needs no per-prompt model field and no transcript-replay/migration logic.

The prior session is replaced via the same create-first-then-delete sequence as
the New Session button (see "New Session button"): a new session bound to the new
slug is created first; only on success is the active stream cancelled, the
transcript cleared, and the old session `DELETE`d (best effort). If the create
fails, the old session and model are left intact. Surface the reset to the user
(e.g. a confirm or a visible "new chat") so the transcript change is not
surprising.

(Per-prompt model selection was considered and rejected: it would apply one
model's compaction state to another mid-session, so it does not actually avoid
the new-session/rebuild work — it only hides it and adds risk.)

## New Session button (explicit drop)

The agent page gets a **New Session** button so the user can deliberately drop
the current session and start a fresh one (also the path the model-change flow
above reuses). It performs a **real drop**, not just a client-side switch.

There is no client-facing delete today: the routes are `POST /sessions`,
`GET /sessions/current`, `GET /sessions/:id`, `.../messages`, `.../prompt`,
`.../prompt_stream`, `.../interrupt`
([`http_gateway.rs:1080-1093`](../ixshare/src/gateway/http_gateway.rs#L1080-L1093)).
Session removal exists only via timeout cleanup. So:

1. **Gateway — `SessionStore::DeleteSession(id)`**: mirror the timeout cleanup
   trio used in `CheckAndDeleteIfTimedOut`
   ([`session.rs:281-283`](../ixshare/src/gateway/session.rs#L281-L283)) —
   `sessions.remove(id)` + `remove_session_lock(id)` +
   `clear_active_stream_cancel(id)` (the last cancels any in-flight stream on the
   session). Scope it to a single id.

2. **Gateway — `DELETE /sessions/:id` handler**: resolve the caller tenant from
   the access token, confirm the session belongs to that tenant (reuse
   `caller_tenant` + an ownership check as in `GetSession`), then
   `DeleteSession`. Return 204/200 on success, 404 if not found / not owned.

3. **Dashboard — proxy + button**: a `DELETE /agent/sessions/<id>` proxy route
   (same shape as the other agent proxies in
   [`app.py:8251-8278`](../dashboard/app.py#L8251-L8278)) and a **New Session**
   button that follows the create-first-then-delete order: `POST /agent/sessions`
   with the selected slug FIRST; on success, commit the selection, cancel any
   active stream, switch the view to the new id, clear the chat, and `DELETE` the
   previous session (best effort). On create failure, keep the current session and
   model. A monotonic token guards overlapping clicks so only the latest commits.

The model-change flow ("Changing the model mid-chat") uses this same
create-first-then-delete sequence, so the prior session is replaced rather than
left to linger until timeout — without risking the active chat on a create
failure.

## Compatibility note (breaking)

This is **not** backward-compatible: a slug is now mandatory and the gateway's
fallback configs are gone. A client that sends no `agent_endpoint` gets a `400`
from `CreateSession` instead of silently running on the gateway default. The only
client is the dashboard, which always supplies a slug (or blocks when none is
available), so this is acceptable — but any other caller of the session API must
send a slug. Deployments that relied on `AGENT_MODEL_ENDPOINT` /
`AGENT_MODEL_CONTEXT_LENGTH` must instead publish an endpoint and set
`DEFAULT_AGENT_MODEL` in the dashboard.

## Implementation order

1. **Gateway** (testable on its own; note the breaking slug requirement lands
   here): new `GetEndpointMetadata` reader, `agent_endpoint` on
   `CreateSessionRequest` and `Session` (serialized), `CreateSession`
   published-status validation **and slug-mandatory `400`**,
   `agent_model_endpoint(tenant, slug)` with the slug-less branch removed,
   prompt-error handling when a bound slug no longer routes, per-turn `ctx`
   resolution (`agent_model_context_length(slug)` with built-in-constant
   fallback) threaded through first-call/synthesis preflight and
   `postturn_compaction_threshold(ctx)`, and removal of the
   `AGENT_MODEL_ENDPOINT` / `AGENT_MODEL_CONTEXT_LENGTH` env + `NODE_CONFIG`.
2. **Gateway — session drop**: `SessionStore::DeleteSession(id)` +
   `DELETE /sessions/:id` handler with tenant-ownership check.
3. **Dashboard**: endpoint dropdown + tenant-keyed `localStorage` with
   stale-slug validation + `DELETE /agent/sessions/<id>` proxy + **New Session**
   button + new-session (create-first-then-delete) on explicit model change /
   New Session (load on mismatch is non-destructive — resume + sync the selector)
   + send `agent_endpoint` on session creation.

## Review findings addressed

### First review

- **#1 — model change never takes effect on resume.** `Session.agent_endpoint`
  is serialized to the client. A model change takes effect through the explicit
  dropdown-change / New Session flow (create-first-then-delete bound to the new
  slug), not via reload. On load the dashboard **always resumes** the `current` session
  (non-destructive); on an endpoint mismatch it syncs the dropdown to the
  session's bound model rather than deleting the chat — deleting on load would
  wipe a valid conversation on a plain refresh (stale `localStorage`, empty
  endpoint list, or another tab). (No reliance on session timeout to switch
  models.)
- **#2 — row presence is not a published check.** `CreateSession` validates
  against function status `published == true` (the same authoritative check used
  at routing), not `Endpoints` row existence. Unpublished/inaccessible slugs are
  rejected at session creation.
- **#3 — cached context length goes stale on republish.** Context length is not
  stored on the session; it is resolved per turn via `GetEndpointMetadata(slug)`,
  so routing and compaction thresholds stay in sync with the currently published
  endpoint.

### Second review

- **#1 — persisted selection must be tenant-scoped.** The slug is stored in
  `localStorage` keyed by tenant, and validated against the current tenant's
  dropdown options before being sent. A stale slug after a tenant switch is
  treated as unset rather than sent, so it cannot fail `POST /agent/sessions`.
- **#2 — live context-length lookup must be once per turn.** The context length
  is resolved exactly once at prompt start (`ctx`) and threaded through
  first-call preflight, synthesis preflight, and post-turn compaction —
  `postturn_compaction_threshold` takes `ctx` as a parameter instead of
  re-reading it. One metadata read per prompt; no mid-turn window change.

### Third & fourth reviews (superseded)

Those reviews drove a `func_revision`-guarded context-length lookup (validate the
metadata row against the routed `func.Version()` because publish is non-atomic),
which in turn required adding a prompt-start route-resolution step to the agent
path. **Both were removed by the working assumption** (see "Where the context
length lives"): a published endpoint is assumed to have a `context_length`, used
directly, so there is no revision guard and no route-resolution step. The
underlying non-atomic-publish fact is now handled by accepting brief threshold
staleness plus the NULL-to-conservative-built-in-constant safety net.

### Fifth review

- **#1 — prompt-time endpoint failure must error, not fall through.** When a
  session-bound slug becomes unpublished/unroutable after session creation, the
  dispatch to its endpoint fails (the existing dispatch-time published check,
  `"endpoint ... is unpublished"`). The endpoint resolution order now states this
  explicitly: a bound slug whose dispatch fails **errors the prompt** and asks the
  user to pick a model / start a new session — it does **not** fall through to
  `AGENT_MODEL_ENDPOINT` or any default, which would route the conversation to a
  different model. The env/config/loopback chain applies only when the session
  has no selected slug. (Context length, by contrast, uses a conservative
  built-in constant on the slug path when metadata is missing — not the global
  default, which is a different model's window.) Note: this relies on the **existing
  dispatch-time** check; no dedicated pre-dispatch route-resolution step is
  required (the revision-guard approach that needed one was dropped — see the
  superseded note above).

## Implementation checklist

### Gateway — endpoint metadata reader

- [x] Add `GetEndpointMetadata(slug) -> Result<Option<EndpointMetadata>>` in
      [`secret.rs`](../ixshare/src/gateway/secret.rs) beside `UpsertEndpointMetadata`
      ([`secret.rs:902`](../ixshare/src/gateway/secret.rs#L902)):
      `SELECT ... FROM Endpoints WHERE slug = $1`, mapped to `EndpointMetadata`.

### Gateway — session model binding

- [x] Add `agent_endpoint: Option<String>` to `CreateSessionRequest`
      ([`session.rs:337-340`](../ixshare/src/gateway/session.rs#L337-L340)),
      `#[serde(default)]`. No context-length field.
- [x] Add `pub agent_endpoint: Option<String>` to `Session`
      ([`session.rs:31-46`](../ixshare/src/gateway/session.rs#L31-L46)).
      **Not** `#[serde(skip)]` — must serialize to the client for resume
      comparison.
- [x] Thread it through `SessionStore::CreateSession`
      ([`session.rs:105`](../ixshare/src/gateway/session.rs#L105)) — add an
      `agent_endpoint` parameter, set it on the constructed `Session`.
- [x] In the `CreateSession` handler
      ([`session.rs:2763`](../ixshare/src/gateway/session.rs#L2763)): when
      `agent_endpoint` is present, validate it is published + tenant-accessible
      using the same status check as routing
      ([`http_gateway.rs:846-865`](../ixshare/src/gateway/http_gateway.rs#L846-L865)),
      reject otherwise; pass the slug into `CreateSession`.

### Gateway — endpoint + context-length resolution

- [x] Change `agent_model_endpoint(tenant)` -> `agent_model_endpoint(tenant, slug: Option<&str>)`
      ([`session.rs:515`](../ixshare/src/gateway/session.rs#L515)): use the slug
      when set, else the existing env -> config -> loopback chain. Update callers
      at [`session.rs:1493`](../ixshare/src/gateway/session.rs#L1493) and
      [`session.rs:1810`](../ixshare/src/gateway/session.rs#L1810) to pass
      `session.agent_endpoint`.
- [x] Change `agent_model_context_length()` -> `agent_model_context_length(slug: Option<&str>)`
      ([`session.rs:761`](../ixshare/src/gateway/session.rs#L761)): slug set ->
      `GetEndpointMetadata(slug).context_length`. Never return 0 from the slug
      path. (The no-slug branch this item added is removed by "mandatory slug,
      remove agent-model configs" below.)
- [x] **Slug-path fallback -> built-in constant.** On the slug path,
      NULL/missing/DB-error currently falls back to the global default at
      [`session.rs:771-776`](../ixshare/src/gateway/session.rs#L771-L776). Change
      those cases to a **conservative built-in constant**
      (e.g. `const AGENT_FALLBACK_CTX: u64 = 8192;`). (The no-slug branch is
      deleted entirely below.)
- [x] Change `postturn_compaction_threshold()` -> `postturn_compaction_threshold(ctx: u64)`
      ([`session.rs:712`](../ixshare/src/gateway/session.rs#L712)); pass the
      turn's `ctx` at all call sites
      ([`session.rs:1681`](../ixshare/src/gateway/session.rs#L1681),
      [`session.rs:2472`](../ixshare/src/gateway/session.rs#L2472),
      [`session.rs:2708`](../ixshare/src/gateway/session.rs#L2708)).
- [x] In `HandlePromptStream` resolve `ctx` once via the slug-aware lookup
      ([`session.rs:1857`](../ixshare/src/gateway/session.rs#L1857)) and thread it
      through first-call preflight (already uses `ctx`,
      [`session.rs:1896`](../ixshare/src/gateway/session.rs#L1896)) and synthesis
      preflight ([`session.rs:2332`](../ixshare/src/gateway/session.rs#L2332)).
- [x] In `HandlePrompt` (non-stream,
      [`session.rs:1478`](../ixshare/src/gateway/session.rs#L1478)) resolve `ctx`
      once the same way and pass it to its post-turn check.
- [x] Map bound-slug dispatch failures to a clear, actionable error in **both**
      prompt paths — today they wrap any backend failure (e.g.
      `service failure: endpoint not found` from the funccall route,
      [`http_gateway.rs:490`](../ixshare/src/gateway/http_gateway.rs#L490), or
      `"endpoint ... is unpublished"`) generically as `Model error: ...`:
      - non-stream `HandlePrompt`: the `PromptResponse.content` mapping at
        [`session.rs:1573-1579`](../ixshare/src/gateway/session.rs#L1573-L1579).
      - stream `HandlePromptStream`: the `model_error` JSON mapping at
        [`session.rs:1968-1974`](../ixshare/src/gateway/session.rs#L1968-L1974).
      Detect the unpublished/not-found-endpoint case and replace the message with
      a "this model is no longer available — pick a model / start a new session"
      text (do not fall through to a default endpoint). A shared helper that
      classifies the dispatch error keeps the two paths consistent.

### Gateway — mandatory slug, remove agent-model configs

- [x] `CreateSession` ([`session.rs:2936-2961`](../ixshare/src/gateway/session.rs#L2936-L2961)):
      change the `_ => None` arm to reject a slug-less request with `400`
      (`{"error":"no_agent_model","message":"select a model"}`). After this,
      `Session.agent_endpoint` is always `Some`.
- [x] Delete the slug-less fallback branch in `agent_model_endpoint`
      ([`session.rs:561-572`](../ixshare/src/gateway/session.rs#L561-L572)); the
      function builds the funccall URL from the (now-required) slug only.
- [x] Delete `agent_model_context_length_default()`
      ([`session.rs:747-752`](../ixshare/src/gateway/session.rs#L747-L752)) and the
      no-slug branch of `agent_model_context_length`
      ([`session.rs:782`](../ixshare/src/gateway/session.rs#L782)).
- [x] Remove `agent_model_endpoint` and `agent_model_context_length` fields from
      `NODE_CONFIG` ([`node_config.rs:1099`](../ixshare/src/node_config.rs#L1099),
      [`node_config.rs:1105`](../ixshare/src/node_config.rs#L1105)) and their
      defaults ([`node_config.rs:1183-1184`](../ixshare/src/node_config.rs#L1183-L1184)),
      and drop any `AGENT_MODEL_ENDPOINT` / `AGENT_MODEL_CONTEXT_LENGTH` references
      in deployment config/compose. (`agent_*_compaction_ratio` configs stay.)

### Gateway — session drop

- [x] Add `SessionStore::DeleteSession(id)` mirroring the timeout-cleanup trio
      (`sessions.remove` + `remove_session_lock` + `clear_active_stream_cancel`,
      [`session.rs:281-283`](../ixshare/src/gateway/session.rs#L281-L283)).
- [x] Add `DELETE /sessions/:id` route + handler
      ([`http_gateway.rs:1080-1093`](../ixshare/src/gateway/http_gateway.rs#L1080-L1093)):
      tenant-ownership check (as in `GetSession`), then `DeleteSession`; 200/204
      on success, 404 if not found / not owned.

### Dashboard — Python (Flask)

- [x] Supply the published endpoint list to the agent page. The `InferXAgent`
      route currently passes only `back_href`
      ([`app.py:8200-8206`](../dashboard/app.py#L8200-L8206)) — it provides no
      endpoint data. Either:
      - pass `build_endpoint_list_entries(include_unpublished=False, tenant=T)`
        ([`app.py:5728`](../dashboard/app.py#L5728)) into the template as
        `render_template(..., endpoints=...)`, **or**
      - add a dedicated JSON route (e.g. `GET /agent/endpoints`) that returns the
        active tenant's published endpoints, which the page fetches on load.
      Prefer the JSON route if the dropdown must refresh without a full page
      reload; otherwise template context is simpler.
- [x] Expose the **active tenant name** to the page so the frontend can build the
      `agentEndpoint:<tenant>` `localStorage` key consistently. `InferXAgent`
      passes only `back_href` today
      ([`app.py:8200-8206`](../dashboard/app.py#L8200-L8206)); resolve it with
      `resolve_active_tenant_name(listroles())` (as `AgentCreateSession` does,
      [`app.py:8211-8213`](../dashboard/app.py#L8211-L8213)) and pass it as
      template context (e.g. `active_tenant=...`) and/or include it in the
      `GET /agent/endpoints` JSON payload, matching whichever delivery option is
      chosen above. The key must use the same tenant identity the server binds
      sessions to, not a client guess.
- [x] Add `agent_endpoint` to `AgentCreateSession`
      ([`app.py:8209`](../dashboard/app.py#L8209)): read it from the incoming
      request JSON and include it in the gateway `POST /sessions` body alongside
      `api_key`.
- [x] Add a `DELETE /agent/sessions/<id>` proxy route, same shape as the agent
      proxies at [`app.py:8251-8278`](../dashboard/app.py#L8251-L8278).

### Dashboard — frontend (`inferx_agent.html`)

- [x] Render the endpoint dropdown from the data supplied above (template
      `endpoints` or the `GET /agent/endpoints` fetch). Load/validate it **before**
      `initSession` runs.
- [x] Persist the selected slug to `localStorage` keyed by tenant
      (`agentEndpoint:<tenant>`); validate the stored slug against the current
      tenant's dropdown options and treat a stale/foreign slug as unset.
- [x] Update `createSession()`
      ([`inferx_agent.html:581-591`](../dashboard/templates/inferx_agent.html#L581-L591)):
      it currently posts no body — change it to
      `createSession(slug)` and send `body: JSON.stringify({ agent_endpoint: slug })`
      so the slug reaches `AgentCreateSession`. (Slug-mandatory invariant: after
      the gateway `400` change, `createSession` must be called only with a
      non-empty validated slug — see "Mandatory-slug frontend invariant" below.)
- [x] `initSession` ([`inferx_agent.html:831`](../dashboard/templates/inferx_agent.html#L831)):
      **load is strictly non-destructive.** Always resume `current` if it exists;
      if its `agent_endpoint` differs from the selected slug, resume anyway and
      sync the dropdown/selection to the session's bound model (do **not** delete
      or start a new session on mismatch — that would wipe a valid chat on a plain
      refresh). Only create when no session exists. This matches the data-flow
      section and the current implementation
      ([`inferx_agent.html:846-858`](../dashboard/templates/inferx_agent.html#L846-L858)).
- [x] **Session-expiry reconnect:** `sendMessage()` recreates a session on
      `session_not_found` and currently calls bare `createSession()`
      ([`inferx_agent.html:733`](../dashboard/templates/inferx_agent.html#L733)).
      Update it to `createSession(selectedSlug)` so the chosen model survives an
      expiry/reconnect — otherwise the agent silently drops back to the gateway
      default after the session times out. (This is the fourth session-creation
      site, alongside `initSession`, the New Session button, and the dropdown
      change — all must use the slug-aware create.)
- [x] Add a **New Session** button; factor the lifecycle into one
      `startNewSession(slug)` helper using **create-first-then-delete**:
      `createSession(slug)` FIRST -> on success commit selection, cancel stream,
      switch view, clear chat, then `DELETE` the previous id (best effort); on
      failure keep the current session/model and revert the dropdown. A monotonic
      `sessionSeq` token guards overlapping calls. Shared by the button and the
      dropdown-change handler
      ([`inferx_agent.html:727-778`](../dashboard/templates/inferx_agent.html#L727-L778)).

### Dashboard — default agent model (first open)

See "Default agent model (first open) — dashboard-owned".

- [x] Add `DEFAULT_AGENT_MODEL = os.getenv("DEFAULT_AGENT_MODEL", "").strip()`
      with the other module-level config near
      [`app.py:188-201`](../dashboard/app.py#L188-L201). Value is a published slug.
- [x] Pass it to the agent page as `default_agent_model` template context next to
      `endpoints` / `active_tenant` in `InferXAgent`
      ([`app.py:8218-8223`](../dashboard/app.py#L8218-L8223)).
- [x] In `inferx_agent.html`, change first-open selection so a brand-new session
      (no validated stored slug) preselects: `DEFAULT_AGENT_MODEL` if it is in the
      published list -> else the first published endpoint. Preselect it in the
      dropdown (showing `model_name`) and bind it to the session at create time.
      Do not override a valid stored selection or an existing session's bound
      model. (Because the slug is mandatory, there is no "empty Default" fallback
      target — see the invariant below.)
- [x] Empty-list case: if there are **no** published endpoints (nothing to
      preselect), the agent cannot start. Disable the composer + New Session
      button, show a clear "publish or select a model first" message, and **do not
      call `createSession` at all** — a slug-less create would be rejected with
      `400`.

### Mandatory-slug frontend invariant

- [x] After the gateway `400 no_agent_model` change, **every** `createSession`
      call site passes a non-empty, validated slug. The four sites are
      `initSession` (no-session branch), `startNewSession` (New Session button +
      dropdown change), and the expiry reconnect in `sendMessage`. Audit all four;
      none may pass `''`.
- [x] The empty-published-list state must block all of those paths up front (see
      empty-list item), so a slug-less create is never even attempted.
- [x] `createSession(slug)` always sends `body: { agent_endpoint: slug }` (no
      "omit when unset" path remains).

### Verification

- [ ] Slug-less `POST /agent/sessions` returns `400 no_agent_model` (no silent
      default).
- [ ] Audit confirms all four `createSession` call sites pass a validated
      non-empty slug, and the empty-list state blocks create entirely.
- [ ] Tests below pass.

## Testing

Once implemented, cover at least:

- **Mid-session unpublish:** bind a session to a published slug, unpublish it,
  then prompt — expect a clear error (no silent fallback to a default model), and
  that creating a new session / reselecting recovers.
- **Mid-session republish to a different context window:** confirm the next turn
  picks up the new endpoint's `context_length` (resolved fresh at prompt start),
  and that a published endpoint with NULL `context_length` uses the conservative
  built-in constant (not the global default, not 0).
- **Tenant switch with stale `localStorage`:** a saved slug from tenant A must
  not be sent for tenant B; it is treated as unset and falls back to no selection
  rather than failing `POST /agent/sessions`.
- **Per-turn `ctx` stability:** within one prompt, the same `ctx` is used across
  first-call preflight, synthesis preflight, and post-turn compaction even if a
  republish lands mid-turn (one metadata read per prompt).
