# Tenant Context Design Review

## Decision

We will adopt the following design:

- keep `restrictTenant` for hard-scoped credentials such as API keys
- introduce a general user-level `defaultTenant`
- persist `defaultTenant` in a new user-centric table
- keep `UserRole` as the authorization source of truth
- keep InferX admin as a special authorization case, but give it a `defaultTenant` for fallback billing/context

Recommended naming:

- code field: `defaultTenant`
- DB column: `default_tenant`

## Why This Design

We need to distinguish two different concepts:

- authorization scope
- default tenant context

These are not the same thing.

### `restrictTenant`

`restrictTenant` means:

- this credential cannot act outside this tenant

This is appropriate for:

- API keys
- service credentials
- any explicitly tenant-scoped credential

### `defaultTenant`

`defaultTenant` means:

- if the request/session does not explicitly choose a tenant, use this tenant as the default context

This is appropriate for:

- dashboard/browser user sessions
- InferX admin fallback billing/default context
- current regular users, even if they usually have only one tenant today
- future users who may access more than one tenant

## Why Not Reuse `UserRole`

`UserRole` is still correct for authorization, but it is not enough for default context.

`UserRole` tells us:

- which tenants a user can access
- whether they are admin or user

It does **not** tell us:

- which tenant should be chosen by default
- which tenant should be billed if no tenant is explicitly selected
- which tenant should be treated as the user’s home/default company

So:

- `UserRole` remains the authorization truth
- `defaultTenant` becomes the default-selection truth

## Why Not Reuse `restrictTenant`

`restrictTenant` already has strong semantics:

- it is a hard security restriction on the credential

That is correct for API keys, but not correct for dashboard user tokens.

A dashboard user may:

- have broader roles
- still need one default tenant context

So overloading `restrictTenant` for normal user sessions would blur the distinction between:

- credential restriction
- user preference/default context

That is why `defaultTenant` should be separate.

## Why A New Table Is Needed

`defaultTenant` is:

- user-centric
- durable
- not a role
- not onboarding workflow state
- not tenant/company metadata

So it does not fit well into existing tables.

Even if regular users are currently constrained to a single non-`public` tenant, a persisted user-level field is still justified because:

- multiple InferX admins may exist
- global admin authorization does not reveal which tenant should be used as fallback billing/default context
- we want one durable user-level model for default context instead of permanently branching by user type

### Why not `UserOnboard`

`UserOnboard` is for onboarding saga/workflow state:

- pending
- complete
- failed
- saga step

It is not the right home for a long-term default tenant preference.

### Why not `TenantProfile`

`TenantProfile` is tenant-centric:

- one row describes a tenant/company and its owner identity

`defaultTenant` is user-centric:

- one row should describe a user’s default tenant context

### Why not `UserRole`

`UserRole` describes allowed access, not default choice.

## Proposed New Table

Add a user-level profile table, for example:

- `UserProfile`
  - `sub` primary key
  - `default_tenant`
  - `created_at`
  - `updated_at`

This table becomes the source of truth for default tenant context.

Expected steady state:

- every user has one `UserProfile` row
- regular users use `UserProfile.default_tenant` directly
- InferX admin users use the same field directly
- `UserRole` remains authorization-only

### Table notes

- `sub` should be the stable identity key
- do not duplicate `username` here unless a concrete operational need appears later
- `default_tenant` should reference the tenant table/object model as a foreign-key-like constraint where feasible

### Tenant deletion behavior

If a tenant is deleted and a user’s `default_tenant` points to it, the system should:

- clear `default_tenant` to `NULL`, or
- reject deletion until the reference is reassigned

The preferred operational behavior is to clear it to `NULL` and let runtime resolution fall back safely.

## How `defaultTenant` Gets Set

### Normal onboarding

When a user is onboarded and their tenant is created:

- create the `UserProfile` row if it does not already exist
- set `default_tenant = onboard tenant`

This will be true for:

- regular users
- InferX admin too, if the admin has an onboard/home tenant

### Future invite flow

For now, regular users should be constrained to:

- at most one non-`public` tenant at a time

That means:

- if a regular user is invited to another tenant, the invite flow must reject it until the existing tenant relationship is resolved
- if the user owns or was onboarded with a tenant, that prior relationship must be resolved before reassignment
- the exact resolution policy is TBD
  - transfer ownership
  - remove membership
  - delete tenant
  - another admin workflow

If later we support:

- one tenant -> many users
- potentially one user -> many tenants

then:

- joining another tenant should **not** automatically overwrite `default_tenant`
- only set it automatically if the user does not already have one
- otherwise let the user change it explicitly later

TBD for future implementation:

- expose a user-facing mutation path for `defaultTenant`
- for example, a settings/tenant-switcher flow backed by an endpoint such as `PATCH /user/profile`

## How `defaultTenant` Gets Loaded

When Keycloak authentication succeeds and the app normalizes into `AccessToken`:

- load the user profile by `sub`
- attach `defaultTenant` into `AccessToken`

If no `UserProfile` row exists, or if the stored `default_tenant` is missing, deleted, or not authorized for the user:

- treat this as an invalid authenticated user state
- fail authentication/normalization for tenant-scoped dashboard flows
- do not rely on downstream request handlers to recover tenant context

So the runtime token model becomes:

- `restrictTenant`
- `defaultTenant`
- roles

Each field has a different meaning.

### Caching

`defaultTenant` lookup is on the authentication normalization path, so it should not require downstream code to re-read the database on every request.

Recommended approach:

- load `defaultTenant` once during auth normalization
- attach it to the normalized `AccessToken`

This keeps request-time tenant resolution cheap and consistent with how other token-derived fields are already used.

### Staleness

Because `defaultTenant` is attached to the normalized `AccessToken`, changes to `defaultTenant` during an active session may not be visible immediately.

Examples:

- user changes `defaultTenant`
- tenant deletion clears `defaultTenant` to `NULL`

Recommended interpretation:

- stale `defaultTenant` is acceptable within the normal token/session lifetime
- any use of `defaultTenant` must still pass authorization checks at resolution time

If that bounded staleness later proves unacceptable, an explicit session invalidation or token-cache eviction mechanism will be needed.

## Tenant Resolution Order

Recommended resolution order:

1. If `restrictTenant` exists, use it
2. Else if the request explicitly supplies a tenant and the user is authorized for it, use that
3. Else if `defaultTenant` exists and the user is authorized for it, use that
4. Else if the user is **not** InferX admin and exactly one accessible tenant exists, use that
5. Otherwise return an error requiring explicit tenant selection

### Authorization invariant

`defaultTenant` must never bypass authorization.

This means:

- `defaultTenant` is only a candidate fallback tenant
- the candidate tenant must still exist as a valid tenant entity
- it must always be validated against the user’s current permissions
- if access to `defaultTenant` has been revoked, resolution must fall through to later steps

In particular:

- a deleted tenant referenced by stale cached `defaultTenant` must not be accepted
- revoked membership in `defaultTenant` must not silently continue to work
- stale cached `defaultTenant` values are safe only because authorization is re-checked at resolution time

So the step-3 validation should effectively require both:

- tenant exists
- user is currently authorized for that tenant

This keeps:

- API keys strict and simple
- dashboard sessions predictable
- future multi-tenant support possible

## Billing Rule

Billing should follow the effective acting tenant for the request.

That means:

- if request context selects tenant A, bill tenant A
- if nothing is explicitly selected, fall back to `defaultTenant`

For InferX admin specifically:

- admin authorization remains global
- `defaultTenant` gives a fallback billing/default context

This avoids tying global admin authorization to billing semantics.

## InferX Admin Handling

InferX admin remains a special authorization case.

But InferX admin should still have:

- a `defaultTenant`

That lets the system:

- keep global access behavior unchanged
- still choose a deterministic fallback billing tenant when no explicit tenant is selected
- support multiple InferX admins without guessing a fallback tenant from authorization alone

So:

- `IsInferxAdmin()` stays as-is for permission logic
- `defaultTenant` handles fallback context/billing
- InferX admin should not rely on tenant enumeration fallback
- InferX admin resolution should effectively be:
  - `restrictTenant`
  - explicit tenant
  - `defaultTenant`
  - otherwise error

### How InferX admin gets `defaultTenant`

This needs to be treated as an operational initialization step.

Possible ways to set it:

- through the same onboarding flow if admin is onboarded like a normal user
- through deploy-time seeding
- through an admin-only CLI/API/manual setup path

The important invariant is:

- InferX admin authorization remains global
- each InferX admin still has one persisted `defaultTenant` used as fallback context/billing tenant

## Migration For Existing Users

Because `defaultTenant` is a new persisted user-level field, existing users will need a migration/backfill plan.

### Recommended rollout

1. Add the new user-level table with `default_tenant` nullable at first
2. Update onboarding and admin initialization so new users always get a `UserProfile` row
3. Backfill existing users, including regular users
4. Update auth normalization to load `defaultTenant`
5. Keep runtime fallback logic safe for any legacy users still missing `UserProfile` or `default_tenant`
6. Optionally tighten invariants later once the data is clean

### Backfill sources

Recommended backfill priority:

1. onboard tenant from onboarding/profile ownership data
2. if not available, derive from `UserRole` when the user has exactly one tenant
3. for InferX admin, assign its onboard/home tenant if one exists

Backfill goal:

- create a `UserProfile` row for every existing user
- populate `default_tenant` during that backfill where a safe source exists

### Ambiguous cases

If any existing user has:

- no tenant
- more than one tenant
- incomplete onboarding

then the safest approach is:

- leave `default_tenant` as `NULL`
- let runtime resolution fall back to explicit tenant selection or other safe logic
- optionally log/report these users for manual cleanup

### Important backfill note

Backfilling from `UserRole` is only an approximation.

Why:

- `UserRole` is the authorization truth
- it is not guaranteed to represent ownership/home/default semantics

So:

- onboarding/profile ownership data should be treated as authoritative where available
- single-tenant derivation from `UserRole` is acceptable only as a migration/backfill fallback
- rows derived from `UserRole` should be considered heuristic rather than authoritative

### Why nullable-first is better

Making `default_tenant` nullable initially allows a safe rollout:

- schema can ship first
- backfill can be done incrementally
- runtime can stay compatible during migration

This is lower risk than trying to enforce a strict non-null invariant immediately.

## Relationship To Current Product Reality

Even if today we mostly behave like:

- one tenant -> many users
- each regular user belongs to one tenant

this design is still acceptable because:

- it does not break the simple case
- regular users will usually have `defaultTenant == only tenant`, but stored explicitly in `UserProfile`
- invite flow can enforce the single-tenant regular-user rule without changing the long-term data model
- multiple InferX admins already require a user-level fallback tenant model
- it avoids redesign later if multi-tenant users are introduced

In other words:

- today it behaves simply
- later it scales cleanly

## Summary

Chosen design:

- keep `restrictTenant` for hard credential restriction
- add `defaultTenant` for default user context
- persist `defaultTenant` in a new user-centric table
- key the table by `sub`
- keep `UserRole` for authorization
- keep InferX admin special for auth, but use `defaultTenant` for fallback billing/context

This is the cleanest separation of concerns:

- `UserRole`: what the user may do
- `restrictTenant`: what the credential cannot leave
- `defaultTenant`: which tenant to use by default

## Implementation Checklist

The implementation goal is:

- keep API-key tenant resolution on `restrictTenant`
- stop relying on request-supplied tenant overrides for normal dashboard/user-token flows
- give regular users and InferX admin a deterministic default tenant context
- preserve authorization and billing correctness

### Phase 1: Data Model

- [ ] Add a new `UserProfile` table
  - `sub` primary key
  - `default_tenant` nullable
  - timestamps
- [ ] Add a reference/constraint from `default_tenant` to the tenant model where feasible
- [ ] Define tenant-deletion behavior
  - clear `default_tenant` to `NULL`

### Phase 2: Data Access Layer

- [ ] Add read/write methods for `UserProfile`
  - get profile by `sub`
  - upsert `default_tenant`
  - clear `default_tenant` on tenant deletion
- [ ] Add a helper to read `default_tenant` by `sub` without failing if no row exists

### Phase 3: Backfill / Migration

- [ ] Create a migration/backfill plan for existing users
- [ ] Backfill from onboarding/profile ownership data first
- [ ] Backfill from single-tenant `UserRole` only as fallback
- [ ] Leave ambiguous users with `default_tenant = NULL`
- [ ] Report ambiguous rows for manual review
- [ ] Define how InferX admin gets its initial `default_tenant`
  - onboarding
  - deploy-time seed
  - or admin-only setup path
- [ ] Production rollout gate:
  - backfill must run before Phase 5 is enabled in production, or
  - Phase 5 must be feature-flagged until backfill is confirmed complete

### Phase 4: AccessToken Model

- [ ] Add `defaultTenant: Option<String>` to `AccessToken`
- [ ] Keep `restrictTenant` unchanged
- [ ] Do not change `IsInferxAdmin()` semantics

### Phase 5: Auth Normalization

- [ ] During successful auth normalization, load `UserProfile` by `sub`
- [ ] Attach `defaultTenant` to the normalized `AccessToken`
- [ ] Missing `UserProfile` row must resolve to `defaultTenant = None`
- [ ] Keep this load on the auth-normalization path only
- [ ] Use the normalized `AccessToken` as the runtime cache for `defaultTenant`
- [ ] Deployment sequencing:
  - Phase 5 and Phase 8 must be deployed atomically, or
  - Phase 8 must ship before Phase 5

### Phase 6: Tenant Resolution Logic

- [ ] Update tenant resolution order to:
  1. `restrictTenant`
  2. explicit request tenant if present and authorized
  3. `defaultTenant` if present, tenant exists, and user is authorized
  4. non-admin single accessible tenant fallback
  5. otherwise error
- [ ] Ensure InferX admin skips tenant-enumeration fallback
- [ ] Ensure `defaultTenant` never bypasses authorization
- [ ] Ensure `defaultTenant` validation requires both:
  - tenant exists
  - user is authorized for it

### Phase 7: Billing Behavior

- [ ] Make billing follow the resolved acting tenant
- [ ] For InferX admin without explicit tenant selection, fall back to `defaultTenant`
- [ ] Verify no existing path accidentally bills by global admin identity instead of resolved tenant context

### Phase 8: Onboarding / Future Invite Flow

- [ ] On onboarding, create `UserProfile` row and set `default_tenant = onboard tenant`

Deferred future work:

- [ ] In invite flow, only auto-set `default_tenant` if it is currently `NULL`
- [ ] In invite flow, do not overwrite an existing user default automatically
- [ ] Add a user-facing mutation path for changing `defaultTenant`
  - e.g. settings page / tenant switcher
  - backed by something like `PATCH /user/profile`

### Phase 9: Testing

- [ ] Regular user with one tenant resolves correctly from `defaultTenant`
- [ ] API key still resolves by `restrictTenant`
- [ ] `restrictTenant` takes priority over `defaultTenant`
- [ ] InferX admin resolves to `defaultTenant` when no explicit tenant is provided
- [ ] InferX admin with no explicit tenant and no `defaultTenant` returns an error
- [ ] Revoked access to `defaultTenant` causes error rather than silent success
- [ ] Deleted tenant referenced by stale `defaultTenant` is rejected
- [ ] Missing `UserProfile` row is allowed only on `/onboard` bootstrap and otherwise fails auth
- [ ] Ambiguous or invalid user with no `defaultTenant` fails auth until repaired

### Phase 10: Optional Cleanup

- [x] Review all current `X-Tenant` usage
- [x] Remove `X-Tenant` runtime dependence from dashboard and gateway skill flows
- [x] Preserve MCP/API-key correctness during cleanup
