# Keycloak Quick Setup Guide

Step-by-step instructions for configuring a freshly deployed Keycloak instance for InferX.

---

## Prerequisites

Make sure Keycloak is running and the port is forwarded:

```bash
sudo kubectl port-forward svc/keycloak 31260:8080 --address=0.0.0.0
```

Then open in your browser:

```
http://localhost:31260/authn/admin
```

Log in with:
- **Username**: `admin`
- **Password**: `inferx!`

---

## Step 1 — Create the `inferx` Realm

1. In the top-left dropdown (showing **Keycloak**), click **Create realm**
2. Set **Realm name** to `inferx`
3. Make sure **Enabled** is on
4. Click **Create**

---

## Step 2 — Create the `inferx_client` Client

1. In the left sidebar, go to **Clients** → **Create client**
2. **General Settings**:
   - Client type: `OpenID Connect`
   - Client ID: `inferx_client`
   - Click **Next**
3. **Capability config**:
   - Turn on **Client authentication** (makes it confidential)
   - Leave **Standard flow** enabled
   - Click **Next**
4. **Login settings**:
   - Valid redirect URIs: `http://localhost:31250/*`
   - Valid post-logout redirect URIs: `http://localhost:31250/*`
   - Web origins: `http://localhost:31250`
   - Click **Save**
5. Go to the **Credentials** tab and set the client secret to:
   ```
   Nv0cPI1cAedpWYeEkZNL8PYqGrzIEOE7
   ```

---

## Step 3 — Create an Admin User

1. In the left sidebar, go to **Users** → **Create new user**
2. Fill in:
   - Username: `ixadmin`
   - Email: `yourname@gmail.com`
   - Email verified: on
3. Click **Create**
4. Go to the **Credentials** tab → **Set password**
5. Enter a password, turn off **Temporary**, click **Save**

---

## Step 4 — Verify Setup

Check the OpenID discovery endpoint is reachable from inside the cluster:

```bash
curl http://keycloak:8080/authn/realms/inferx/.well-known/openid-configuration
```

Or from the host:

```bash
curl http://localhost:31260/authn/realms/inferx/.well-known/openid-configuration
```

You should get a JSON response with `authorization_endpoint`, `token_endpoint`, etc.

---

## Done

The InferX dashboard and gateway are pre-configured to connect to Keycloak automatically. Once the realm and client exist, they will work without further changes.
