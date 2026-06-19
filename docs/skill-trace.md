# Skill Chain Trace Design

## Problem

When a user or agent calls skill A, which calls skill B, which calls skill C, there is no
visibility into what is happening. The caller waits with no indication of progress, making
it impossible to distinguish slow execution from a hang or error.

Goal: provide real-time progress information so the caller knows what is happening at every
level of the call tree.

---

## Call Tree Example

```
User/Agent
  └── skill A  (depth 1)
        ├── model inference
        └── call_skillep → skill B  (depth 2)
                              ├── model inference
                              └── call_skillep → skill C  (depth 3)
                                                    └── model inference
```

Desired progress output (in order of occurrence):

```
call_start          A
subcall_start       B        (A calls B)
subcall_start       C        (B calls C)
heartbeat           C        (C model still thinking)
subcall_finish      C
subcall_finish      B
call_finish         A
```

---

## Transport

### Transport mechanism

This design keeps the skill trace path minimal:
- Use the existing skill HTTP endpoint
- Use one HTTP POST request per skill call
- Return either normal JSON or an SSE (`text/event-stream`) response on that same request

The distinction between the two legs is the framing inside the stream:

| Leg | Mechanism | Framing |
|---|---|---|
| Skill endpoint → Dashboard / MCP server | HTTP POST → SSE response | Skill-trace SSE events |
| MCP server → Agent | MCP transport | MCP JSON-RPC (`notifications/progress`, `tools/call` result) |

```
Agent
  ── HTTP POST / SSE (MCP framing) ──→ MCP server (mcp_stream_server.rs)
                                           │  notify_progress()
                                           │  reqwest + SSE read
  ── HTTP POST / SSE (skill framing) ─→ Gateway / skill_chain.rs

Browser playground
  ── fetch() + SSE read ──────────────→ Gateway / skill_chain.rs
```

### SSE Stream

When `X-Skill-Trace: 1` header is present, the gateway response switches from a single JSON
body to a `text/event-stream` SSE response on the same POST request. The caller sends the
skill request and receives trace events on that same response stream.

The model calls inside the skill chain remain `stream: false` internally
(`skill_chain.rs:131`). The SSE stream is only on the outer gateway→caller connection.

This trace stream is plain SSE plus app-defined JSON payloads. It is not JSON-RPC and not a
new custom streamable-HTTP protocol.

### SSE Message Format

An SSE message is a block of fields terminated by a blank line:

```
event: <type>
data: <json>

```

Rules:
- `event:` without `data:` → ignored by client, never dispatched
- `data:` without `event:` → dispatched as generic `"message"` event (what vLLM uses)
- Both together → dispatched as the named event type
- A line starting with `:` is a **comment** — sent as bytes (prevents proxy idle timeout)
  but ignored by the client. Used for heartbeats when no data is needed.

### Full Stream Shape

```
event: skill_trace
data: {"type":"call_start",...}

event: skill_trace
data: {"type":"subcall_start",...}

: heartbeat

: heartbeat

event: skill_trace
data: {"type":"subcall_finish",...}

event: skill_trace
data: {"type":"call_finish",...}

event: skill_result
data: {"id":"chatcmpl-xxx","object":"chat.completion","choices":[...],...}

data: [DONE]
```

Key points:
- All `event: skill_trace` lines come **before** any result data, because the skill chain
  forces non-streaming internally and only returns a response after the full loop finishes.
- `event: skill_result` carries a complete (non-streaming) OpenAI chat completion object.
- `data: [DONE]` follows OpenAI convention and has no `event:` prefix.
- Plain `data:` lines with no `event:` prefix are never emitted during skill chain execution.
  They only appear from direct (non-chained) vLLM streaming calls.
- This version uses one endpoint and one response stream only; there is no separate trace
  subscribe/listen endpoint for production calls.

---

## Request Headers

| Header | Values | Effect |
|---|---|---|
| `X-Skill-Trace` | `1` | Enable trace SSE response |
| `X-Skill-Trace-Content` | `1` | Include `query` / `output` text in events |
| `X-Request-Id` | string | Correlates logs/events for one request; propagated to child calls |

Both `X-Skill-Trace` and `X-Skill-Trace-Content` are propagated to child skill calls
alongside the already-forwarded `Authorization`, `X-Request-Id`, and `traceparent`
headers.

Mode interaction:
- Without `X-Skill-Trace: 1`, the endpoint behaves normally and returns `application/json`
- With `X-Skill-Trace: 1`, the outer response is always SSE
- When trace is enabled, request-body `stream: true` is ignored for skill-chain execution and
  the final result is emitted as `event: skill_result`

---

## Trace Event Schema

```json
{
  "type":           "call_start | subcall_start | subcall_finish | call_finish | heartbeat",
  "call_id":        "uuid",
  "parent_call_id": "uuid | null",
  "depth":          1,
  "skill":          "tenant/namespace/name",
  "ts_ms":          1234567890,
  "elapsed_ms":     8200,    // absent on *_start events; present on *_finish and heartbeat

  // only when X-Skill-Trace-Content: 1
  "query":          "...",   // present on *_start events
  "output":         "...",   // present on *_finish events (subcall_finish only; call_finish omits
                             // this since the full result is in skill_result)

  // only on *_finish and call_finish events
  "result_code":    "pass | fail",
  "fail_reason":    "timeout | max_depth | transport_error | invalid_response | null"
}
```

`fail_reason` maps to existing detection points in `skill_chain.rs`:

| `fail_reason` | Location |
|---|---|
| `timeout` | line 588 — `timeout budget exhausted` |
| `max_depth` | line 760 — `max chain depth exceeded` |
| `transport_error` | line 666 — `child_call_transport_error` |
| `invalid_response` | line 741 — `child_call_invalid_response` |

### `call_id` origin

`call_id` is generated by the gateway trace layer. It is a trace-node id, not a model/tool id.

Current origin rules:
- Root call: the gateway creates a fresh UUID when trace mode starts for the request
- Direct child call: the forwarding parent creates a fresh UUID for that child trace node
- Deeper descendants: the child gateway instance creates its own local trace-node UUIDs, and
  the parent forwards them upstream while rewriting `depth` / `parent_call_id` as needed

Important distinctions:
- `call_id` is not derived from the skill name
- `call_id` is not the same as `X-Request-Id`
- `call_id` is not the same as the OpenAI / tool-call `id`
- One request can contain many `call_id` values because one request can contain many trace nodes

Think of it this way:
- `call_id` answers: "which trace node is this event about?"
- `parent_call_id` answers: "which trace node called it?"
- Tool-call ids answer: "which model-emitted tool invocation in the transcript was this?"

---

## Heartbeat

Two mechanisms:

**SSE comment** (`": heartbeat\n\n"`) — lightest option. Sends bytes to prevent proxy/idle
timeout. Client receives nothing; no JS handler needed. Used when no data is meaningful.

**`heartbeat` event** — when the caller benefits from knowing elapsed time and current phase:

```json
{
  "type":       "heartbeat",
  "call_id":    "a1",
  "depth":      1,
  "skill":      "tenant/ns/A",
  "phase":      "model_inference | child_call",
  "elapsed_ms": 8000,
  "ts_ms":      1008000
}
```

`phase` distinguishes what is blocking:
- `model_inference` — waiting for LLM to respond (`dispatch_func_call` await, `skill_chain.rs:431`)
- `child_call` — waiting for a child skill chain to finish (`child_req.send()` await, `skill_chain.rs:653`)

Implementation: wrap both awaits with `tokio::select!` and an interval ticker (5 second
default):

```rust
let mut ticker = tokio::time::interval(Duration::from_secs(5));
ticker.tick().await; // skip first immediate tick
let result = loop {
    tokio::select! {
        r = the_await_target => break r,
        _ = ticker.tick() => {
            emit_trace(&trace_tx, TraceEvent::heartbeat(..., "model_inference"));
        }
    }
};
```

---

## Where Trace Events Flow

### Direct HTTP caller (human/dashboard)

Trace events flow directly to whoever holds the SSE connection from the original POST request.
The dashboard or direct HTTP caller consumes them in real time.

### Agent via MCP

The agent speaks MCP protocol and must not receive raw SSE events. The MCP server
(`mcp_stream_server.rs`) acts as the adapter:

```
Agent  ←── MCP notifications/progress ───  MCP server  ←── SSE skill_trace events ───  Skill chain
```

The MCP server:
1. Adds `X-Skill-Trace: 1` to the reqwest call to the skill gateway
2. Reads the SSE stream line by line instead of buffering the full response (`.bytes()`)
3. Converts each `skill_trace` event to `context.peer.notify_progress(...)` — already
   called once today at `mcp_stream_server.rs:174`
4. Extracts the `skill_result` event as the final `CallToolResult`

The agent receives standard MCP `notifications/progress` with a human-readable `message`
field. No knowledge of SSE or skill chain internals required.

Important boundary:
- Skill trace on the gateway is plain SSE
- MCP transport remains MCP transport
- `mcp_stream_server.rs` only translates gateway SSE events into MCP notifications/results

Example agent-visible progress:
```
[progress] A: calling acme/default/pricer
[progress] acme/default/pricer: model thinking 5s...
[progress] acme/default/pricer done (pass, 8.2s)
[progress] A: done
```

---

## End-to-End Event Examples

These examples are intended as future-reference snapshots of the full trace path as seen by
different consumers. They are illustrative, but they match the current event model and
normalization rules in this document.

### Example 1 — Direct dashboard run, no child calls

User asks the root skill directly and the skill answers without calling any sub-skill.

Raw SSE stream from the gateway:

```text
event: skill_trace
data: {"type":"call_start","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/planner","ts_ms":1000}

: heartbeat

event: skill_trace
data: {"type":"heartbeat","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/planner","phase":"model_inference","elapsed_ms":5000,"ts_ms":6000}

event: skill_trace
data: {"type":"call_finish","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/planner","elapsed_ms":8200,"ts_ms":9200,"result_code":"pass","fail_reason":null}

event: skill_result
data: {"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Final answer"},"finish_reason":"stop"}]}

data: [DONE]
```

Dashboard text rendering:

```text
START  acme/skills/planner
WAIT   acme/skills/planner  5.0s
DONE   acme/skills/planner  8.2s
```

Key observations:
- The `: heartbeat` comment keeps the connection alive but does not render in the dashboard
- The structured `heartbeat` event renders as `WAIT`
- The final answer appears only after `event: skill_result`

### Example 2 — Full tree dashboard run, A → B → C

Root skill `A` calls `B`, and `B` calls `C`. The dashboard sees one single normalized stream
from the original top-level request.

Raw SSE stream from the gateway:

```text
event: skill_trace
data: {"type":"call_start","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/A","ts_ms":1000}

event: skill_trace
data: {"type":"subcall_start","call_id":"b-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/B","ts_ms":1800}

event: skill_trace
data: {"type":"subcall_start","call_id":"c-call","parent_call_id":"b-call","depth":3,"skill":"acme/default/C","ts_ms":2400}

: heartbeat

event: skill_trace
data: {"type":"heartbeat","call_id":"c-call","parent_call_id":"b-call","depth":3,"skill":"acme/default/C","phase":"model_inference","elapsed_ms":5000,"ts_ms":7400}

event: skill_trace
data: {"type":"subcall_finish","call_id":"c-call","parent_call_id":"b-call","depth":3,"skill":"acme/default/C","elapsed_ms":6200,"ts_ms":8600,"result_code":"pass","fail_reason":null}

event: skill_trace
data: {"type":"subcall_finish","call_id":"b-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/B","elapsed_ms":6900,"ts_ms":9300,"result_code":"pass","fail_reason":null}

event: skill_trace
data: {"type":"call_finish","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/A","elapsed_ms":10400,"ts_ms":11400,"result_code":"pass","fail_reason":null}

event: skill_result
data: {"id":"chatcmpl-2","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"A final answer using B and C"},"finish_reason":"stop"}]}

data: [DONE]
```

Dashboard text rendering:

```text
START  acme/skills/A
  START  acme/default/B
    START  acme/default/C
    WAIT   acme/default/C  5.0s
    DONE   acme/default/C  6.2s
  DONE   acme/default/B  6.9s
DONE   acme/skills/A  10.4s
```

Key observations:
- The child emitted its own local `call_start` / `call_finish`, but the parent rewrote them
  to `subcall_start` / `subcall_finish`
- `depth` is normalized to the full tree, not the child’s local tree
- `parent_call_id` preserves the true immediate ancestry (`C` points to `B`, not directly to `A`)

### Example 3 — Child-call wait heartbeat vs model wait heartbeat

The root call can emit `WAIT` while blocked on its own model or while blocked on a child.
The dashboard currently renders both as `WAIT`, but the underlying trace event distinguishes
them with `phase`.

Underlying trace events:

```json
{"type":"heartbeat","call_id":"root-a1","depth":1,"skill":"acme/skills/A","phase":"model_inference","elapsed_ms":5000,"ts_ms":6000}
{"type":"heartbeat","call_id":"b-call","depth":2,"skill":"acme/default/B","phase":"child_call","elapsed_ms":5000,"ts_ms":9000}
```

Current dashboard rendering:

```text
WAIT   acme/skills/A  5.0s
  WAIT   acme/default/B  5.0s
```

Interpretation:
- First line: root skill `A` is still waiting for its own model turn to complete
- Second line: `B` is still waiting for a child skill call to return

### Example 4 — Failure after SSE starts

The root skill starts tracing, calls a child, and the child returns an invalid response.
Because SSE has already started, the gateway must finish the trace cleanly rather than
switching back to a plain HTTP error.

Raw SSE stream from the gateway:

```text
event: skill_trace
data: {"type":"call_start","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/A","ts_ms":1000}

event: skill_trace
data: {"type":"subcall_start","call_id":"b-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/B","ts_ms":1800}

event: skill_trace
data: {"type":"subcall_finish","call_id":"b-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/B","elapsed_ms":2100,"ts_ms":3900,"result_code":"fail","fail_reason":"invalid_response"}

event: skill_trace
data: {"type":"call_finish","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/A","elapsed_ms":4300,"ts_ms":5300,"result_code":"fail","fail_reason":"invalid_response"}

data: [DONE]
```

Dashboard text rendering:

```text
START  acme/skills/A
  START  acme/default/B
  FAIL   acme/default/B  2.1s
FAIL   acme/skills/A  4.3s
```

Key observations:
- There is no `event: skill_result` on failure-after-SSE
- The stream still terminates cleanly with `[DONE]`
- The client should treat EOF before `[DONE]` as interrupted/broken, but `[DONE]` with failed
  finish events as a logical failure

### Example 5 — MCP translation of the same run

Suppose the gateway stream for a chained skill run is:

```text
event: skill_trace
data: {"type":"call_start","call_id":"root-a1","depth":1,"skill":"acme/skills/planner","ts_ms":1000}

event: skill_trace
data: {"type":"subcall_start","call_id":"price-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/pricer","ts_ms":1800}

event: skill_trace
data: {"type":"heartbeat","call_id":"price-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/pricer","phase":"model_inference","elapsed_ms":5000,"ts_ms":6800}

event: skill_trace
data: {"type":"subcall_finish","call_id":"price-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/pricer","elapsed_ms":8200,"ts_ms":10000,"result_code":"pass","fail_reason":null}

event: skill_trace
data: {"type":"call_finish","call_id":"root-a1","depth":1,"skill":"acme/skills/planner","elapsed_ms":10300,"ts_ms":11300,"result_code":"pass","fail_reason":null}

event: skill_result
data: {"id":"chatcmpl-3","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Planned with pricing"},"finish_reason":"stop"}]}

data: [DONE]
```

The MCP server converts that into agent-visible progress messages such as:

```text
[progress] acme/skills/planner: started
[progress] acme/skills/planner: calling acme/default/pricer
[progress] acme/default/pricer: model inference 5.0s
[progress] acme/default/pricer: pass 8.2s
[progress] acme/skills/planner: pass 10.3s
```

And returns the `skill_result` payload as the final `CallToolResult`.

Key observations:
- The agent never sees raw SSE
- The MCP server is an adapter, not a second trace protocol

### Example 6 — Debug run with content included

The debug endpoint always includes prompt/output content and stores the session after the run.

Raw debug SSE stream from the gateway:

```text
event: skill_trace
data: {"type":"call_start","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/planner","ts_ms":1000,"query":"plan my trip to Paris"}

event: skill_trace
data: {"type":"subcall_start","call_id":"price-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/pricer","ts_ms":1800,"query":"Paris flight + hotel price"}

event: skill_trace
data: {"type":"subcall_finish","call_id":"price-call","parent_call_id":"root-a1","depth":2,"skill":"acme/default/pricer","elapsed_ms":1200,"ts_ms":3000,"output":"€500","result_code":"pass","fail_reason":null}

event: skill_trace
data: {"type":"call_finish","call_id":"root-a1","parent_call_id":null,"depth":1,"skill":"acme/skills/planner","elapsed_ms":4100,"ts_ms":5100,"result_code":"pass","fail_reason":null}

event: skill_result
data: {"id":"chatcmpl-4","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"Trip planned for €500"},"finish_reason":"stop"}]}

data: [DONE]
```

Stored debug session shape:

```json
{
  "session_id": "dbg-123",
  "owner_tenant": "acme",
  "namespace": "default",
  "skillname": "planner",
  "created_at_ms": 1234567890,
  "events": [
    {"type":"call_start","query":"plan my trip to Paris", "...":"..."},
    {"type":"subcall_start","query":"Paris flight + hotel price", "...":"..."},
    {"type":"subcall_finish","output":"€500", "...":"..."},
    {"type":"call_finish","result_code":"pass", "...":"..."}
  ],
  "final_result": {"id":"chatcmpl-4","object":"chat.completion","choices":[...]}
}
```

Key observations:
- Production trace is content-gated by `X-Skill-Trace-Content: 1`
- Debug trace always includes content because the endpoint itself is privileged
- The stored session is intended for short-lived debugging, not durable archival

---

## Implementation in `skill_chain.rs`

### State additions

```rust
struct SkillChainRequestState {
    // ... existing fields ...
    trace_tx:   Option<mpsc::UnboundedSender<SkillTraceEvent>>,
    call_id:    String,   // UUID for this call level
}
```

### Response setup (before loop)

If `X-Skill-Trace: 1` present:
- Create `(trace_tx, trace_rx)` channel
- Spawn task that reads from `trace_rx` and writes SSE to the response body
- Set `Content-Type: text/event-stream`
- Store `trace_tx` in state

Terminal rules once SSE starts:
- Success: exactly one root `call_start`, zero or more additional `skill_trace` events, exactly
  one root `call_finish`, then exactly one `skill_result`, then `data: [DONE]`
- Failure after SSE starts: emit a terminal `skill_trace` finish event with `result_code=fail`,
  emit no `skill_result`, then `data: [DONE]`
- Failure before SSE starts: keep existing non-streaming HTTP error behavior

### Trace emission points

| Point | Event |
|---|---|
| Entry to `handle_skill_call_chain` | `call_start` |
| Before `child_req.send()` | `subcall_start` |
| Interval tick during `child_req.send()` await | `heartbeat` (phase=child_call) |
| After child returns | `subcall_finish` |
| Interval tick during `dispatch_func_call` await | `heartbeat` (phase=model_inference) |
| Final response ready | `call_finish` |

### Child event bubbling (full tree)

For full A→B→C visibility, when calling child skills with `X-Skill-Trace: 1`:
- Child responds with SSE
- Parent reads child SSE stream
- Parent forwards child trace progress upstream to its own `trace_tx`
- Parent extracts `skill_result` event as the tool result content

Normalization rule:
- The root call is represented by `call_start` / `call_finish`
- Descendants are represented by `subcall_start` / `subcall_finish`
- If a child emits root-level events for itself, the parent rewrites them before forwarding so
  downstream consumers do not see mixed root/subcall models
- `depth`: the forwarding parent rewrites each child event's `depth` to the child's actual
  depth in the full tree (parent's depth + 1 for direct children, deeper for further
  descendants). The child only knows its own local depth starting at 1.
- `parent_call_id`: rewritten to preserve the immediate ancestry in the full tree. For a direct
  child event, this is the forwarding parent's `call_id`. For deeper descendants, the parent
  must preserve the descendant's true immediate parent rather than flattening everything to the
  current forwarder.

Requirement for dashboard trace:
- Full nested visibility is required so a single dashboard prompt can show the complete
  execution path, including B→C and deeper descendants, on one progress stream
- Depth-1-only mode is not sufficient for the dashboard user experience and is not the intended
  end-state

---

## Dashboard Integration (`skill_detail.html`)

The playground already makes a streaming fetch (`skill_detail.html:284-378`) and has an SSE
reader loop (`skill_detail.html:333-367`) that handles `data:` lines.

### Changes

1. Add `X-Skill-Trace: 1` to the playground fetch headers
2. Extend the SSE parser to track `event:` before consuming `data:`
3. Add a progress text area above the output textarea

### Extended SSE parser

```js
let pendingEvent = null;
// in the line-processing loop:
if (line.startsWith('event:')) {
    pendingEvent = line.slice(6).trim();
    continue;
}
if (line.trim() === '') { pendingEvent = null; continue; }
if (!line.startsWith('data:')) continue;

const data = line.slice(5).trim();
if (data === '[DONE]') { pendingEvent = null; continue; }

if (pendingEvent === 'skill_trace') {
    try { appendTrace(JSON.parse(data)); } catch (_) {}
} else if (pendingEvent === 'skill_result') {
    try {
        const content = extractTextContent(
            JSON.parse(data)?.choices?.[0]?.message?.content
        );
        if (content) outputEl.value = content;
    } catch (_) {}
} else {
    // plain data: no event prefix — vLLM streaming delta (non-skill-chain calls)
    try {
        const delta = JSON.parse(data)?.choices?.[0]?.delta?.content;
        if (delta) outputEl.value += delta;
    } catch (_) {}
}
pendingEvent = null;
```

Note: during skill chain execution, the `else` (vLLM delta) branch is never reached because
all data arrives only after the chain completes and is wrapped in `event: skill_result`.

### Progress text area rendering

```js
function appendTrace(ev) {
    tracePanel.hidden = false;
    const icon = {
        subcall_start:  '→',
        subcall_finish: ev.result_code === 'pass' ? '✓' : '✗',
        heartbeat:      '…',
        call_start:     '▶',
        call_finish:    ev.result_code === 'pass' ? '■' : '✗',
    }[ev.type] || '·';
    const indent = '  '.repeat((ev.depth || 1) - 1);
    const timing = ev.elapsed_ms != null ? `  ${(ev.elapsed_ms / 1000).toFixed(1)}s` : '';
    const line = document.createElement('div');
    line.className = `trace-line trace-${ev.type}`;
    line.textContent = `${indent}${icon} ${ev.skill}${timing}`;
    traceList.appendChild(line);
}
```

Example progress text area during a run:
```
▶ acme/default/planner
  → acme/default/pricer
  … acme/default/pricer  5.0s
  ✓ acme/default/pricer  8.2s
  → acme/default/booker
  ✓ acme/default/booker  1.8s
■ acme/default/planner  10.3s
```

---

## Debug Tool for Skill Owners

Separate from the agent-facing trace, skill owners need deeper debugging capability when
building skills that call other skills.

### Differences from agent progress

| | Agent progress (MCP) | Skill owner debug |
|---|---|---|
| Audience | End user's agent | Skill developer |
| Content visible | No | Yes — full prompts + outputs |
| Trigger | Every production call | Explicit debug invocation |
| Format | MCP `notifications/progress` | Structured call tree |
| Result stored | No | Yes, short-lived session |

### Debug invocation endpoint

```
POST /skills/debug/{tenant}/{ns}/{skill}
Authorization: Bearer <owner-apikey>

{
  "query": "plan my trip to Paris",
  "stream": true
}
```

- Auth check: caller must be the skill's owner tenant
- Content always included (no opt-in required — debug endpoint implies it)
- Result stored server-side as a debug session, retrievable via
  `GET /skills/debug/session/{session_id}`

### Sub-skill mocking

For testing a skill in isolation without invoking real child skills:

```json
{
  "query": "plan my trip",
  "mocks": {
    "acme/default/pricer": "€500",
    "acme/default/booker": "Booked, ref #123"
  }
}
```

The gateway intercepts `call_skillep` tool calls targeting mocked skills and returns the mock
response directly, skipping the HTTP call. Implementation: check the mocks map before
`child_req.send()` in the skill chain loop.

### Dashboard "Skill Debugger"

A tab on the skill detail page (owners only) with:
- Input form: query text box + "Run" button
- Live call tree that builds as SSE events stream in
- Expandable nodes showing prompts and outputs at each level
- Timing and pass/fail badges per node
- Session history: list of recent debug runs, re-openable after the fact

---

## Implementation Milestones

Full-tree trace is the required end-state. The milestones below are engineering rollout steps,
not separate product requirements.

### Milestone 1 — Dashboard progress plumbing

- Gateway: emit depth-1 `skill_trace` events + SSE comment heartbeats when
  `X-Skill-Trace: 1` present
- Dashboard playground: add header, extend SSE parser, add progress text area
- Scope: ~80 lines Rust in `skill_chain.rs`, ~60 lines JS/HTML in `skill_detail.html`
- No change to agent or MCP behavior
- This is only an incremental first step; it does not satisfy the final dashboard goal by itself

### Milestone 2 — Full tree (B→C visibility)

- Child calls propagate `X-Skill-Trace`, return SSE
- Parent reads child SSE stream, rewrites descendant lifecycle events as needed, then forwards
  them upstream
- Heartbeat events include `phase` and `elapsed_ms`
- This milestone is required for the intended dashboard experience and is not optional

### Milestone 3 — MCP progress notifications

- MCP server reads SSE from skill gateway instead of buffering full response
- Converts `skill_trace` events to `notify_progress` calls
- Agent sees standard MCP progress; no protocol changes on agent side

### Milestone 4 — Debug tool

- Debug endpoint with owner auth check and content inclusion
- Session storage
- Sub-skill mocking
- Dashboard debug tab with full tree view and session history

---

## Minimal Contract

The first version intentionally stays minimal:
- One existing skill endpoint
- Optional trace mode via `X-Skill-Trace: 1`
- SSE framing only
- App-level JSON event schema only
- No JSON-RPC envelope for skill trace
- No separate production trace subscription endpoint

Version notes:
- Milestone 1 uses SSE comment heartbeats only (`: heartbeat`)
- Structured `heartbeat` trace events begin in Milestone 2 when `phase` / `elapsed_ms` are added

Non-goals for this version:
- Resumable trace sessions
- Multi-viewer trace fanout
- A new custom streamable-HTTP protocol

---

## Implementation Checklist

Each item is tagged with the milestone it belongs to: `[M1]` `[M2]` `[M3]` `[M4]`.

Status below reflects the code in this repo as of 2026-06-04. Items are checked only when the
described behavior already exists, not when a nearby prerequisite exists.

### Gateway trace mode

- [x] `[M1]` Detect `X-Skill-Trace: 1` on skill requests
- [x] `[M1]` Keep normal `application/json` behavior when the header is absent
- [x] `[M1]` Switch to `text/event-stream` on the same POST response when the header is present
- [x] `[M1]` Ignore request-body `stream: true` during skill-chain execution when trace mode is enabled

### SSE writer

- [x] `[M1]` Create a trace event channel for one request
- [x] `[M1]` Stream SSE messages from that channel to the response body
- [x] `[M1]` Emit `event: skill_trace` for progress/lifecycle events
- [x] `[M1]` Emit `event: skill_result` for the final OpenAI-style completion object on success
- [x] `[M1]` Emit `data: [DONE]` exactly once on clean termination

### Root lifecycle events

- [x] `[M1]` Emit exactly one root `call_start`
- [x] `[M1]` Emit exactly one root `call_finish`
- [x] `[M1]` Include `result_code=pass|fail` on finish events
- [x] `[M1]` Include `fail_reason` on failed terminal events

### Failure handling

- [x] `[M1]` Before SSE starts: keep existing HTTP/JSON error behavior
- [x] `[M1]` After SSE starts: emit terminal failed `call_finish`, emit no `skill_result`, then `[DONE]`
- [x] `[M1]` Treat connection close without `[DONE]` as interrupted/broken

### Child-call propagation

- [x] `[M2]` Propagate `X-Skill-Trace` and `X-Skill-Trace-Content` to child skill calls
- [x] `[M1]` Keep propagating `Authorization`, `X-Request-Id`, `traceparent`
- [x] `[M2]` Read child responses as SSE when trace mode is enabled
- [x] `[M2]` Extract child `skill_result` as the tool result content

### Full-tree normalization

- [x] `[M2]` Forward descendant trace events upstream
- [x] `[M2]` Normalize descendant lifecycle to `subcall_start` / `subcall_finish`
- [x] `[M2]` Rewrite `depth` to reflect actual position in the full tree (parent depth + relative depth)
- [x] `[M2]` Rewrite `parent_call_id` to preserve the true immediate parent for each descendant
- [x] `[M2]` Prevent mixed root/descendant event models from leaking to clients

### Heartbeats

- [x] `[M1]` Emit SSE comment heartbeats (`: heartbeat`) during model wait and child-call wait
- [x] `[M2]` Add structured `heartbeat` `skill_trace` events with `phase` and `elapsed_ms`
- [x] `[M2]` Add ticker logic around `dispatch_func_call` await and `child_req.send()` await

### Trace schema

- [x] `[M1]` Implement the documented `skill_trace` payload fields
- [x] `[M1]` Omit `elapsed_ms` on `*_start` events; populate on `*_finish` and `heartbeat`
- [x] `[M2]` Gate `query` / `output` behind `X-Skill-Trace-Content: 1`
- [x] `[M1]` Ensure timestamps are populated consistently
- [x] `[M1]` Keep event ordering deterministic

### Dashboard

- [x] `[M1]` Add `X-Skill-Trace: 1` to playground requests
- [x] `[M1]` Extend the current streaming parser to handle `event:`
- [x] `[M1]` Add a progress text area above the output textarea
- [x] `[M1]` Render one line per event with indentation by `depth`
- [x] `[M1]` On success: show progress lines, then final output in the output area
- [x] `[M1]` On failure: show terminal failure line and no final result

### MCP adapter

- [x] `[M3]` Add `X-Skill-Trace: 1` on gateway calls from `mcp_stream_server.rs`
- [x] `[M3]` Stop buffering full response with `.bytes()` in trace mode
- [x] `[M3]` Parse SSE incrementally
- [x] `[M3]` Convert `skill_trace` events into `notify_progress(...)`
- [x] `[M3]` Convert `skill_result` into final `CallToolResult`

### Debug tool

- [x] `[M4]` Add debug endpoint with owner auth check
- [x] `[M4]` Always include content (query/output) on debug endpoint
- [x] `[M4]` Store debug session server-side; expose via `GET /skills/debug/session/{id}`
- [x] `[M4]` Implement sub-skill mocking via `mocks` map in request body
- [x] `[M4]` Add dashboard debug tab with live tree view and session history

### Tests

- [ ] `[M1]` No-trace request still returns normal JSON
- [ ] `[M1]` Trace request returns SSE with correct `Content-Type` header
- [ ] `[M1]` Success path emits `call_start -> ... -> call_finish(pass) -> skill_result -> [DONE]`
- [ ] `[M1]` Failure-after-SSE emits `call_finish(fail)` and no `skill_result`
- [ ] `[M2]` Nested A→B→C trace is visible at A
- [ ] `[M2]` Child root events are normalized to `subcall_*` with correct `depth` and `parent_call_id`
- [ ] `[M1]` Dashboard parser handles `skill_trace`, `skill_result`, comment heartbeats, `[DONE]`
- [ ] `[M3]` MCP adapter handles SSE trace and delivers final result correctly

### Verification / rollout

- [ ] `[M1]` Test direct dashboard flow against a depth-1 skill chain
- [ ] `[M2]` Test direct dashboard flow against a depth-2+ skill chain (A→B→C)
- [ ] `[M3]` Test MCP flow against a chained skill
- [ ] `[M1]` Verify long-running calls do not appear hung (comment heartbeats firing)
- [ ] `[M1]` Verify broken streams are surfaced distinctly from logical failures
