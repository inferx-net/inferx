# Proposal: Adopt The `skill2` Refactor Direction On Top Of `main` Without Regressions

## Goal

Adopt the readability and maintainability direction from `skill2`, but do it on top of the current `main` implementation and preserve the existing behavior of:

- public `/skills/...` HTTP APIs
- tenant resolution and billing/quota behavior
- skill chaining and child skill calls
- MCP tracing and cancellation
- non-chat skill subpaths

The intent is to improve structure, not to change the public contract.

## Current `main` Baseline

`main` already has important behavior that should not be regressed:

- `http_gateway::SkillCall` is the trust boundary for tenant resolution.
- top-level skill requests resolve tenant from auth-derived context through `resolve_skill_calling_tenant(...)`.
- `skill_chain::parse_skill_chain_request(...)` injects tool definitions, validates child depth, forces `stream = false`, and preserves trace behavior.
- `mcp_stream_server::call_tool(...)` sends loopback HTTP requests with headers for tenant, trace, and MCP cancellation.
- child skill calls in `skill_chain` use the same public chat-completions wire format as external callers.

That baseline is correct in terms of behavior, even if some of the code paths are harder to follow than they should be.

## Recommended Direction

### 1. Keep The Public Skill API Stable

Do not move trusted fields such as `tenant` or trace control into the public JSON request body for `/skills/.../v1/chat/completions`.

Keep the public request contract effectively as:

- `messages`
- `max_tokens`
- `temperature`
- `stream`
- existing optional request fields already accepted by `main`

Tenant identity, trace enablement, and cancellation should remain derived from authenticated headers and server-side context.

### 2. Separate Public Request Data From Internal Execution Context

The `skill2` direction is useful if it is split into two concepts instead of one:

- `SkillChatRequest`
  - typed representation of the public chat-completions body
  - contains only request-body fields

- `SkillExecutionContext`
  - internal-only metadata derived by the gateway
  - examples:
    - `calling_tenant`
    - `trace_enabled`
    - `trace_include_content`
    - `cancel_token`
    - `allowed_child_skilleps`

This preserves the trust boundary while still making downstream code cleaner.

### 3. Keep `http_gateway::SkillCall` As The Trust Boundary

`SkillCall` should continue to:

- detect whether the request is the chat-completions POST path or a generic skill subpath
- resolve tenant from auth context
- enforce quota/billing based on the resolved caller
- derive cancellation and trace behavior from headers/context
- pass typed data into the next layer only after trust decisions are complete

This is the most important anti-regression rule.

### 4. Extract Shared Skill-Chain Normalization

The maintainability win in `skill2` should be adopted here by extracting the normalization logic from `parse_skill_chain_request(...)` into smaller helpers, not by bypassing that parser.

Recommended helper split:

- `parse_skill_chat_request(body_bytes) -> SkillChatRequest`
- `normalize_skill_chain_template(request, allowed_child_skilleps, is_child, child_depth) -> Value`
- `build_skill_chain_state(template, headers, debug_mocks, current_depth) -> SkillChainRequestState`

The key behavior that must remain centralized:

- reject client-provided non-empty top-level tools
- inject the `call_skillep` tool only when allowed
- omit tools when the allowlist is empty
- force model turns to `stream = false`
- validate child depth headers
- preserve request-history layout

### 5. Keep Child Calls Backward-Compatible

Child skill calls should keep using the same public chat-completions request shape that external callers use today.

Do not introduce a new required child payload schema unless every caller is migrated and compatibility is explicitly maintained.

That means the child-call code in `skill_chain` should continue to send:

- `messages`
- `stream`
- optional standard chat fields as needed

and keep tenant/trace/cancel metadata in headers or internal context, not in required JSON body fields.

### 6. Improve MCP Call Construction Without Changing Endpoint Semantics

`mcp_stream_server.rs` is a good place to adopt more structure from `skill2`, but only internally.

Recommended cleanup:

- keep a typed request struct for the MCP-generated chat body
- extract request construction into a small helper
- keep `X-Skill-Trace` and `X-Mcp-Cancel-Id` in headers
- keep SSE parsing and trace resolution helpers as they are, or clean them up further if behavior remains identical

This improves readability without changing the public skill endpoint contract.

## Concrete Refactor Plan

### Phase 1: Pure Internal Cleanup

No behavior change expected.

- In `mcp_stream_server.rs`
  - rename or document the request struct more clearly
  - extract request-body creation into a helper
  - keep header-based tenant/trace/cancel propagation unchanged

- In `http_gateway.rs`
  - extract the chat-path check into a helper
  - extract route-building data into a small internal struct if helpful
  - keep request parsing order behaviorally identical

- In `skill_chain.rs`
  - split `parse_skill_chain_request(...)` into smaller helpers
  - keep all validation and normalization behavior identical

### Phase 2: Typed Internal Adapters

Introduce typed internal representations only after Phase 1 is covered by tests.

- add `SkillChatRequest`
- add `SkillExecutionContext`
- adapt `handle_skill_call_chain(...)` to take typed/internal pieces while preserving current external behavior

This should be an internal signature cleanup, not a protocol change.

### Phase 3: Optional Consolidation

If the first two phases are stable, consider consolidating duplicated logic around:

- trace flag derivation
- child-depth parsing
- chat-request serialization
- tool injection policy

Only do this after behavior is locked down by tests.

## No-Regression Requirements

The following behaviors should be explicitly protected before and during refactoring.

### Auth And Tenant Safety

- restricted inference API keys cannot impersonate another tenant
- tenant resolution comes from `restrictTenant` or `defaultTenant`, and must still pass auth-derived checks
- quota and billing still use the resolved caller tenant logic already in `main`

### Skill Routing

- non-POST or non-chat skill subpaths still pass through untouched
- chat-completions POST still routes into skill-chain handling

### Skill Chaining

- top-level requests still inject the skill tool when allowed
- empty allowlist still omits tools
- child-depth validation still works
- child skill calls still succeed with the current public wire format

### Trace And Cancellation

- `X-Skill-Trace` still enables SSE trace mode
- `X-Skill-Trace-Content` still controls content inclusion
- MCP cancellation still reaches `execute_skill_chain(...)`
- dropped SSE clients still cancel execution as they do in current `main`

### Response Compatibility

- normal chat completion responses remain unchanged
- error mapping remains unchanged for invalid chain depth, bad request, and downstream failures

## Test Plan

Before code movement, add or confirm coverage for:

1. `resolve_skill_calling_tenant(...)`
   - matching restricted tenant hint succeeds
   - mismatched restricted tenant hint fails

2. `parse_skill_chain_request(...)`
   - rejects non-empty client tools
   - injects tool definition for empty tools
   - omits tools for empty allowlist
   - rejects invalid child depth

3. `handle_skill_call_chain(...)` behavior
   - non-trace path returns standard response
   - trace path returns SSE stream

4. child-call compatibility
   - child request body shape remains valid for `SkillCall`

5. MCP path
   - request body stays standard chat-completions JSON
   - trace and cancellation continue to flow through headers

## Recommendation Summary

The right move is not to port `skill2` literally.

The right move is to adopt its design intent:

- clearer typed data flow
- fewer ad hoc conversions
- smaller functions
- better separation of concerns

while preserving the behavioral guarantees already present in `main`.

In short:

- keep public protocol stable
- keep trust decisions in `http_gateway`
- keep chain normalization centralized
- use typed structs internally
- add tests first, then refactor in small phases
