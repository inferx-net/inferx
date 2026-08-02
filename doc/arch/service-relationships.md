# InferX Service Architecture

## 1. Service Topology

```
     External Clients (SDK / curl / browser)
           |
           | HTTP/TLS
           v
    +--------------+
    |   Gateway    |--- lease/return/kill ---> +------------+
    |   (4000)     |                           | Scheduler  |
    |              |<--- LeasedWorker ---------| (1238)     |
    |              |                           | leader-    |
    |              |                           | elected    |
    +------+-------+                           +-----+------+
           |                                         |
           | gRPC (IxMetaService)                    | watches etcd
           v                                         v
    +--------------+                          +-------------+
    |  StateSvc    |<------ gRPC ------------>|   etcd     |
    |  (1237)      |   (per-node state)       | (cluster   |
    |  metadata    |                          |  state)    |
    |  authority   |                          +-------------+
    +------+-------+
           |
           | aggregates per-node state
           | (AggregateClient -> each node's state svc)
           |
    +------+------+------+------+
    |      |      |      |      |
    v      v      v      v      v
  +----+ +----+ +----+ +----+ +----+
  | NA | | NA | | NA | | NA | | NA |  (one per physical node)
  |1233| |1233| |1233| |1233| |1233|
  +--+-+ +--+-+ +--+-+ +--+-+ +--+-+
     |      |      |      |      |
     +------+------+------+------+
            |
            | Podman + CRIU + nvproxy (LD_PRELOAD)
            v
       vLLM containers
```

## 2. Services

### 2.1 Gateway

**Role:** External API entry point. The only service exposed to clients.

**Inbound:** HTTP/TLS on port 4000 from external clients.

**Outbound:**
- **Scheduler** (gRPC, port 1238): `LeaseWorker`, `ReturnWorker`, `KillPod`, `ConnectScheduler`, `RefreshGateway`
- **StateSvc** (gRPC, port 1237): object CRUD (create/update/delete tenants, namespaces, functions, policies), `Get`/`List`/`Watch` for object cache
- **NodeAgent** (HTTP): direct pod log reads, pod audit, function call to specific pod (via leased worker IP)
- **PostgreSQL**: API keys, skills, billing, audit
- **Keycloak**: JWT validation (JWKS fetch)

**State:** Stateless. Maintains in-memory caches via informers (GwObjRepo watching StateSvc).

### 2.2 Scheduler

**Role:** Pod placement and lifecycle orchestration. Decides where pods run, when snapshots are created, and how standby pods are maintained.

**Inbound:** gRPC on port 1238 from Gateways (`LeaseWorker`, `ReturnWorker`, `KillPod`, `ConnectScheduler`).

**Outbound:**
- **NodeAgent** (gRPC, port 1233): `create_func_pod`, `terminate_pod`, `resume_pod`, `remove_snapshot`
- **StateSvc** (gRPC, port 1237): `Get`/`Update` FunctionStatus on failures
- **etcd**: leader election (lease-based), object watches via informers
- **PostgreSQL**: audit (snapshot schedule records, usage ticks for billing)

**State:** Leader-elected (single active instance). Maintains full cluster view: nodes, pods, functions, snapshots, policies. All state derived from etcd watches + RPC completions.

### 2.3 StateSvc

**Role:** Central metadata authority. The system of record for all cluster objects.

**Inbound:** gRPC (`IxMetaService`) on port 1237 from Gateway, Scheduler, and all NodeAgents.

**Outbound:**
- **etcd**: persist all objects (tenants, namespaces, functions, function status, nodes, policies, scheduler registration)
- **NodeAgent state services** (HTTP, port 1236 per node): `AggregateClient` streams pod/node/snapshot data from each node
- **PostgreSQL**: request audit (LISTEN/NOTIFY)

**State:** Authoritative. All writes go to etcd with optimistic concurrency. Serves reads from in-memory `CacheStore` caches backed by etcd watches. Per-node state (pods, snapshots) aggregated from NodeAgents into separate caches.

### 2.4 NodeAgent (na)

**Role:** Per-node runtime. Manages everything on one physical machine.

**Inbound:**
- **Scheduler** (gRPC, port 1233): `create_func_pod`, `terminate_pod`, `resume_pod`, `remove_snapshot`, `cr_container_switch`
- **Pods** (Unix seqpacket): TSOT socket operations, NaMsg lifecycle messages
- **StateSvc** (gRPC, port 1237): serves per-node state (pod/node/snapshot) to `AggregateClient`

**Outbound:**
- **etcd**: node registration (lease-based, 30s TTL), CIDR allocation
- **Podman/Docker** (API): container lifecycle
- **CRIU**: process checkpoint/restore
- **nvproxy** (Unix seqpacket): GPU snapshot/restore via `nvproxycli`
- **SPDK** (C FFI): NVMe I/O for snapshot storage
- **CUDA** (FFI): GPU memory allocation, cuFile GDS, checkpoint API
- **Peer NodeAgents** (TCP): inter-node pod networking (TSOT)

**State:** Per-node. Maintains local pod state, GPU memory slots, snapshot caches, NVMe file allocations. Reports state to StateSvc via per-node state service.

## 3. Data Store Roles

```
+-------------+     +------------------+     +------------------+
|    etcd     |     |  PostgreSQL      |     |  Keycloak        |
|             |     |                  |     |                  |
| Cluster     |     | Audit DB:        |     | JWT issuance     |
| state:      |     | - pod audit      |     | RBAC roles       |
| - tenants   |     | - request audit  |     | User identities  |
| - namespaces|     | - snapshot sched |     |                  |
| - functions |     |                  |     |                  |
| - funcstatus|     | Billing DB:      |     |                  |
| - nodes     |     | - usage ticks    |     |                  |
| - policies  |     | - tenant credits |     |                  |
| - scheduler |     | - billing rates  |     |                  |
|   reg.      |     |                  |     |                  |
| - statesvc  |     | Secret DB:       |     |                  |
|   reg.      |     | - API keys       |     |                  |
| - unique_id |     | - skills         |     |                  |
|             |     | - templates      |     |                  |
| Leader      |     | - subscriptions  |     |                  |
| election    |     | - endpoint meta  |     |                  |
| CIDR locks  |     | - tenant profiles|     |                  |
+-------------+     +------------------+     +------------------+
```

| Store | Owner | What | Consistency |
|-------|-------|------|-------------|
| etcd | StateSvc | All cluster metadata (control plane) | Strong (optimistic concurrency via mod_revision) |
| PostgreSQL (audit) | All services | Audit logs, billing ticks | Append-only |
| PostgreSQL (billing) | Gateway, Scheduler | Tenant credits, billing rates, usage summaries | Transactional |
| PostgreSQL (secret) | Gateway | API keys, skills, templates, subscriptions | Transactional |
| SQLite (per-node) | NodeAgent | NVMe disk block allocation | Local transactional |
| In-memory (CacheStore) | StateSvc, Gateway, Scheduler | Cached object copies | Eventually consistent (via etcd watch) |

## 4. Communication Patterns

### 4.1 Synchronous (gRPC)

| From -> To | Protocol | Purpose |
|------------|----------|---------|
| Client -> Gateway | HTTP/TLS | Function calls, admin, skills |
| Gateway -> Scheduler | gRPC/TCP | Lease/return workers, kill pods |
| Gateway -> StateSvc | gRPC/TCP | Object CRUD, watch |
| Scheduler -> NodeAgent | gRPC/TCP | Create/terminate/resume pods, remove snapshots |
| Scheduler -> StateSvc | gRPC/TCP | Get/update function status |
| NodeAgent -> StateSvc | gRPC/TCP | Report per-node state |
| Any -> StateSvc | gRPC/UDS | Local state queries (Unix domain socket) |

### 4.2 Asynchronous (Watch/Informer)

| Watcher | Watched (via StateSvc) | What they do with events |
|---------|----------------------|--------------------------|
| Gateway | Tenant, Namespace, Function, FunctionStatus, FuncPolicy | Update in-memory route table, function cache |
| Scheduler | Function, FuncPod, Node, ContainerSnapshot, FuncPolicy, Tenant, FunctionStatus | Trigger scheduling decisions (create/terminate pods, snapshots) |
| StateSvc | Tenant, Namespace, Function (self-watch) | Update in-memory managers for admission checks |
| NodeAgent (node_mgr) | Node | Add/remove peers for inter-node networking |

### 4.3 Streaming

| Stream | Protocol | Purpose |
|--------|----------|---------|
| StateSvc -> NodeAgent (per node) | HTTP (AggregateClient) | Stream pod/node/snapshot state to StateSvc |
| Gateway -> Client | HTTP (chunked/SSE) | Stream function call response (LLM token streaming) |
| Gateway -> Client | SSE | Skill chain trace events |
| StateSvc -> Any | gRPC server stream | Watch object changes (DeltaEvents) |
| Pod -> NodeAgent | Unix seqpacket | TSOT socket operations, NaMsg lifecycle with fd passing |

## 5. Lifecycle Flows

### 5.1 Cluster Bootstrap

```
1. etcd starts
2. StateSvc starts:
   a. Connect to etcd, create CacheStores for all object types
   b. IxAggrStore starts Node informer (no nodes yet)
   c. Self-register in etcd (20s lease)
   d. Start gRPC server on port 1237
3. NodeAgent starts (on each node):
   a. Allocate CIDR via etcd lock
   b. Register Node in etcd (30s lease)
   c. Start per-node state service (port 1236)
   d. Start PodMgr gRPC (port 1233), TSOT (port 1235), NA proxy
4. StateSvc IxAggrStore:
   a. Node informer receives Added events
   b. Creates IxAgent per node -> AggregateClient streams state
5. Scheduler starts:
   a. Connect to etcd, create SchedObjRepo with 7 informers
   b. Leader election (compete for "scheduler" key)
   c. On winning: start gRPC server (port 1238)
   d. Initial list completes -> warm-up (5s) -> start scheduling
6. Gateway starts:
   a. Connect to StateSvc, create GwObjRepo with informers
   b. Connect to Scheduler (SCHEDULER_CLIENT)
   c. Start HTTP/TLS server (port 4000)
7. Gateway -> Scheduler: ConnectScheduler
8. Scheduler starts creating standby pods per function policies
```

### 5.2 Function Deployment

```
Client -> Gateway: POST /object/function (create function spec)
  -> Gateway -> StateSvc: Create (gRPC)
  -> StateSvc: admission check -> etcd Create -> create FunctionStatus
  -> etcd watch -> Scheduler informer receives Added event
  -> Scheduler.ProcessAddFunc:
     -> If nvidiaReplica > 0: create NVIDIA runtime pod
     -> If minReplica > 0: resume standby pods to meet minimum
     -> If standbyPerNode > 0: create snapshot on best node
  -> Scheduler -> NodeAgent: create_func_pod (gRPC)
  -> NodeAgent: pull image, start container, wait for health
  -> NodeAgent -> StateSvc: report pod state (Ready)
  -> StateSvc -> Scheduler informer: FuncPod Modified
  -> Scheduler: pod becomes Ready -> available for leasing
```

### 5.3 Request Serving

```
1. Client -> Gateway: POST /funccall/{tenant}/{ns}/{func}/{path}
2. Gateway: authenticate (JWT/API key) -> AccessToken
3. Gateway: check tenant billing quota
4. Gateway -> Scheduler: LeaseWorker(funcId)
5. Scheduler:
   a. Find idle Ready pod -> set Working(gatewayId)
   b. Return LeasedWorker (pod IP, port)
   (or: no idle pod -> resume standby -> queue -> return when ready)
6. Gateway: HTTP request to pod (keepalive connection pool)
7. Pod: vLLM processes request, streams tokens
8. Gateway: stream response to client, track TTFT
9. Gateway -> Scheduler: ReturnWorker (pod back to idle)
10. Gateway: record metrics, audit
```

### 5.4 Checkpoint/Restore Cycle

```
Scheduler decides to create a snapshot:
1. Scheduler -> NodeAgent: create_func_pod (type=Snapshot)
2. NodeAgent: start container, load model, run warmup
3. NodeAgent -> nvproxy: snapshot (GPU memory -> files)
4. NodeAgent -> CRIU: checkpoint (process state -> files)
5. NodeAgent: stop container, save snapshot metadata
6. NodeAgent -> StateSvc: report snapshot Ready
7. Scheduler: snapshot available for restore

Scheduler decides to restore (resume) a standby pod:
1. Scheduler -> NodeAgent: resume_pod
2. NodeAgent: CRIU restore (process state from files)
3. NodeAgent -> nvproxy: restore (GPU memory from files/NVMe)
4. NodeAgent: POST /wake_up to vLLM
5. Pod enters Ready state -> available for leasing
```

### 5.5 Node Failure / Restart

```
NodeAgent restarts (node epoch changes):
1. Old Node lease expires -> etcd deletes Node object
2. Scheduler: Node informer receives Deleted -> CleanNode
3. New NodeAgent starts -> registers with new epoch
4. Scheduler: Node informer receives Added -> AddNode
5. Scheduler: CheckNodeEpoch detects restart
   -> ReconcileNodeAfterNodeAgentRestart:
      a. Remove transient pods (non-Standby/Ready)
      b. Recalculate available resources
      c. Sync surviving pod states
      d. Clean pending snapshots
6. Normal scheduling resumes
```

## 6. Control Plane vs Data Plane

```
CONTROL PLANE (manages what runs where):
  StateSvc    -- "what exists" (cluster objects in etcd)
  Scheduler   -- "what should run where" (placement decisions)
  Gateway     -- "who can access what" (auth, routing, billing)

DATA PLANE (runs the actual work):
  NodeAgent   -- runs pods, manages GPU/NVMe/networking
  vLLM pods   -- serve inference requests
  TSOT        -- pod networking (transparent socket offload)
  nvproxy     -- GPU state checkpoint/restore
  SPDK/NVMe   -- snapshot storage I/O
```

## 7. Scaling Characteristics

| Service | Scaling | State | Failure Mode |
|---------|---------|-------|---------------|
| Gateway | Horizontal (stateless) | In-memory cache (informer) | Requests fail; client retries to another gateway |
| Scheduler | Single (leader-elected) | Full cluster view in memory | Lease expires -> new leader elected -> 5s warmup |
| StateSvc | Horizontal (stateless) | etcd + in-memory cache | Reads fail; writes go to etcd (survives) |
| NodeAgent | One per node | Per-node state | Pods on that node become unreachable; scheduler cleans up |

## 8. Request Routing Summary

```
Client request
  |
  v
Gateway (auth + quota + route)
  |
  +-- function call --> Scheduler.LeaseWorker --> NodeAgent pod
  |                                                    |
  |<-- streaming response <---------------------------+
  |
  +-- skill call --> SkillChain --> dispatch_func_call --> Scheduler.LeaseWorker --> pod
  |
  +-- object CRUD --> StateSvc --> etcd
  |
  +-- billing/admin --> PostgreSQL
  |
  +-- pod management --> Scheduler.KillPod --> NodeAgent
  |
  +-- log read --> NodeAgent (direct HTTP)
```
