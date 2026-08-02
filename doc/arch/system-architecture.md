# InferX System Architecture

## 1. Overview

InferX is a GPU-accelerated serverless inference platform that manages LLM (Large Language Model) serving containers with checkpoint/restore (C/R), GPU memory snapshotting, live GPU migration, and multi-tenant billing. The system is built as a set of cooperating Rust services backed by etcd for distributed state and PostgreSQL for audit/billing.

### 1.1 Services

| Service | Port | Responsibility |
|---------|------|---------------|
| **StateSvc** | 1237 | Central metadata authority: CRUD for tenants, namespaces, functions, nodes, policies. Backed by etcd. Aggregates per-node state. |
| **Scheduler** | 1238 | Leader-elected scheduling: pod placement, snapshot creation, standby maintenance, GPU resource allocation. |
| **Gateway** | 4000 (HTTP/TLS) | External API: function calls, skill chains, billing, RBAC, onboarding, admin. |
| **NodeAgent (na)** | 1233 (gRPC) | Per-node pod lifecycle, GPU memory, snapshots, TSOT networking, NVMe storage. |

### 1.2 Shared Libraries

| Crate | Purpose |
|-------|---------|
| `ixshare` | Core infrastructure: etcd client, metadata store (cacher/informer/watch), peer networking, configuration, audit/billing, scheduler, state service, gateway, protobuf types. |
| `inferxlib` | Domain model: `DataObject<T>` envelope, object managers (func, pod, node, tenant, etc.), GPU resource types, label selectors, validation. |

### 1.3 External Dependencies

- **etcd** — distributed key-value store for cluster state and leader election
- **PostgreSQL** — audit logs, billing, API keys, skills, tenant profiles
- **Keycloak** — JWT-based authentication and RBAC
- **NVIDIA CUDA 13.3** — GPU checkpoint/restore API, VMM, cuFile (GDS)
- **CRIU 4.2** — process checkpoint/restore
- **SPDK** — user-space NVMe driver for snapshot storage
- **Podman/runc** — container runtime

---

## 2. System Architecture

```
                              External Clients
                                    |
                                    v
                    +-------------------------------+
                    |        Gateway (4000)         |
                    |  HTTP/TLS, Auth, Skills,     |
                    |  Billing, RBAC, Onboarding   |
                    +---------------+---------------+
                                    |
                          LeaseWorker / ReturnWorker
                                    |
                                    v
                    +-------------------------------+
                    |      Scheduler (1238)         |
                    |  Leader-elected              |
                    |  Pod placement, snapshots,   |
                    |  standby maintenance         |
                    +---------------+---------------+
                           |              |
              gRPC         |              | watches etcd
                           v              v
                    +---------------+-----------+
                    |  StateSvc     |   etcd    |
                    |  (1237)       | (cluster  |
                    |  Metadata CRUD|  state)   |
                    |  Admission    |           |
                    |  Node aggreg. |           |
                    +-------+-------+-----------+
                            |
                   gRPC (IxMetaService)
                            |
              +-------------+-------------+
              |             |             |
              v             v             v
        +-----------+ +-----------+ +-----------+
        | NodeAgent | | NodeAgent | | NodeAgent |
        |  (1233)   | |  (1233)   | |  (1233)   |
        | Pod Mgr   | | Pod Mgr   | | Pod Mgr   |
        | GPU Mem   | | GPU Mem   | | GPU Mem   |
        | TSOT Net  | | TSOT Net  | | TSOT Net  |
        | NVMe/SPDK | | NVMe/SPDK | | NVMe/SPDK |
        +-----------+ +-----------+ +-----------+
              |
        Podman + CRIU + nvproxy
              |
         vLLM containers
```

---

## 3. Domain Model (inferxlib)

### 3.1 DataObject<T> — The Universal Envelope

Every cluster object is a `DataObject<SpecType>` with:
- `objType` — object kind (e.g., "function", "pod", "node")
- `tenant`, `namespace`, `name` — hierarchical key
- `labels`, `annotations` — metadata for selection
- `channelRev`, `srcEpoch`, `revision` — version tracking
- `object: SpecType` — the typed payload

Key format: `{tenant}/{namespace}/{name}`. Store key: `{objType}/{tenant}/{namespace}/{name}`.

### 3.2 Object Types

| Object | Key | Spec | Purpose |
|--------|-----|------|---------|
| **Function** | `function` | `FuncObject` (spec + status) | Model-serving definition: image, endpoints, resources, standby, sample call, policy ref |
| **FuncPod** | `pod` | `FuncPodObject` (spec + status) | Running pod instance: containers, allocated resources, GPU slots, state |
| **Node** | `node_info` | `NodeSpec` | Node identity: IP, ports, CIDR, GPU resources, NA state |
| **NodeStatus** | `node` | `NodeStatusSpec` | Observed node status: addresses, capacity, conditions |
| **Tenant** | `tenant` | `TenantObject` (spec + status) | Tenant with resource limits (maxFunc, maxGpu, maxReplica, etc.) |
| **Namespace** | `namespace` | `NamespaceObject` | Namespace within a tenant (with disable flag) |
| **FuncPolicy** | `funcpolicy` | `FuncPolicySpec` | Scaling policy: minReplica, maxReplica, standbyPerNode, queueLen, scaleOut/in |
| **FunctionStatus** | `funcstatus` | `FunctionStatusDef` | Function runtime status: published, state, failure counters |
| **ContainerSnapshot** | `snapshot` | `ContainerSnapshot` | GPU memory snapshot: sizes, standby type, state, node |

### 3.3 GPU Resource Model

GPU memory is allocated in **256 MB slots** (`GPU_SLOT_SIZE`). The `GPUResourceMap` tracks:
- `totalSlotCnt` — total slots across all GPUs
- `map: BTreeMap<i32, GPUAlloc>` — per-GPU: slotCnt, contextCnt, ncclCnt
- `slotSize` — bytes per slot

`Resources` describes a pod's request: CPU (millicores), memory (MB), cacheMemory (MB), GPU (type, count, vRAM).

`NodeResources` describes a node's capacity: CPU, memory, cacheMemory, gpuType, `GPUResourceMap`, maxContextCnt.

Allocation uses greedy first-fit across GPUs checking slot, context, and NCCL availability.

### 3.4 Pod Lifecycle States

```
Init -> PullingImage -> Creating -> Created -> Loading -> Ready
                                                        |
                    +-----------------------------------+
                    |                 |                 |
              Standby           Terminating        Working (GatewayId)
                    |                 |
              Resuming           Terminated
                    |
              ResumeDone -> Ready
```

---

## 4. State Service (state_svc/)

### 4.1 Purpose

The State Service is the **central metadata authority**. It persists cluster objects to etcd, serves a gRPC API (`IxMetaService`) for CRUD and watch operations, performs admission validation, and aggregates per-node state from all NodeAgents.

### 4.2 Architecture

```
              gRPC Clients (Gateway, Scheduler, NodeAgents)
                              |
                              v
                    +-------------------+
                    |    StateSvc       |
                    |  (IxMetaService)  |
                    +---+---+---+---+---+
                        |   |   |   |
            +-----------+   |   |   +-----------+
            |               |   |               |
            v               v   v               v
      Admission        EtcdStore  SvcDir    IxAggrStore
      (validation)    (persist)  (cachers)  (node aggreg.)
            |               |       |           |
            v               v       v           v
      TenantMgr        etcd    CacheStore   AggregateClient
      FuncMgr                   (per type)   (per node)
      NamespaceMgr
```

### 4.3 Components

#### 4.3.1 StateSvc (state_svc.rs)

The core struct holding:
- `svcDir: SvcDir` — directory of `CacheStore` instances (one per object type)
- `store: EtcdStore` — etcd persistence backend
- `factory: InformerFactory` — watches for Tenant/Namespace/Function changes
- `tenantMgr`, `funcMgr`, `namespaceMgr` — in-memory caches
- `reqListener` — PostgreSQL LISTEN/NOTIFY for request audit

**gRPC methods (IxMetaService):**
- `create` — admission check -> etcd Create -> side effects (create FunctionStatus)
- `update` — admission check -> etcd Update (optimistic concurrency) -> side effects
- `delete` — admission check -> etcd Delete -> side effects (delete FunctionStatus)
- `get` — read from CacheStore (in-memory)
- `list` — list from CacheStore with label/field selectors
- `watch` — stream DeltaEvents from CacheStore
- `uid` — allocate cluster-unique ID via etcd revision
- `version` — return service version

**gRPC methods (ReqWatchingService):**
- `watch` — stream request audit events from PostgreSQL LISTEN/NOTIFY

**Bootstrap (`StateService()`):**
1. Create `StateSvc` with etcd connection
2. Create `CacheStore` per object type (etcd-backed): Node, Namespace, Function, FunctionStatus, Tenant, FuncPolicy, SchedulerInfo
3. Create `IxAggrStore` for node-agent aggregation; register Pod/Node/Snapshot caches in SvcDir
4. Create `StateSvcRegister` for self-registration in etcd
5. Run concurrently: gRPC server, node aggregation, self-registration, informer processing

#### 4.3.2 Admission Control (admission.rs)

All mutations pass through admission checks:

| Check | Rules |
|-------|-------|
| CreateTenant | Must be in system/system; must not already exist |
| CreateNamespace | Reject reserved "endpoints" namespace; parent tenant must exist; no duplicates |
| CreateFunc | Reject reserved namespace; parent namespace must exist; tenant func count < maxFuncCnt; no duplicates; validate inline policy; check mem-standby permission |
| CreateFuncPolicy | Validate tenant resource limits (minReplica cap, maxReplica, maxStandby, maxQueueLen) |
| UpdateTenant | Must exist; must be in system/system |
| UpdateFunc | Must exist; validate parent namespace; validate inline policy |
| Delete | Tenants and namespaces cannot be deleted; functions can be deleted if they exist |

**Side effects:**
- Creating a Function auto-creates a companion `FunctionStatus` (version, published flag, Normal state, zero counters)
- Updating a Function updates its FunctionStatus (with 3-retry optimistic concurrency loop)
- Deleting a Function deletes its FunctionStatus

#### 4.3.3 Node Agent Aggregation (IxAggrStore.rs)

Aggregates per-node state (pods, node status, snapshots) from each NodeAgent's state service:

1. Runs a `Node` informer watching etcd for node additions/removals
2. For each node, creates an `IxAgent` with three `AggregateClient`s (node, pod, snapshot)
3. Each `AggregateClient` connects to the node's state service (`http://{nodeIp}:{stateSvcPort}`) and streams data into shared `CacheStore`s
4. `WaitlistDone()` blocks until initial node list is processed (with 500ms grace period)
5. Node sync is deliberately delayed 100ms to let pod/snapshot caches populate first (avoids premature snapshot pod creation)

**Two-phase behavior:**
- During init (before `listDone`): accumulate nodes, wait for `InitDone`
- After init: real-time add/remove of agents as nodes join/leave

#### 4.3.4 Self-Registration (statesvc_register.rs)

Registers the StateSvc instance in etcd with a 20-second lease for liveness:
- Name: `{name}_{UniqueId}` (cluster-unique)
- Keepalive every 500ms
- Waits for `IxAggrStore.WaitlistDone()` before registering

### 4.4 Data Flow

**Write path:**
```
Client -> gRPC create/update/delete
  -> Admission check (admission.rs)
  -> EtcdStore.Create/Update/Delete (optimistic concurrency)
  -> Side effects (FunctionStatus CRUD)
  -> etcd watch triggers CacheStore update
  -> Informer receives DeltaEvent
  -> StateSvc.ProcessDeltaEvent -> update in-memory managers
```

**Read path:**
```
Client -> gRPC get/list/watch
  -> SvcDir.GetCacher(objType)
  -> CacheStore.Get/List/Watch (from in-memory cache)
```

**Node state aggregation:**
```
NodeAgents -> per-node state services
  -> IxAgent -> AggregateClient -> CacheStore (pod/node/snapshot)
  -> SvcDir serves these caches to clients
```

---

## 5. Scheduler (scheduler/)

### 5.1 Purpose

The Scheduler is a **leader-elected** service that manages pod placement, snapshot creation, standby pod maintenance, and GPU resource allocation across nodes. It watches cluster state changes and responds to gateway requests for worker leasing.

### 5.2 Architecture

```
                    etcd (cluster state)
                          |
                    InformerFactory
                    (watches 7 object types)
                          |
                    SchedObjRepo
                    (local cache + managers)
                          |
                    SCHEDULER.ProcessDeltaEvent
                    (via mpsc channel)
                          |
                    SchedulerHandler.ProcessOnce
                          |
              +-----------+-----------+
              |                       |
         eventRx                  msgRx
         (DeltaEvents)         (WorkerHandlerMsgs)
              |                       |
    Update internal state    Process requests
    (nodes, pods, funcs,     (LeaseWorker, ReturnWorker,
     snapshots, policies)     KillPod, ConnectScheduler)
              |                       |
              +----------+------------+
                         |
                    spawn_rpc -> NodeAgent gRPC
                    (create_func_pod, resume_pod,
                     terminate_pod, remove_snapshot)
                         |
                    Completion messages -> msgRx
```

### 5.3 Components

#### 5.3.1 Scheduler (scheduler.rs)

Global singleton `SCHEDULER` with:
- `eventTx`/`eventRx` — mpsc channel (1000) for DeltaEvents
- `msgTx`/`msgRx` — mpsc channel (1000) for WorkerHandlerMsgs

**Startup (`SchedulerSvc()`):**
1. Register Prometheus metrics
2. Create `SchedObjRepo` (connects to etcd, registers 7 informers)
3. Spawn `SchedulerProcess()` — leader election + gRPC server
4. Spawn `SCHEDULER.StartProcess()` — event/message processing loop
5. Run `objRepo.Process()` (informer loop) + `SchedulerHttpSrv()` (HTTP debug on port 80)

#### 5.3.2 Leader Election (scheduler_register.rs)

Simple etcd-based leader election:
- Create "scheduler" object in etcd with 1-second lease
- On success: become leader, keepalive every 200ms
- On failure: `WaitForLeaderLoss()` (watch for deletion), then retry

#### 5.3.3 gRPC Service (scheduler_svc.rs)

Implements `SchedulerService`:
- `connect_scheduler` — gateway connection (validates idle/working pods)
- `lease_worker` — lease an idle pod for a function
- `return_worker` — return a leased pod to idle
- `refresh_gateway` — gateway heartbeat
- `kill_pod` — terminate a specific pod

All delegate to `SCHEDULER` singleton which sends messages through the internal channel.

#### 5.3.4 HTTP Debug (scheduler_http.rs)

Axum server on port 80:
- `GET /metrics` — Prometheus metrics
- `GET /debug/state` — full scheduler state as JSON
- `GET /trace/:state` — enable/disable trace logging
- `GET /` — health check

#### 5.3.5 SchedulerHandler (scheduler_handler.rs, 6345 lines)

The core scheduling engine. Maintains:
- `nodes: BTreeMap<String, NodeStatus>` — all nodes with resources, pending/active pods
- `funcs: BTreeMap<String, FuncStatus>` — function status with pods, pending pods, lease queue
- `snapshots: BTreeMap<String, BTreeMap<String, ContainerSnapshot>>` — funcid -> node -> snapshot
- `idlePods: LruCache<String, ()>` — LRU cache of idle pods
- `terminating_pods: BTreeMap<String, TerminatingMeta>` — pods being terminated
- `SnapshotSched: BiIndex<SnapshotScheduleInfo>` — bidirectional snapshot schedule index
- `funcpolicy: BTreeMap<String, FuncPolicySpec>` — cached policies
- `taskQueue: TaskQueue` — throttled task queue (50ms)
- `delayed_tasks: BinaryHeap<TimedTask>` — delayed task min-heap
- Billing sessions (snapshot + standby)
- Per-node semaphores (max 2 concurrent RPCs)

**Main loop (`ProcessOnce`):** `tokio::select!` with biased ordering:
1. `msgRx.recv()` — highest priority: process WorkerHandlerMsgs
2. `delay_interval.tick()` (100ms) — drain due delayed tasks
3. `billing_tick_interval.tick()` (60s) — emit snapshot billing ticks
4. `standby_billing_interval.tick()` (600s) — emit standby billing ticks
5. `interval.tick()` (4s) — if listDone: check warmup, refresh scheduling, clean pods/snapshots, process gateway timeouts, reconcile stuck lease requests
6. `eventRx.recv()` — process DeltaEvents
7. `taskQueue.Next()` — process scheduled tasks
8. `closeNotify.notified()` — shutdown

**Key operations:**

| Operation | Description |
|-----------|-------------|
| `ProcessLeaseWorkerReq` | Find idle Ready pod -> lease immediately. If none, try ResumePod (standby -> ready). If none, queue request for when pod becomes Ready. Enforce tenant GPU limits. |
| `ProcessReturnWorkerReq` | Return pod to idle (or terminate if failworker). Update GPU metrics. |
| `ProcessKillPod` | Kill pod by stopping it and marking as terminating. |
| `ResumePod` | Find best standby pod, allocate resources, spawn async `resume_pod` gRPC to NodeAgent. |
| `TryCreateSnapshotOnNode` | Check resources (may terminate idle pods), reserve resources, spawn async `create_func_pod` with Snapshot type. |
| `TryAdjustStandbyPodsOnNode` | Maintain standbyPerNode count: create Restore pods or terminate excess. |
| `CreateOneNvidiaPod` | Create NVIDIA-runtime pod when nvidiaReplica policy demands. |
| `RefreshScheduling` | Shuffle function IDs, call ProcessAddFunc for each until one succeeds (pre-warms pods). |
| `ReconcileNodeAfterNodeAgentRestart` | Clean transient pods, recalculate resources, sync surviving pod states. |
| `ReconcileAllStuckLeaseRequests` | Remove lease requests older than timeout. |

**Warm-up phase:** After initial list loads (`listDone = true`), scheduler waits 5 seconds for gateways to reconnect before starting normal scheduling. Prevents premature pod eviction.

**Billing:**
- **Snapshot billing**: Per pod, per node. Charges for GPU usage during snapshot loading. 60s ticks.
- **Standby billing**: Per function (model-level). $0.20/hr/GPU while snapshots exist. 600s (10min) ticks. Session tracks `gpu_type`, `gpu_count`, `vram_mb`.

### 5.4 Scheduling Logic

**LeaseWorker flow:**
```
1. Check tenant GPU limit (exempt for "inferx" and "public")
2. Find idle Ready pod for this function
   -> If found: set Working(gatewayId), return pod info
3. Check maxReplica policy
4. Try ResumePod:
   -> Find best standby pod (GetBestResumeWorker)
   -> Allocate ready resources
   -> Mark as Resuming
   -> Spawn async resume_pod RPC
   -> Queue lease request for when pod becomes Ready
5. If no standby: queue lease request
   -> When new pod becomes Ready (via UpdatePod), match to queued request
```

**Snapshot creation flow:**
```
1. Check if snapshot/pending already exists for this func+node
2. Check node readiness and blob support
3. Check resource availability (may terminate idle pods via TryFreeResources)
4. Reserve snapshot resources
5. Spawn async create_func_pod with CreatePodType::Snapshot
6. Track pending pod and pending snapshot
7. On completion: add snapshot to state, start billing
```

**Node selection (`FindNode4Pod`):**
```
1. Check direct availability on candidate nodes
2. If insufficient: simulate killing idle pods
3. Respect minReplica policy (don't reduce below minimum)
4. Return (nodename, pods_to_terminate, allocated_resources)
```

---

## 6. Gateway (gateway/)

### 6.1 Purpose

The Gateway is the **external API entry point**. It handles HTTP/TLS requests, authenticates users via Keycloak JWT or API keys, routes function calls to worker pods, manages skills (multi-turn LLM chains), handles billing/credits, RBAC, and onboarding.

### 6.2 Architecture

```
Client Request
    |
    v
[auth_layer.rs] -- Keycloak JWT / API key -> AccessToken
    |
    v
[TenantQuotaGuard] -- billing quota check
    |
    v
[http_gateway.rs Router] -- ~80+ Axum routes
    |
    +-- /funccall/* --> FuncCall1() --> dispatch_func_call()
    |       |-- resolve_funccall_target() [gw_obj_repo.rs]
    |       |-- RetryGetClient() [func_agent_mgr.rs]
    |       |-- QHttpCallClient [func_worker.rs]
    |       |-- Streaming response with TTFT tracking
    |
    +-- /modelcall/* --> FuncCallWithTokenTracking() [req_token.rs]
    |
    +-- /skills/* --> SkillCall() --> handle_skill_call_chain() [skill_chain.rs]
    |
    +-- /mcp --> McpStreamServer [mcp_stream_server.rs]
    |
    +-- /object/* --> CRUD [http_gw.rs]
    |
    +-- /admin/* --> billing, tenants, endpoints
    |
    +-- /rbac/* --> RBAC grant/revoke
    |
    +-- /apikey/* --> API key management
    |
    +-- /tokenizer/* --> Token counting
    |
    +-- /metrics --> Prometheus
```

### 6.3 Components

#### 6.3.1 Authentication (auth_layer.rs)

Axum middleware that validates:
- **Keycloak JWT bearer tokens**: decodes JWT, extracts subject, username, scope, roles, tenant/namespace restrictions
- **API keys**: looks up in PostgreSQL via `SqlSecret`, validates hash, extracts associated tenant/namespace restrictions

Populates `Arc<AccessToken>` in request extensions for downstream handlers.

Public paths (e.g., health, onboarding) bypass authentication.

#### 6.3.2 Object Repository (gw_obj_repo.rs)

`GW_OBJREPO` — global singleton using Kubernetes-style informers to maintain in-memory caches of:
- `funcMgr` — Function objects
- `funcstatusMgr` — FunctionStatus objects
- `tenantMgr` — Tenant objects
- `namespaceStore` — Namespace objects

Watches the state service for changes via `InformerFactory`. Provides:
- `GetFunc()`, `GetFuncPod()`, `ListReadyPods()`
- `FuncPolicy()` — routing policy for a function
- `EndpointRoutePolicy()` — policy for virtual endpoints
- `GetNodes()`, `GetNode()`

#### 6.3.3 Function Agent Manager (func_agent_mgr.rs)

Manages function routing and worker connections:
- `FuncAgent` — per-function agent with HTTP connection pooling
- `FuncRouteTarget` — routes (tenant, namespace, funcname) to physical function identity
- `EndpointLeaseLimiter` — limits concurrent endpoint leases per tenant
- `GetClient()` — gets `QHttpCallClient` for a route, with retry/timeout

#### 6.3.4 Function Worker (func_worker.rs)

HTTP connection pooling and direct TCP tunneling:
- `QHttpCallClient` — pooled keepalive connections to worker pods (via hyper)
- `QHttpCallClientDirect` — direct TCP-tunneled connection using `IxTcpClient` (bypasses pooling for TSOT)
- `HttpSender` — low-level HTTP request sender

#### 6.3.5 HTTP Gateway (http_gateway.rs, ~6750 lines)

The main gateway server with ~80+ Axum routes:

**Function calls:**
- `FuncCall1()` — main entry: parse path, validate scope, resolve route, dispatch
- `dispatch_func_call()` — core dispatch: normalize request, retry loop, TTFT tracking, streaming response, metrics, client disconnect handling
- `DirectFuncCall()` — direct TCP-tunneled call to specific pod

**Skills:**
- `SkillCall()` — skill invocation: load from DB, resolve tenant, check quota, route to dispatch or skill chain
- Skill CRUD: `CreateSkill`, `GetSkill`, `DeleteSkill`, `PublishSkill`, `UnpublishSkill`
- Skill templates: CRUD + activate/deactivate
- Skill subscriptions: CRUD
- Skill marketplace: listing by published/tenant/namespace

**Billing:**
- `AddTenantCredits`, `GetTenantCredits`, `GetTenantCreditHistory`
- `GetTenantBillingSummary`, `GetTenantHourlyUsage`, `GetTenantUsageByModel`
- `AddBillingRate`, `GetBillingRateHistory`
- `SetTenantQuotaExceeded`

**Admin:**
- `Onboard` — create tenant + API key for new users
- `GetAdminTenants` — list all tenants
- `GetAdminEndpointUsage` — cross-tenant endpoint usage

**RBAC:**
- `RbacGrant`, `RbacRevoke`, `RbacTenantUsers`, `RbacNamespaceUsers`

**Pod management:**
- `GetFuncPods`, `GetFuncPod`, `KillPod`, `ReadLog`, `ReadPodAuditLog`

**Key constants:**
- `FUNCCALL_MAX_BODY_BYTES = 20MB`
- `VIRTUAL_ENDPOINTS_NAMESPACE = "endpoints"`
- `SKILLS_NAMESPACE = "skills"`, `SKILLS_ROOT_DIR = "/opt/inferx/skills"`

#### 6.3.6 Skill Chain (skill_chain.rs, ~3204 lines)

Multi-turn LLM conversation orchestration with parallel child skill tool calls:

- `execute_skill_chain()` — main loop: build request, dispatch to model, parse tool calls, execute parallel child calls, aggregate usage, repeat until no tool calls or max depth (5)
- `handle_skill_call_chain()` — entry point: sets up SSE trace streaming, cancellation, spawns execution
- `execute_parallel_child_call()` — executes a single child skill: validate skillep_id, check allowlist, dispatch HTTP call, read child trace SSE
- SSE trace events: `skill_trace`, `skill_result`, heartbeat, done

Tool definition: `call_skillep` with `skillep_id` and `query` arguments. The LLM must use this tool to invoke child skills.

#### 6.3.7 Scheduler Client (scheduler_client.rs)

`SCHEDULER_CLIENT` — global gRPC client to the Scheduler:
- `LeaseWorker()` — lease a worker pod for a function
- `ReleaseWorker()` — return a leased worker
- `KillPod()` — kill a pod via scheduler

#### 6.3.8 Secret Store (secret.rs)

PostgreSQL-backed store for:
- API keys (CRUD, hash validation)
- Tenant profiles (display name, email)
- Skills (full CRUD, marketplace listing)
- Skill templates (CRUD, activation)
- Skill subscriptions (CRUD, alias updates)
- Endpoint metadata (slug, published status)

#### 6.3.9 Metrics (metrics.rs)

Prometheus + OpenTelemetry:
- `GATEWAY_METRICS` — funccall count (by tenant/namespace/funcname/status), TTFT histograms (keepalive + cold start), cold start counter
- `SCHEDULER_METRICS` — GPU usage (total/used), pod leases, cold start latency
- `METRICS_REGISTRY` — global Prometheus registry
- `InitTracer()` — OpenTelemetry/Jaeger setup

#### 6.3.10 MCP Stream Server (mcp_stream_server.rs)

Model Context Protocol server using `rmcp` crate:
- Tool calls, completions
- Cancellation token registry for request cancellation
- Integrates with skill chain for streaming

#### 6.3.11 Tokenizer (tokenizer.rs)

- `TokenizerRoute()` — token counting endpoints
- `ModelsFuncCall()` — model listing/calling
- `NormalizeFuncRequest()` — normalizes request bodies (used by dispatch_func_call)
- KB (knowledge base) token counting

### 6.4 Function Call Flow

```
1. Client -> POST /funccall/{tenant}/{namespace}/{funcname}/{path}
2. auth_layer: validate JWT/API key -> AccessToken
3. TenantQuotaGuard: check billing quota
4. FuncCall1:
   a. Parse path -> (tenant, namespace, funcname, path)
   b. Validate scope/permissions from AccessToken
   c. resolve_funccall_target() -> FuncRouteTarget
   d. dispatch_func_call():
      i.   NormalizeFuncRequest() (tokenizer.rs)
      ii.  RetryGetClient() loop (within timeout):
           - SCHEDULER_CLIENT.LeaseWorker() -> LeasedWorker
           - funcAgentMgr.GetClient() -> QHttpCallClient
           - QHttpCallClient.Send() -> streaming response
      iii. Track TTFT (time-to-first-token)
      iv.  Stream response to client
      v.   On completion: SCHEDULER_CLIENT.ReleaseWorker()
      vi.  Record metrics (funccallcnt, TTFT, cold start)
5. On client disconnect: log status 499, cancel streaming
```

### 6.5 Skill Chain Flow

```
1. Client -> POST /skills/{tenant}/{namespace}/{skillname}
2. SkillCall():
   a. Load skill from PostgreSQL (SqlSecret.GetSkill)
   b. Resolve calling tenant (from auth or skill subscription)
   c. Check quota
   d. Load skill prefix from /opt/inferx/skills/{...}/skill.data
   e. If skill has child skills: handle_skill_call_chain()
      else: dispatch_func_call() directly
3. handle_skill_call_chain():
   a. parse_skill_chain_request() -> inject call_skillep tool definition
   b. Set up SSE trace streaming (mpsc channel)
   c. execute_skill_chain():
      i.   Build request body from template + history
      ii.  dispatch_func_call() to model
      iii. Parse response for tool calls
      iv.  If tool calls: execute_parallel_child_call() for each (concurrent)
      v.   Aggregate child results into history
      vi.  Repeat until no tool calls or max depth (5)
   d. Emit final response with aggregated token usage
```

---

## 7. Node Agent (na)

### 7.1 Purpose

The Node Agent runs on each physical machine and manages:
- Pod lifecycle (create, terminate, resume, snapshot)
- GPU memory allocation (256 MB slots via CUDA VMM)
- Snapshot storage (NVMe/SPDK, in-memory, file)
- TSOT networking (transparent socket offload for pods)
- CR containers (checkpoint/restore with CRIU + nvproxy)

### 7.2 Architecture

```
Scheduler --gRPC--> nodeagent_svc.rs (PodMgr)
                        |
                   podmgr_agent.rs (PmAgent)
                        |
                   pod_agent.rs (per-pod state machine)
                        |
              +---------+---------+---------+
              |         |         |         |
          snapshot   gpumem    nvme    namespace
           _mgr      _mgr     _agent    _mgr
              |         |         |
         tsot/ (networking, na_proxy, pod_broker)
              |
         LD_PRELOAD=libnvproxy.so
              |
         vLLM container (Podman + CRIU)
```

### 7.3 Key Subsystems

**Pod Management (pod_mgr/):**
- `nodeagent_svc.rs` — gRPC `NodeAgentService`: create_func_pod, terminate_pod, resume_pod, read_pod_log, remove_snapshot, cr_container_switch
- `podmgr_agent.rs` — supervisor over all pod agents, container cleanup
- `pod_agent.rs` (3249 lines) — per-pod state machine: PullingImage -> Loading -> Ready -> Snapshoting -> Terminating
- `snapshot_mgr.rs` (2215 lines) — snapshot cache (Mem/File/Blob), GPU memory save/restore with GDS, NVLink P2P
- `gpumem_mgr.rs` (972 lines) — slot-based GPU VMM, bitmap allocator, flex slots

**TSOT Networking (tsot/):**
- `tsot_svc.rs` — top-level orchestrator, seqpacket listeners
- `pod_broker.rs` — per-pod session, routes TsotMsg (socket, connect, DNS, NaMsg)
- `conn_svc.rs` — TCP server for inbound peer connections, outbound connects
- `dns_proxy.rs` — DNS resolution (metastore for *.svc.cluster.local, host DNS otherwise)
- `na_proxy.rs` — Node Agent proxy, seqpacket bridge for NaMsg lifecycle messages with fd passing

**NVMe Storage (nvme/):**
- `nvme_agent.rs` — SPDK driver agent, submission/completion queues
- `nvme_disk.rs` — SQLite-persisted block allocation striped across disks
- `nvme_file.rs` — file abstraction, read/write from host/memfd/GPU
- `mem_mgr.rs` — 64 MB buffer pool (CUDA host-registered)

### 7.4 Node Agent Proxy

The NA proxy (`na_proxy.rs`) is a Unix seqpacket bridge between the NA service and per-pod NA processes:
- **Server side** (`NA_PROXY`): binds `/opt/inferx/run/naproxy`, accepts pod connections, relays NaMsg with fd passing (GPU memory fds, memfds)
- **Client side** (`NA_PROXY_CLIENT`): connects to the proxy, receives InitCacheMem (memfd), InitGPUMem (GPU fds), SyncPodDone (ready signal)

**NaMsg types:** Standby, RunningPaused, HibereteDone, SnapshotDone, SnapshotDataReady, ResetGPUDone, PingResp, Resume, ResetGPU, GPUMemoryObjectLease, CudaHostMemAlloc, RestoreDataReady, NAConnect, HibernateMem, Fds, InitCacheMem, InitGPUMem, SyncPod*, PingReq, NodeAgentReady.

---

## 8. Shared Infrastructure (ixshare)

### 8.1 Configuration (node_config.rs)

`NODE_CONFIG` — loaded from `/opt/inferx/config/node.json` with env var overrides:
- `NodeConfig` — raw config schema (ports, etcd addresses, GPU config, resources, TLS, Keycloak)
- `GatewayConfig` — gateway-specific (scheduler port, audit DB, billing, onboarding credits, Keycloak config)
- `SchedulerConfig` — scheduler-specific (etcd, state service, audit DB, billing)
- `StateSvcConfig` — state service-specific (etcd, service IP, port)
- `NodeAgentConfig` — node agent-specific (all ports, CIDR, resources, snapshot dir, blob store, shared memory, TLS)

Env var overrides: `ENDPOINTS_DEFAULT_POLICY`, `INFERX_ENDPOINT_FUNC_DEFAULT_POLICY`, `INFERX_TENANT_POLICY` (JSON merge).

### 8.2 Metadata Store (metastore/)

Kubernetes-style caching/watch infrastructure:

**CacheStore** — in-memory cache over a backend store:
- Ring buffer of recent events (2000 entries)
- BTreeMap of current objects
- Watcher registry with predicate filtering
- Serves Get/List/Watch from cache (with revision consistency)
- Falls through to backend for revision=-1 requests

**EtcdStore** — etcd-backed `BackendStore`:
- CRUD with optimistic concurrency (etcd transactions with mod_revision)
- Paginated listing with continue tokens
- Watch setup with prefix
- Cache initialization and sync (InitCacheStore + UpdateCacheStore loop)

**Informer** — client-side watch pattern:
- InitList (initial load from state service)
- WatchUpdate (continuous watch loop)
- Distribute DeltaEvents to registered EventHandler
- ThreadSafeStore (BTreeMap with spin RwLock)

**InformerFactory** — manages multiple Informers:
- Batch InitList/Update/Reload across all informers concurrently
- AddEventHandler to all informers

**CacherClient** — gRPC client for IxMetaService:
- TCP and UDS connections
- Create/Update/Delete/Get/List/Watch/Uid

**SvcDir** — server-side service directory:
- Holds CacheStore instances by objType
- Implements IxMetaService gRPC trait
- Create/Update/Delete assign channelRev as revision

**UniqueId** — cluster-unique ID generator via etcd revision

### 8.3 Etcd Client (etcd/)

- `EtcdClient` — thin wrapper around `etcd_client::Client` with tokio Mutex
- `EtcdStore` — etcd-backed store with optimistic concurrency, watch, pagination
- `watch.rs` — Watcher (establishes watch stream, parses events, filters) + WatchReader (consumer)

### 8.4 Peer Manager (peer_mgr.rs)

`PEER_MGR` — registry of peer nodes for inter-node TCP:
- `AddPeer(hostIp, port, cidrAddr)` — register a peer
- `RemovePeer(cidrAddr)` — unregister
- `LookforPeer(ip)` — find peer by masking IP with 12-bit mask
- `BelongCidr(ip)` — check if IP is in same CIDR
- `IxTcpClient` — TCP client for TSOT: connects to peer NA, sends TsotConnReq, reads TsotConnResp

### 8.5 Audit & Billing (audit.rs)

PostgreSQL-backed audit agents with mpsc buffering:
- `PodAuditAgent` (300 buffer) — pod lifecycle audit (create, update, fail)
- `ReqAuditAgent` (300 buffer) — request latency audit
- `UsageTickAuditAgent` (1000 buffer) — billing ticks (start/periodic/final)

`SqlAudit` provides:
- Pod audit CRUD, fail log recording
- Snapshot schedule audit (upsert)
- Usage tick recording (18 columns)
- Tenant credits (add, balance, history)
- Billing rates (add, history)
- Billing summaries (balance, used, threshold, quota)
- Hourly usage queries (by tenant, namespace, function, model)
- Analytics summary (total ms, cents, top model/namespace, peak hour)
- Endpoint usage (cross-tenant, by period)

### 8.6 Protobuf Types (pb_gen/)

Generated gRPC types included via `include!`:
- `ixmeta.rs` — IxMetaService (create, update, delete, get, list, watch, uid, version)
- `na.rs` — NodeAgentService, SchedulerService (create_func_pod, lease_worker, etc.)
- `tsot.rs` — TSOT message types
- `qmeta.rs` — additional metadata types

### 8.7 PostgreSQL (pgsql/)

- `Listener` — PostgreSQL LISTEN/NOTIFY for real-time event streaming (used by request audit watch)

---

## 9. Cross-Service Data Flows

### 9.1 Function Creation

```
Client -> Gateway POST /object/function
  -> auth_layer (validate JWT)
  -> TenantQuotaGuard (check quota)
  -> CreateObj -> CacherClient.Create (gRPC to StateSvc)
  -> StateSvc.create:
     1. Admission check (CreateFuncCheck)
     2. EtcdStore.Create (persist to etcd)
     3. CreateFuncStatus (auto-create companion FunctionStatus)
  -> etcd watch -> CacheStore update
  -> Informers (Gateway, Scheduler) receive Added event
  -> Scheduler: ProcessAddFunc -> create standby pods
```

### 9.2 Function Call (End-to-End)

```
1. Client -> Gateway POST /funccall/{tenant}/{ns}/{func}/{path}
2. Gateway: auth -> quota -> resolve route -> dispatch_func_call
3. Gateway -> Scheduler: LeaseWorker (gRPC)
4. Scheduler:
   a. Find idle Ready pod -> set Working
   b. Or: ResumePod (standby -> ready) -> queue lease
   c. Return LeasedWorker (pod IP, port, node)
5. Gateway: QHttpCallClient -> pod (HTTP, keepalive)
6. Pod processes request, streams response
7. Gateway: stream response to client, track TTFT
8. Gateway -> Scheduler: ReturnWorker (pod back to idle)
9. Gateway: record metrics, audit
```

### 9.3 Pod Creation (Scheduler -> NodeAgent)

```
Scheduler: TryCreateSnapshotOnNode or ProcessAddFunc
  -> FindNode4Pod (select node, compute resources)
  -> StartWorker (spawn async gRPC)
  -> NodeAgent.create_func_pod:
     1. PmAgent.CreatePod
     2. NamespaceMgr.CreatePodSandbox (allocate IP)
     3. PodAgent::New + spawn task
     4. PodAgent.Process:
        a. PullingImage -> PullImage
        b. RunContainer (bollard/podman)
        c. Loading -> wait for health probe
        d. Ready
  -> On Ready: UpdatePod -> Scheduler matches to queued lease
```

### 9.4 Checkpoint/Restore

```
Scheduler -> NodeAgent: cr_container_switch
  -> CrContainer state machine:
     1. Running -> GpuContextLoad: POST /sleep?level=1 to vLLM
     2. GpuContextLoad -> MemLoaded: nvproxy_snapshot() via seqpacket
     3. MemLoaded -> Checkpoint: CRIU checkpoint (podman)
     4. Checkpoint -> Stopped: stop container, setup OverlayFS
  
  Restore:
     5. Stopped -> CleanUpper: clean overlay upper
     6. CleanUpper -> MemLoaded: CRIU restore (podman)
     7. MemLoaded -> GpuContextLoad: nvproxy_restore() via seqpacket
     8. GpuContextLoad -> Running: POST /wake_up to vLLM
```

### 9.5 Node Discovery

```
1. NodeAgent starts -> NodeRegister (etcd lease, 30s TTL)
   -> Writes Node object to etcd with IP, ports, CIDR, GPU resources
2. StateSvc: Node informer receives Added event
   -> IxAggrStore.AddIxAgent -> AggregateClient streams pod/node/snapshot
3. Scheduler: Node informer receives Added event
   -> SchedulerHandler.AddNode -> update nodes map, metrics
4. Other NodeAgents: node_mgr.rs receives Added event
   -> PEER_MGR.AddPeer (for inter-node TSOT routing)
```

---

## 10. Configuration

### 10.1 Service Ports

| Port | Service |
|------|---------|
| 1233 | NodeAgent (PodMgr gRPC) |
| 1234 | TSOT CNI |
| 1235 | TSOT Service (conn_svc TCP) |
| 1236 | NA State Service |
| 1237 | State Service |
| 1238 | Scheduler gRPC |
| 1240 | IxProxy HTTP (health) |
| 4000 | Gateway HTTP/TLS |
| 80 | Scheduler HTTP (debug/metrics) |

### 10.2 Key Paths

| Path | Purpose |
|------|---------|
| `/opt/inferx/config/node.json` | Main configuration |
| `/opt/inferx/config/config.json` | Node agent config |
| `/opt/inferx/log/` | Log files |
| `/opt/inferx/run/naproxy` | NA proxy UDS |
| `/opt/inferx/sockets/tsot-socket` | TSOT pod listener |
| `/opt/inferx/sockets_host/tsot-socket` | TSOT gateway listener |
| `/opt/inferx/socket/nvproxy.sock` | nvproxy agent socket |
| `/opt/inferx/skills/` | Skill data files |
| `/opt/inferx/snapshot/` | GPU snapshots |
| `/opt/inferx/data/sqlite.db` | NVMe disk allocation DB |

### 10.3 External Services

| Service | Purpose |
|---------|---------|
| etcd | Cluster state, leader election, unique ID generation |
| PostgreSQL (auditdb) | Pod audit, request audit, snapshot schedule audit |
| PostgreSQL (billingdb) | Usage ticks, tenant credits, billing rates |
| PostgreSQL (secret store) | API keys, skills, templates, subscriptions, endpoint metadata |
| Keycloak | JWT authentication, RBAC roles |

---

## 11. Tenant Resource Limits

| Limit | Default | Description |
|-------|---------|-------------|
| maxFuncCnt | 10 (serde) / 6 (default fn) | Max functions per tenant |
| maxGpu | 4 (serde) / 2 (default fn) | Max GPUs per tenant |
| maxReplica | 2 | Max replicas per function |
| maxStandby | 1 | Max standby pods per node |
| maxQueueLen | 100 | Max queued lease requests |
| minReplicaCap | 0 | Cap for scheduler default minReplica |
| allocMemStandby | false | Allow Mem-type standby |
| quota_exempt | false | Exempt from billing quota |

Platform tenants ("inferx", "public") are exempt from GPU limits.

---

## 12. Billing Model

### 12.1 Usage Types

| Type | Description | Billing |
|------|-------------|---------|
| request | Active inference request | Per-request GPU usage |
| snapshot | Snapshot pod creation/loading | GPU usage during loading |
| standby | Idle standby pods (snapshots exist) | $0.20/hr/GPU per model |

### 12.2 Tick Types

| Tick | When | Interval |
|------|------|----------|
| start | Pod creation/start | Once |
| periodic | Regular billing | 60s (snapshot), 600s (standby) |
| final | Pod termination | Once |

### 12.3 Billing Fields

`UsageTick`: session_id, tenant, caller_tenant, namespace, funcname, fprevision, nodename, pod_id, gateway_id, gpu_type, gpu_count, vram_mb, total_vram_mb, tick_time, interval_ms, tick_type, usage_type, is_coldstart.

### 12.4 Tenant Credits

- Tenants have a credit balance (in cents)
- Credits are added by admins
- Usage debits the balance
- `quota_exceeded` flag disables inference when balance is exhausted
- `RecalculateTenantQuota` recomputes balance and quota flag

---

## Appendix A: Module Sizes

| Module | Lines (approx) |
|--------|---------------|
| gateway/http_gateway.rs | ~6,750 |
| scheduler/scheduler_handler.rs | ~6,345 |
| gateway/skill_chain.rs | ~3,204 |
| na/pod_mgr/pod_agent.rs | ~3,249 |
| na/pod_mgr/snapshot_mgr.rs | ~2,215 |
| audit.rs | ~3,500+ |
| node_config.rs | ~1,000+ |
| na/pod_mgr/gpumem_mgr.rs | ~972 |
| gateway/secret.rs | ~1,600+ |
| gateway/func_worker.rs | ~1,400+ |
| na/pod_mgr/podmgr_agent.rs | ~1,310 |

## Appendix B: Glossary

| Term | Meaning |
|------|---------|
| StateSvc | State Service — central metadata authority |
| NA | Node Agent — per-node runtime |
| TSOT | Transparent Socket-Over-Transport — pod networking |
| CR | Checkpoint/Restore |
| GDS | GPU Direct Storage |
| VMM | Virtual Memory Management (CUDA cuMem*) |
| Slot | 256 MB GPU memory unit |
| Standby | Snapshot stored on disk/memory/NVMe, ready for fast restore |
| FuncPod | Running instance of a Function |
| WorkerPod | Pod available for leasing (Idle/Working state) |
| LeasedWorker | Pod leased to a gateway for serving requests |
| SkillChain | Multi-turn LLM conversation with child skill tool calls |
| skillep | Skill endpoint — child skill callable via call_skillep tool |
| EndpointLeaseLimiter | Per-tenant concurrent lease limiter |
| BiIndex | Bidirectional index (id1, id2) -> Info |
| DataObject<T> | Universal typed object envelope |
| DeltaEvent | Object change event (Added/Modified/Deleted/InitDone) |
| Informer | Client-side watch pattern (list + watch + local store) |
| CacherClient | gRPC client for IxMetaService |
| SvcDir | Server-side directory of CacheStores |
| CacheStore | In-memory cache with ring buffer and watchers |
| EtcdStore | etcd-backed BackendStore with optimistic concurrency |
| AggregateClient | Streams per-node state into shared caches |
