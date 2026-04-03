# Model Status UX Analysis

## Problem

Users are confused after deploying a model. During snapshot creation and pod boot, they have no clear signal of when the model is ready to serve inference.

---

## What's Currently Broken

### 1. After deploy, no navigation to the model page
Three deploy surfaces all show a client-side success modal instead of navigating to the model page:

- **`func_create` path**: Submits via `fetch('/func_save')` (`func_create.html:1630`), then calls `showModelSaveModal()` (`func_create.html:1657`). Modal shows Pods/Models links only.
- **Catalog detail path**: Backend returns `redirect_url` in the JSON response (`app.py:5011`), but `catalog_detail.html`'s success handler calls `showModelSaveModal()` (`:601`, defined at `:546`) which ignores it and shows generic links.
- **Catalog list quick-deploy path**: `handleDeployNow()` (`catalog_list.html:633`) calls the same `showModelSaveModal()` (`catalog_list.html:666`) after the fetch succeeds. The response includes `redirect_url` (same backend endpoint as catalog detail), but the handler passes the raw `payload` to a modal that ignores it (`catalog_list.html:575`).

Fix: all three paths need a **frontend navigation action** — a backend-only redirect has no effect since all three use `fetch`.

### 2. Status is binary and misleading
Both the list (`func_list.html:262`) and detail (`func.html:74`) pages show `FuncStatus.state`, which is only `Normal` or `Fail`. During the entire boot process — image pull, pod init, model loading — the state is already `Normal`. This tells users nothing about progress.

### 3. Pod states are the real signal, but they're hidden
The actual progress lives in pod states:
```
Init → PullingImage → Creating → Created → Loading → Ready
```
These are shown in `func.html:132-151`, but buried in a raw admin-style table. No regular user would know to look there.

### 4. No auto-refresh
The page is static. Users must manually reload repeatedly to check progress.

---

## Pod State Machine (complete reference)

From `pod_mgr.rs:235`:

| State | Meaning |
|---|---|
| `Init` | Scheduled, waiting for node assignment |
| `PullingImage` | Downloading container image |
| `Creating` | Node agent creating the container |
| `Created` | Container created, not yet running |
| `Loading` | Container running, readiness probe in progress (model loading into GPU) |
| `LoadingTimeout` | Readiness probe timed out — pod will be restarted |
| `Ready` | Readiness probe passed — serving traffic |
| `Snapshoting` | Snapshot being written to disk |
| `Snapshoted` | Snapshot written |
| `Restoring` | Restoring from snapshot |
| `Standby` | Hibernated to save GPU — normal resting state; resumes on first inference request |
| `Resuming` | Waking from standby |
| `ResumeDone` | Resume complete |
| `MemHibernated` | GPU memory hibernated to DRAM |
| `Terminating` | Shutdown in progress |
| `Terminated` | Graceful exit |
| `Failed` | Abnormal exit |
| `Cleanup` | Artifacts being removed |
| `Deleted` | — |

---

## Recommendations

### Change 1: Frontend navigation to the model page after deploy or update

Create and update flows should set the `view=user` query param on the redirect URL so users land on the model detail/readiness page rather than staying on the editor or falling into the admin default.

In all three paths, use `URLSearchParams` to set `view=user` — the backend URL from `app.py:5011` already contains query params, so string concatenation is fragile and can produce duplicate params:
```js
const url = new URL(redirectUrl, window.location.origin);
url.searchParams.set('view', 'user');
window.location.href = url.toString();
```

**`func_create` path**: Frontend-only change. `tenant`, `namespace`, and `name` are already in the submitted `payload` (`func_create.html:1622-1625`), and the success branch already exists at `func_create.html:1657`. Use the same redirect for both create and update so a successful save lands on the model detail page with `view=user`.

**Catalog detail path**: Frontend-only change. The backend already returns `redirect_url` (`app.py:5011`). The response is parsed into `payload` at `catalog_detail.html:594`. At the success call site (`catalog_detail.html:601`), navigate to `payload.redirect_url` with `view=user` added via `URLSearchParams` instead of calling `showModelSaveModal()` (`catalog_detail.html:546`).

**Catalog list quick-deploy path**: Frontend-only change. `handleDeployNow()` (`catalog_list.html:633`) parses the response into `payload` and calls `showModelSaveModal(payload)` (`catalog_list.html:666`). Navigate to `payload.redirect_url` with `view=user` via `URLSearchParams` instead.

### Change 2: Infer meaningful model status from pods — aggregate model state

The default create-page policy is `min_replica=0`, `standby_per_node=1` (`func_create.html:693,695`). With this policy, a freshly deployed model will likely transition `Loading → Ready → Standby`, and stay in `Standby`.

**High-level principle**: model status should be computed as an aggregate over all current-version pods, using **override precedence** based on what the user can do right now. If one pod is already usable, lower-priority pods should not hide that.

Examples:

- If **any pod is `Ready`**, the model state is **`Ready`** regardless of other pods.
- If there is **no `Ready` pod**, but **any pod is `Resuming`**, the model state is **`Resuming...`**.
- If there is **no `Ready` or `Resuming` pod**, but **any pod is `Standby`**, the model state is **`Standby`**.
- If there is **no `Ready` / `Resuming` / `Standby` pod**, but **any restore pod is still in an active pre-standby state**, the model state is **`Restoring`**.

For the current workflow, we can ignore `Normal` pods and optimize around the snapshot/restore path:

- **Snapshot pod**, any active state: `Creating Snapshot...`
- **Restore pod**, any active state before `Standby`: `Restoring`
- `Standby`, `MemHibernated`: `Standby`
- `Resuming`, `ResumeDone`: `Resuming...`
- `Ready`: `Ready`
- `FuncStatus=Fail`: `Failed`
- Fallback / unmapped early state: `Pending`

This gives users a simple flow:

`Creating Snapshot...` → `Restoring` → `Standby` → `Resuming...` → `Ready`

```python
def infer_model_status(pods, func_status_state):
    def state_of(p):
        return p["object"]["status"]["state"]

    def create_type_of(p):
        return p["object"]["spec"].get("create_type")

    # Override precedence: highest user value wins.
    if any(state_of(p) == "Ready" for p in pods):
        return "ready", "Ready"

    if any(state_of(p) in {"Resuming", "ResumeDone"} for p in pods):
        return "resuming", "Resuming..."

    if any(state_of(p) in {"Standby", "MemHibernated"} for p in pods):
        return "standby", "Standby"

    # Whitelist active restore states only — teardown states (Terminating, Cleanup, Deleted)
    # are not progress. Once the restored pod reaches Standby, the user-facing
    # state becomes Standby instead of Restoring.
    _RESTORE_ACTIVE = {"Init", "PullingImage", "Creating", "Created", "Loading", "LoadingTimeout", "Restoring"}
    if any(create_type_of(p) == "Restore" and state_of(p) in _RESTORE_ACTIVE for p in pods):
        return "restoring", "Restoring"

    # Whitelist active snapshot states only.
    _SNAPSHOT_ACTIVE = {"Init", "PullingImage", "Creating", "Created", "Loading", "Snapshoting"}
    if any(create_type_of(p) == "Snapshot" and state_of(p) in _SNAPSHOT_ACTIVE for p in pods):
        return "snapshotting", "Creating Snapshot..."

    if func_status_state == "Fail":
        return "failed", "Failed"

    # Conservative fallback: avoid false failures for unknown states.
    return "pending", "Pending"
```

Polling should stop when status becomes `ready` or `failed`. Stop at `standby` as well, but **resume polling** when the user clicks the inference button (`func.html:101` — `streamOutput()`), since that triggers a resume and the pod will progress through `Resuming → Ready`. Without resuming the poll, the banner stays stuck at `Standby` while the model is actually waking up.

When a model has multiple pods, the UI should show the **highest-priority aggregate state**, not the lowest-level raw state:

1. `Ready`
2. `Resuming...`
3. `Standby`
4. `Restoring`
5. `Creating Snapshot...`
6. `Failed`
7. `Pending`

Optional secondary detail for admin/debug:

- `Ready`
- `1 ready pod, 1 pod creating snapshot`

### Change 3: ~~Status banner on existing func.html~~ — superseded by Change 4

~~No new page needed — augment the existing `func.html`.~~

This approach is superseded. Change 4 (new `func_user.html`) is the chosen implementation path. `func.html` is largely unchanged for inferx admins (only a "User View" toggle link is added). The status display, auto-refresh, and polling logic described in Change 2 are all implemented in `func_user.html`, not `func.html`.

### Change 4: New user-facing model detail page (`func_user.html`)

Rather than augmenting the existing admin page, create a dedicated `func_user.html` and keep the current `func.html` largely unchanged for inferx admins (the only addition is a "User View" toggle link). Route based on a unified permission check and `view=user` query param.

**Permission model — split by responsibility:**

Edit/Delete visibility uses `isAdmin`, which comes from `func["isAdmin"]` (`app.py:5747`), set by `IsNamespaceAdmin()` in the gateway (`http_gw.rs:1366`). `HasNamespaceAdminPermission()` at `auth_layer.rs:359` already returns true for inferx admins, tenant admins, and namespace admins — so `isAdmin` is the correctly scoped superset for model management within that tenant/namespace.

Admin-view routing uses `is_inferx_admin_user()`, so only inferx admins can land on `func.html`. Tenant admins, namespace admins, and regular users always render `func_user.html`.

Do **not** copy the list-page `can_manage_models` check (`app.py:5642`) for the detail page. That check is unscoped (any tenant/namespace role anywhere), so it would show Edit/Delete to a user who is admin in some other namespace but not this one. `isAdmin` from the gateway is already correctly scoped to the model's own tenant/namespace.

**Post-deploy navigation must target the user view:**
Inferx admins would otherwise land on `func.html` by default, missing the readiness page entirely. Post-deploy navigation (Change 1) must add `view=user` to the redirect URL for all three deploy paths so every deployer, including inferx admins, lands on the readiness page first.

The backend already produces redirect URLs via `dashboard_href("prefix.GetFunc", tenant=..., namespace=..., name=...)` (`app.py:5011`), which generates `/func?tenant=...&namespace=...&name=...`. Set `view=user` via `URLSearchParams` in all three paths — do not use string concatenation, as the URL already contains query params.

**Routing logic in `GetFunc`:**
```python
view = request.args.get("view", "")
if is_inferx_admin and view != "user":
    return render_template("func.html", ...)
else:
    return render_template("func_user.html", ..., fails=fails)
    # fails is already fetched by GetFunc for the admin page; pass it here too
    # func_user.html shows only create_time, exit_info, and the log link
```

**Toggle links:**
- `func.html` — add a **"User View"** link for inferx admins
- `func_user.html` — add an **"Admin View"** link for inferx admins only; regular users and namespace/tenant admins stay on the user-facing page

No new route needed. The base URL (`/func?tenant=...&namespace=...&name=...`) always routes correctly for non-admins. Inferx admins can bookmark either view; all other users always land on `func_user.html`.

**Page layout:**

```
┌──────────────────────────────────────────────────────────────────┐
│  MODEL INFO TABLE                                                │
│  ┌────────────┬───────────┬────────┬──────────────────────────┐ │
│  │ Model Name │ GPU Count │  vRAM  │          Status          │ │
│  ├────────────┼───────────┼────────┼──────────────────────────┤ │
│  │ my-model   │     1     │ 80 GB  │         Standby          │ │
│  │            │           │        │          ← poll          │ │
│  │            │           │        │  (Failed → link to logs) │ │
│  └────────────┴───────────┴────────┴──────────────────────────┘ │
│  [short state note under table; text changes with status]       │
│  [Edit]  [Delete]  (if isAdmin)                                  │
├──────────────────────────────────────────────────────────────────┤
│  PODS TABLE  (shown always; full row set reconciled on poll)     │
│  ┌──────────────────────────────────┬──────────────────────────┐ │
│  │ Pod Name (link to pod detail)    │ State          ← poll    │ │
│  └──────────────────────────────────┴──────────────────────────┘ │
├──────────────────────────────────────────────────────────────────┤
│  INFERENCE PLAYGROUND  (shown for all non-terminal states; Go    │
│  stays disabled until standby or ready; stays mounted while a    │
│  request is in flight, even if status moves to resuming)         │
│                                                                  │
│  [when not ready: "Model is not ready to serve requests yet."]   │
│                                                                  │
│  [Prompt textarea / image URL / audio URL]                       │
│  [Go]  [Cancel]                                                  │
│  Start latency: _ ms  TTFT: _ ms  TPS: _                        │
│  [Output area]                                                   │
├──────────────────────────────────────────────────────────────────┤
│  SAMPLE REST CALL  [Copy]                                        │
├──────────────────────────────────────────────────────────────────┤
│  LOGS TABLE  id="logs"  (shown always; trimmed columns)          │
│  [when health=Fail: "Your model encountered an error —           │
│   check the entries below for details."]                         │
│  ┌─────────────────┬─────────────────┬────────────────────────┐  │
│  │   Create Time   │    Exit Info    │          Log           │  │
│  └─────────────────┴─────────────────┴────────────────────────┘  │
├──────────────────────────────────────────────────────────────────┤
│  ▶ Model Spec  (collapsed <details>, read-only)                  │
└──────────────────────────────────────────────────────────────────┘
```

**Poll data source:**
The poll calls `/proxy/function/{tenant}/{namespace}/{name}/` via the existing generic proxy (`app.py:5129`), which forwards the path verbatim to the gateway — so `/proxy/function/...` reaches the gateway's `/function/...` endpoint, the same one the server-side page uses via `getfunc_response()` (`app.py:1777`). This returns the full `FuncDetail` JSON (func object + pods array). No new backend endpoint is needed.

Note: `/proxy/object/function/...` would be wrong — the proxy appends the path directly to `apihostaddr` (`app.py:5147`), so that would hit `/object/function/...` on the gateway, not `/function/...`.

**Auto-refresh scope:**
The poll interval (every 5s) updates only:
1. The **Status** cell — inferred from pod states plus function failure state via `infer_model_status()`
2. The **full pods table row set** — snapshot pods disappear as they transition through `Snapshoted → Terminating → Terminated → Cleanup → Deleted` (`scheduler_handler.rs:784`, `pod_agent.rs:990`), and restore pods appear in their place. Polling must re-render the entire pod list, not just update existing state cells.
3. Visibility of the inference playground section (see rules below)

`GetFuncDetail` (`gw_obj_repo.rs:890`) does not return fail logs — they are fetched separately via `GetFailLogs()` at server render time (`app.py:5751`) and are not available from `/proxy/function/...`. The logs table is updated via a one-shot fetch on the `Normal → Fail` transition. Once health is `Fail` the model is in a terminal error state for the current version; no new fail records will arrive, and a re-deploy creates a new version that triggers a full page reload anyway:

```js
// inside poll callback
if (prevHealth !== 'Fail' && newHealth === 'Fail') {
    fetchAndRenderLogs();  // one-shot; no scroll
}
prevHealth = newHealth;
```

Do **not** auto-scroll on failure — the user may have an in-flight inference request and scrolling away from the output area would be disorienting. Instead, render the `Failed` status badge as a visually highlighted `<a href="#logs">Failed ↓</a>` link so the user can navigate when ready.

Treat terminal poll failures as terminal UI states too. If `/proxy/function/...` returns `401`, `403`, `404`, or `410`, stop polling, show `Unavailable` in the status cell, and surface a short inline message (`session expired`, `access removed`, or `model no longer available`) instead of retrying forever against a terminal condition.

`fetchAndRenderLogs()` calls a new lightweight endpoint. The endpoint must mirror the two guards applied by the existing `/failpod` route (`app.py:5961`) and `GetFunc` (`app.py:5759-5761`):

```python
# new route in app.py
@prefix_bp.route("/faillogs")
@require_login_unless_gateway_aligned_anonymous_enabled
def GetFailLogsJson():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")
    version = request.args.get("version")

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    local_tz = pytz.timezone("America/Los_Angeles")
    fails = GetFailLogs(tenant, namespace, name, version)
    for a in fails:
        dt = datetime.fromisoformat(a["createtime"].replace("Z", "+00:00"))
        a["createtime"] = dt.astimezone(local_tz).strftime("%Y-%m-%d %H:%M:%S")
    return jsonify(fails)
```

This keeps public-tenant behaviour consistent with `/failpod` and ensures timestamps match those shown on initial page load.

Everything else (model name, GPU count, vRAM, sample REST call, spec) is static and does not re-render.

**Playground visibility rules:**
- Show for all non-terminal states so the prompt area does not jump in and out during polling.
- The inferred status codes align with the displayed labels: `standby` shows `Standby`, and `resuming` shows `Resuming...`.
- Before the model reaches `standby` or `ready`, keep the prompt visible but disable `Go` and `Cancel`, and show a short inline note: `Model is not ready to serve requests yet.`
- **Once a request is in flight, keep the playground mounted and visible regardless of status transitions.** The inference code depends on `button`, `cancel`, `output`, `startDiv`, `ttftDiv`, and `tpsDiv` remaining in the DOM throughout the request (`func.html:503`, `func.html:610`). If the poll fires during a `Resuming...` transition and hides the playground, the in-flight request loses its output target and cancel control.
- Concretely: track an `inferenceInFlight` boolean; keep the section mounted, enable `Go` only for `standby` / `ready`, and let `Cancel` stay enabled only while a request is actually in flight.

Polling stops at `ready`, `failed`, or terminal `Unavailable`. Pauses at `standby`; resumes when user clicks Go from `standby`. In the current implementation, a standby wake-up click opens a bounded resume window (20 seconds) and schedules the first follow-up poll after 1 second so the page can observe `Resuming → Ready` without wasting an immediate no-op poll. If the frontend later observes first output during that standby wake-up cycle, it triggers one immediate status poll and then suppresses the normal interval polling for that cycle. If the model settles back to `standby` without progressing, polling pauses again instead of continuing forever. The logs fetch is a one-shot triggered by the `Normal → Fail` transition — it does not run on a separate interval.

**Polling lifecycle and perf impact:**
- Polling exists only on `func_user.html`. The admin page `func.html` does not poll unless an inferx admin explicitly switches to **User View**.
- On initial user-page render, the page calls `schedulePoll(5000)`, but a timer is only armed if the current state is transient.
- Polling starts automatically only when the inferred state is transient (`Pending`, `Restoring`, `Resuming...`, `Creating Snapshot...`).
- Polling does **not** run while the model is idle in `Ready` or `Standby`.
- `Standby` is intentionally quiet by default to avoid background load from dormant models.
- When the user clicks **Go** from `Standby`, the page resumes polling after a 1-second delay and opens a bounded 20-second resume window so it can observe `Resuming → Ready`.
- If the standby wake-up request produces first output before the next 5-second tick, the page fires one immediate status poll and then stops the interval polling loop for that standby wake-up cycle.
- If the model falls back to `Standby` and stays there after that 20-second window, polling pauses again automatically.
- Polling stops immediately when the page reaches a terminal UI state: `Ready`, `Failed`, or terminal `Unavailable`.
- Terminal proxy responses (`401`, `403`, `404`, `410`) are treated as terminal states; the page shows an inline `Unavailable` message and stops polling rather than retrying forever.
- Transient model states (`Pending`, `Restoring`, `Resuming...`, `Creating Snapshot...`) show a persistent animated spinner inside the Status badge so the page still feels live between poll intervals.
- Each poll is a single `GET /proxy/function/{tenant}/{namespace}/{name}/` request and only re-renders the status cell, pods table, and playground visibility.
- The renderer skips no-op DOM writes when the inferred status, pods rows, logs rows, or section visibility did not actually change, which reduces repaint/reflow churn without changing polling semantics.
- Fail logs are **not** polled continuously. The page performs at most one extra `/faillogs` fetch on the `Normal → Fail` transition.
- Overlap is blocked with a `pollInFlight` guard, so one tab cannot stack concurrent poll requests against the same page state.

This means the steady-state cost per open user-view tab is at most one request every 5 seconds while the model is transient, plus at most one extra immediate status poll when first output is observed, plus at most one extra `/faillogs` fetch on failure. Idle `Ready` and `Standby` pages generate no background polling traffic.

**Section-by-section rationale:**

| Section | Include | Notes |
|---|---|---|
| Model name | Yes | Static |
| GPU count + vRAM | Yes | Static; cost/billing awareness |
| Status (inferred) | Yes | Polls every 5s while transient; also carries terminal `Failed` / `Unavailable` states, and `Failed` is rendered as a link to `#logs` |
| Model state note | Yes | Short inline explanation under the Model Info table; text changes with the inferred status (`Pending`, `Restoring`, `Standby`, `Resuming...`, `Ready`, `Creating Snapshot...`, `Failed`, `Unavailable`) |
| Edit / Delete | Yes, gated on `isAdmin` | Static; `isAdmin` is already model-scoped via gateway |
| Pods table (name + state) | Yes | Full row set reconciled on each poll (rows can appear/disappear); pod name links to existing pod detail page |
| Inference playground | Yes, shown for all non-terminal states; `Go` enabled only at `standby` / `ready`; `Cancel` enabled only while a request is in flight | Cannot be a simple copy of `streamOutput()` — see JS extraction note below |
| Start latency + TTFT + TPS | Yes | `tcpconn_latency_header` is resume latency; already at `func.html:606-613` |
| Sample REST call + copy | Yes | Already at `func.html:124-130` |
| Model spec | Yes, read-only collapsed `<details>` | `funcspec` only (not `full_funcspec`); no `updateSpec()` button |
| Logs table | Yes, trimmed columns | Show `create time`, `exit info`, `log link` only — omit `id`, `node name`, `revision`, `tenant`, `namespace`; add `id="logs"` anchor; show contextual note when health is `Fail`. Populated at page load; refreshed once via `/faillogs` on `Normal → Fail` transition (no further polling needed — `Fail` is terminal for the current version). The log link points to `/failpod` → `pod.html`; both detail pages pass `funcname` in the query string, and `GetFailPod` reads `request.args.get("funcname") or name` so the model back-link resolves correctly. `/failpod` audit timestamps are formatted the same way as `/pod`. |
| Snapshot history | No | Admin only |
| Policy JSON | No | Admin only |
| Full spec JSON | No | Admin only (`is_inferx_admin`) |

**Inference JS extraction note:**
`streamOutput()` and its helpers (`streamOutputText`, `streamOutputImage`, `streamOutputAudio`) at `func.html:493-883` are tightly coupled to inline globals (`tenant`, `namespace`, `name`, `apiType`, `map`, `sampleQueryPath`, `funcconfig`, `funcEditData` at `func.html:360-367`) and to specific DOM element IDs (`output`, `debug`, `prompt`, `urlInput`, `button`, `cancel`, `processing`, `go`, `tpsDiv`, `ttftDiv`, `startDiv`). `func_user.html` must either:
- (a) Extract these functions into a shared JS file (e.g. `static/inference.js`) that accepts config as parameters, included by both templates, or
- (b) Duplicate the script block and ensure all required DOM IDs are present in `func_user.html`

Option (a) is cleaner for long-term maintenance; option (b) is faster to ship but creates drift. Either way the full DOM element set and all globals must be explicitly declared in `func_user.html`.

---

## Model List Page

`FuncBrief` in `gw_obj_repo.rs` does not include pod-level data — only `snapshotNodes`. To show meaningful status in the list view, a backend change is needed to include a pod state summary (e.g., `readyPodCount`, `totalPodCount`) in `FuncBrief`.

This is worthwhile but a **phase 2** item. The post-deploy navigation change addresses the immediate confusion first.

---

## Residual Risks

**Normal pods not covered by the current status model**
The `infer_model_status` sketch explicitly ignores `Normal` pods and is optimized for the snapshot/restore workflow. If `Normal` pods (cold-start, no snapshot) re-enter the user path, the mapping will need another pass — a `Normal` pod progressing through `Init → Loading → Ready` without a `create_type` of `Snapshot` or `Restore` would fall through to the `"Preparing"` fallback rather than showing meaningful progress.

**Polling resume is still tied to the dashboard inference button**
The current implementation adds a bounded resume window after the page's own Go button is clicked, which covers page-initiated wake attempts. However, if the model is woken by a request sent from outside the page (e.g. API call, `ixctl`, another browser tab), the banner can still remain stuck at `Standby` until the user manually reloads. A broader signal from the backend would be needed to eliminate that gap completely.

---

## Implementation Priority

| Priority | Change | Effort |
|---|---|---|
| P0 | Navigate to model page after deploy — all three paths must set `view=user` query param via `URLSearchParams` so all deployers land on the readiness page | Small |
| P1 | New `func_user.html`: status in top table (auto-refresh), pod table (full row reconcile on poll), inference playground shown through non-terminal states with request controls enabled only at `standby`/`ready` | Medium |
| P2 | Add pod state summary to `FuncBrief` for list view | Medium (backend) |

---

## Implementation Checklist

### Routing and backend

- [ ] Update `GetFunc` in `dashboard/app.py` to render `func_user.html` for `view=user` or any non-inferx-admin viewer, and pass `fails=fails` into that template.
- [ ] Add a lightweight `/faillogs` JSON route in `dashboard/app.py`.
- [ ] Mirror the existing public-tenant guard in `/faillogs` via `deny_public_tenant_request(tenant)`.
- [ ] Mirror the existing `createtime` timezone formatting in `/faillogs` so refreshed rows match initial page render.
- [ ] Pass the real function name through `/failpod` links and have `GetFailPod` read `request.args.get("funcname") or name` before rendering `pod.html`.

### Post-deploy navigation

- [ ] Update `dashboard/templates/func_create.html` create/update success flow to navigate to the model page with `view=user`.
- [ ] Update `dashboard/templates/catalog_detail.html` deploy success flow to navigate to `redirect_url` with `view=user`.
- [ ] Update `dashboard/templates/catalog_list.html` deploy success flow to navigate to `redirect_url` with `view=user`.
- [ ] Use `URL` / `URLSearchParams` in all three deploy paths instead of string concatenation.

### User model page

- [ ] Create `dashboard/templates/func_user.html`.
- [ ] Render model summary fields: model name, GPU count, vRAM, and inferred status.
- [ ] Render a trimmed pods table with pod name and pod state only.
- [ ] Render admin-only `Edit` / `Delete` actions when `isAdmin` is true.
- [ ] Render the sample REST call and copy affordance.
- [ ] Render the read-only filtered model spec (`funcspec`) in collapsed form.
- [ ] Render a trimmed logs table with `create time`, `exit info`, and `log` link only.
- [ ] Add `id="logs"` anchor on the logs section.
- [ ] Render `Failed` status as a link to `#logs`.
- [ ] Add an inferx-admin-only `User View` toggle link in `func.html`.
- [ ] Add an inferx-admin-only `Admin View` toggle link in `func_user.html`.

### Polling and status logic

- [ ] Poll `/proxy/function/{tenant}/{namespace}/{name}/` every 5 seconds while the model is transient.
- [ ] Implement aggregate model-state precedence: `ready` > `resuming` > `standby` > `restoring` > `snapshotting` > `failed`, with `pending` reserved for the generic fallback; keep internal enum references aligned with `Snapshoting`, and keep `Normal` pods as an explicit fallback / out-of-scope case for the current workflow.
- [ ] Re-render the full pods table row set on each poll rather than only patching state text.
- [ ] Update inferred status and internal fail state from poll results.
- [ ] Pause polling at `standby`.
- [ ] Resume polling when the user clicks Go from `standby`.
- [ ] Trigger one immediate status poll when the standby wake-up request returns first output, then stop interval polling for that wake-up cycle.
- [ ] Stop polling at `ready`, `failed`, or terminal `Unavailable`.
- [ ] Bound the post-Go resume polling window so a canceled or failed wake attempt does not poll forever if the model returns to `standby`.
- [ ] Surface terminal poll errors (`401/403/404/410`) as an inline `Unavailable` state instead of retrying forever.
- [ ] Keep the inference playground mounted while a request is in flight, even if status changes during polling.
- [ ] Trigger a one-shot `/faillogs` fetch on `Normal → Fail`.

### Inference UI wiring

- [ ] Extract shared inference JS from `func.html` into reusable code, or duplicate it carefully into `func_user.html`.
- [ ] Ensure the required DOM IDs remain present on the user page: `button`, `cancel`, `output`, `prompt`, `urlInput`, `go`, `processing`, `startDiv`, `ttftDiv`, and `tpsDiv`.
- [ ] Preserve resume-latency / TTFT / TPS display behavior on the user page.

### Verification

- [ ] Verify deploy redirect from all three entry points: create page, catalog detail, and catalog list.
- [ ] Verify the user-facing lifecycle flow: `Restoring` -> `Standby` -> `Resuming...` -> `Ready`.
- [ ] Verify snapshot pods disappear and restore pods appear correctly during polling.
- [ ] Verify the logs table is populated on initial page load.
- [ ] Verify the one-shot `/faillogs` fetch updates the logs table on `Normal → Fail`.
- [ ] Verify the accepted limitation: fail records that arrive after the one-shot fetch require manual page refresh.
- [ ] Verify `/failpod` back-link returns to the model page correctly.
- [ ] Verify inferx admins can switch between admin view and user view.
