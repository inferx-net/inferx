# Copyright (c) 2021 Quark Container Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http:#www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

import json
import os
import time
from datetime import datetime, timezone
from urllib.parse import urlencode
import pytz

import requests
import markdown
import functools

from flask import (
    Blueprint,
    Flask,
    jsonify,
    redirect, url_for, session, 
    render_template,
    render_template_string,
    request,
    Response,
    send_from_directory,
    Blueprint
)

from authlib.integrations.flask_client import OAuth
from authlib.common.security import generate_token 

from threading import Thread

import logging
import sys
import multiprocessing

from werkzeug.middleware.proxy_fix import ProxyFix


# logger = logging.getLogger('gunicorn.error')
# sys.stdout = sys.stderr = logger.handlers[0].stream

app = Flask(__name__)
app.secret_key = os.environ.get("FLASK_SECRET", "supersecret")
SLACK_INVITE_URL = os.getenv(
    "SLACK_INVITE_URL",
    "https://join.slack.com/t/inferxcommunity/shared_invite/zt-3pp01352q-MM3CuRprsoeb68QKwygXjQ",
).strip()
DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS = os.getenv(
    "DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS",
    "false",
).lower() in ("1", "true", "yes")


@app.context_processor
def inject_dashboard_links():
    return {
        "slack_invite_url": SLACK_INVITE_URL,
        "dashboard_gateway_aligned_anonymous_access": DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS,
        "dashboard_gateway_aligned_anonymous_active": (
            DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS
            and session.get('access_token', '') == ''
        ),
    }

#Create a Blueprint with a common prefix
prefix_bp = Blueprint('prefix', __name__, url_prefix='')

def configure_logging():
    if "gunicorn" in multiprocessing.current_process().name.lower():
        logger = logging.getLogger('gunicorn.error')
        if logger.handlers:
            sys.stdout = sys.stderr = logger.handlers[0].stream
            app.logger.info("Redirecting stdout/stderr to Gunicorn logger.")
    else:
        app.logger.info("Running standalone Flask — no stdout/stderr redirection.")

configure_logging()


KEYCLOAK_URL = os.getenv('KEYCLOAK_URL', "http://192.168.0.22:31260/authn")
KEYCLOAK_REALM_NAME = os.getenv('KEYCLOAK_REALM_NAME', "inferx")
KEYCLOAK_CLIENT_ID = os.getenv('KEYCLOAK_CLIENT_ID', "infer_client")
KEYCLOAK_CLIENT_SECRET = os.getenv('KEYCLOAK_CLIENT_SECRET', "M2Dse5531tdtyipZdGizLEeoOVgziQRX")
KEYCLOAK_GOOGLE_IDP_ALIAS = os.getenv('KEYCLOAK_GOOGLE_IDP_ALIAS', "google").strip()
KEYCLOAK_GITHUB_IDP_ALIAS = os.getenv('KEYCLOAK_GITHUB_IDP_ALIAS', "github").strip()
FORCE_HTTPS_REDIRECTS = os.getenv('FORCE_HTTPS_REDIRECTS', 'false').lower() in ("1", "true", "yes")
SESSION_SIZE_DEBUG = os.getenv('SESSION_SIZE_DEBUG', 'false').lower() in ("1", "true", "yes")

server_metadata_url = f"{KEYCLOAK_URL}/realms/{KEYCLOAK_REALM_NAME}/.well-known/openid-configuration"

oauth = OAuth(app)
app.wsgi_app = ProxyFix(
    app.wsgi_app, 
    x_for=1,       # Number of trusted proxy hops
    x_proto=1,     # Trust X-Forwarded-Proto (HTTP/HTTPS)
    x_host=1,      # Trust X-Forwarded-Host (external host)
    x_port=1,      # Trust X-Forwarded-Port (external port)
    x_prefix=1  
)

keycloak = oauth.register(
    name='keycloak',
    client_id=KEYCLOAK_CLIENT_ID,
    client_secret=KEYCLOAK_CLIENT_SECRET,
    server_metadata_url=server_metadata_url,
    client_kwargs={
        'scope': 'openid email profile',
        'code_challenge_method': 'S256'  # Enable PKCE
    }
)

tls = False

apihostaddr = os.getenv('INFERX_APIGW_ADDR', "http://localhost:4000")
PUBLIC_API_BASE_URL = os.getenv('INFERX_PUBLIC_API_BASE_URL', "").strip()
ONBOARD_MAX_RETRIES = int(os.getenv('ONBOARD_MAX_RETRIES', '3'))
ONBOARD_BACKOFF_BASE_SEC = float(os.getenv('ONBOARD_BACKOFF_BASE_SEC', '0.5'))
ONBOARD_TIMEOUT_SEC = float(os.getenv('ONBOARD_TIMEOUT_SEC', '10'))
MAX_GPU_VRAM_MB_ENV = "INFERX_MAX_GPU_VRAM_MB"
MAX_GPU_COUNT_ENV = "INFERX_MAX_GPU_COUNT"


def load_max_gpu_vram_mb_override():
    raw = str(os.getenv(MAX_GPU_VRAM_MB_ENV, "0") or "").strip()
    if raw == "":
        return 0

    try:
        value = int(raw)
        if value < 0:
            raise ValueError("must be >= 0")
        return value
    except Exception as e:
        app.logger.warning(
            "failed to parse %s (%s), using node-derived max vRam",
            MAX_GPU_VRAM_MB_ENV,
            e,
        )
        return 0


MAX_GPU_VRAM_MB_OVERRIDE = load_max_gpu_vram_mb_override()


def load_max_gpu_count_override():
    raw = str(os.getenv(MAX_GPU_COUNT_ENV, "0") or "").strip()
    if raw == "":
        return 0

    try:
        value = int(raw)
        if value < 0:
            raise ValueError("must be >= 0")
        return value
    except Exception as e:
        app.logger.warning(
            "failed to parse %s (%s), using node/supported GPU count limits",
            MAX_GPU_COUNT_ENV,
            e,
        )
        return 0

VLLM_IMAGE_WHITELIST = [
    "vllm/vllm-openai:v0.12.0",
    "vllm/vllm-openai:v0.13.0",
    "vllm/vllm-openai:v0.14.0",
    "vllm/vllm-openai:v0.15.0",
    "vllm/vllm-openai:v0.16.0",
    "vllm/vllm-openai:v0.17.1",
    "vllm/vllm-omni:v0.14.0",
]

DEFAULT_GPU_RESOURCE_LOOKUP = {
    1: {"CPU": 10000, "Mem": 30000},
    2: {"CPU": 20000, "Mem": 60000},
    4: {"CPU": 20000, "Mem": 80000},
}
GPU_RESOURCE_LOOKUP_ENV = "INFERX_GPU_RESOURCE_LOOKUP_JSON"


def load_gpu_resource_lookup():
    raw = str(os.getenv(GPU_RESOURCE_LOOKUP_ENV, "") or "").strip()
    if raw == "":
        return dict(DEFAULT_GPU_RESOURCE_LOOKUP)

    def _invalid(reason: str):
        raise ValueError(f"{GPU_RESOURCE_LOOKUP_ENV} {reason}")

    try:
        parsed = json.loads(raw)
        if not isinstance(parsed, dict) or len(parsed) == 0:
            _invalid("must be a non-empty JSON object")

        normalized = {}
        for key, value in parsed.items():
            try:
                gpu_count = int(key)
            except Exception:
                _invalid(f"has non-integer GPU count key: {key!r}")
            if gpu_count <= 0:
                _invalid(f"has non-positive GPU count key: {gpu_count}")
            if not isinstance(value, dict):
                _invalid(f"entry for GPU count {gpu_count} must be an object")

            cpu = value.get("CPU")
            mem = value.get("Mem")
            if not isinstance(cpu, int) or isinstance(cpu, bool) or cpu <= 0:
                _invalid(f"entry for GPU count {gpu_count} has invalid CPU (must be positive integer)")
            if not isinstance(mem, int) or isinstance(mem, bool) or mem <= 0:
                _invalid(f"entry for GPU count {gpu_count} has invalid Mem (must be positive integer)")

            normalized[gpu_count] = {"CPU": cpu, "Mem": mem}

        return dict(sorted(normalized.items()))
    except Exception as e:
        app.logger.warning(
            "failed to parse %s (%s), using default lookup: %s",
            GPU_RESOURCE_LOOKUP_ENV,
            e,
            DEFAULT_GPU_RESOURCE_LOOKUP,
        )
        return dict(DEFAULT_GPU_RESOURCE_LOOKUP)


GPU_RESOURCE_LOOKUP = load_gpu_resource_lookup()
SUPPORTED_GPU_COUNTS = sorted(GPU_RESOURCE_LOOKUP.keys())
MAX_GPU_COUNT_OVERRIDE = load_max_gpu_count_override()


def resolve_effective_max_gpu_count(node_max_gpu_count: int) -> int:
    max_supported_count = SUPPORTED_GPU_COUNTS[-1] if len(SUPPORTED_GPU_COUNTS) > 0 else 0
    current_logic_max = node_max_gpu_count if node_max_gpu_count > 0 else max_supported_count
    if current_logic_max <= 0:
        return 0

    if MAX_GPU_COUNT_OVERRIDE <= 0 or MAX_GPU_COUNT_OVERRIDE >= current_logic_max:
        return current_logic_max

    return MAX_GPU_COUNT_OVERRIDE

DEFAULT_MODEL_ENVS = [
    ["LD_LIBRARY_PATH", "/usr/local/lib/python3.12/dist-packages/nvidia/cuda_nvrtc/lib/:$LD_LIBRARY_PATH"],
    ["VLLM_CUDART_SO_PATH", "/usr/local/cuda-12.1/targets/x86_64-linux/lib/libcudart.so.12"],
]

RESERVED_ENV_KEYS = {row[0] for row in DEFAULT_MODEL_ENVS}

FIXED_ENDPOINT = {"port": 8000, "schema": "Http", "probe": "/health"}
FIXED_STANDBY = {"gpu": "File", "pageable": "File", "pinned": "File"}
EMBEDDED_POLICY_REQUIRED_DEFAULTS = {
    "min_replica": 0,
    "max_replica": 1,
    "standby_per_node": 1,
}
DEFAULT_SAMPLE_QUERY_PROMPTS = [
    "Write a Python function that computes Fibonacci numbers. Explain time complexity.",
    "Translate the following Chinese text to English: \u4eca\u5929\u5929\u6c14\u5f88\u597d\u3002",
    "Explain general relativity in simple language.",
    "Write a legal contract clause about liability and indemnification.",
    "Summarize the plot of a fantasy novel involving dragons.",
    "Solve this calculus integral: \u222b x^3 log(x) dx",
    "Generate a JSON schema describing a user profile.",
    "Explain why emojis like \U0001F600\U0001F525\U0001F680 represent byte-level tokens.",
]

if FORCE_HTTPS_REDIRECTS:
    app.config["SESSION_COOKIE_SECURE"] = True


def external_url(endpoint: str, **values) -> str:
    values["_external"] = True
    if FORCE_HTTPS_REDIRECTS:
        values["_scheme"] = "https"
    return url_for(endpoint, **values)


def select_user_display_name(userinfo) -> str:
    given_name = str(userinfo.get("given_name", "")).strip()
    if given_name != "":
        return given_name

    name = str(userinfo.get("name", "")).strip()
    if name != "":
        return name.split()[0]

    preferred_username = str(userinfo.get("preferred_username", "")).strip()
    if preferred_username != "":
        return preferred_username.split("@")[0].split(".")[0]

    email = str(userinfo.get("email", "")).strip()
    if email != "":
        return email.split("@")[0].split(".")[0]

    return ""


def compact_legacy_session_payload():
    # Flask default session is cookie-based; remove legacy bulky values so
    # cookie size stays under browser limits.
    session.pop("token", None)
    session.pop("user", None)


def json_value_size_bytes(value) -> int:
    try:
        return len(json.dumps(value, separators=(",", ":"), default=str).encode("utf-8"))
    except Exception:
        return len(str(value).encode("utf-8"))


def session_key_sizes(payload: dict):
    key_sizes = []
    for key, value in payload.items():
        key_sizes.append((str(key), json_value_size_bytes(value)))
    key_sizes.sort(key=lambda row: row[1], reverse=True)
    return key_sizes


def estimate_signed_session_cookie_bytes(payload: dict) -> int:
    try:
        serializer = app.session_interface.get_signing_serializer(app)
        if serializer is None:
            return -1
        signed = serializer.dumps(payload)
        return len(signed.encode("utf-8"))
    except Exception:
        return -1


def log_session_cookie_size_comparison(token, userinfo):
    if not SESSION_SIZE_DEBUG:
        return

    current_payload = dict(session)
    legacy_payload = dict(current_payload)
    legacy_payload["user"] = userinfo
    legacy_payload["token"] = token
    legacy_payload["id_token"] = token.get("id_token")

    current_cookie_bytes = estimate_signed_session_cookie_bytes(current_payload)
    legacy_cookie_bytes = estimate_signed_session_cookie_bytes(legacy_payload)

    current_top = session_key_sizes(current_payload)[:8]
    legacy_top = session_key_sizes(legacy_payload)[:8]

    app.logger.warning(
        "session cookie size estimate (bytes): current=%s legacy(before-fix)=%s delta=%s current_top=%s legacy_top=%s",
        current_cookie_bytes,
        legacy_cookie_bytes,
        (legacy_cookie_bytes - current_cookie_bytes)
        if current_cookie_bytes >= 0 and legacy_cookie_bytes >= 0
        else "n/a",
        current_top,
        legacy_top,
    )


def token_expires_at_from_oauth_token(token) -> float:
    try:
        expires_at = float(token.get("expires_at", 0) or 0)
    except Exception:
        expires_at = 0

    if expires_at > 0:
        return expires_at

    try:
        expires_in = float(token.get("expires_in", 0) or 0)
    except Exception:
        expires_in = 0

    if expires_in > 0:
        return time.time() + expires_in

    return 0


@app.before_request
def compact_session_before_request():
    compact_legacy_session_payload()


def call_onboard_with_retry(access_token: str, sub: str):
    if access_token == "":
        raise Exception("missing access token for onboarding")

    if sub == "":
        raise Exception("missing sub claim for onboarding")

    url = "{}/onboard".format(apihostaddr.rstrip('/'))
    headers = {'Authorization': f'Bearer {access_token}'}

    delay = ONBOARD_BACKOFF_BASE_SEC
    last_error = "onboard failed"
    for attempt in range(1, ONBOARD_MAX_RETRIES + 1):
        try:
            resp = requests.post(url, headers=headers, timeout=ONBOARD_TIMEOUT_SEC)
            if resp.status_code == 200:
                data = resp.json()
                tenant_name = data.get("tenant_name", "")
                if tenant_name == "":
                    raise Exception("onboard response missing tenant_name")
                return data

            last_error = f"onboard attempt {attempt}/{ONBOARD_MAX_RETRIES} failed: HTTP {resp.status_code}, body={resp.text}"
        except Exception as e:
            last_error = f"onboard attempt {attempt}/{ONBOARD_MAX_RETRIES} exception: {e}"

        if attempt < ONBOARD_MAX_RETRIES:
            time.sleep(delay)
            delay *= 2

    raise Exception(last_error)


def normalize_public_api_base_url() -> str:
    base = PUBLIC_API_BASE_URL if PUBLIC_API_BASE_URL != "" else apihostaddr
    base = str(base or "").strip()
    if base == "":
        return "http://localhost:4000"
    if "://" not in base:
        base = f"https://{base}"
    return base.rstrip("/")


def store_onboard_session(sub: str, onboard_info, onboarding_apikey: str = "", onboarding_apikey_name: str = ""):
    tenant_name = onboard_info.get('tenant_name', '')
    apikey_from_onboard = str(onboard_info.get('apikey', '') or '').strip()
    apikey_name_from_onboard = str(onboard_info.get('apikey_name', '') or '').strip()
    if onboarding_apikey == "":
        onboarding_apikey = apikey_from_onboard
    if onboarding_apikey_name == "":
        onboarding_apikey_name = apikey_name_from_onboard
    session['sub'] = sub
    # Keep backward compatibility with existing single-tenant reads.
    session['tenant_name'] = tenant_name
    # Prepare for future multi-tenant UX (switcher/invite join).
    session['active_tenant_name'] = tenant_name
    session['tenant_names'] = [tenant_name] if tenant_name != '' else []
    session['tenant_role'] = onboard_info.get('role', '')
    session['tenant_created'] = bool(onboard_info.get('created', False))
    session['onboarding_inference_apikey'] = str(onboarding_apikey or '')
    session['onboarding_inference_apikey_name'] = str(onboarding_apikey_name or '')


def render_onboard_error(redirectpath: str, error_message: str, invite_code: str = ''):
    target = redirectpath
    if target == '':
        target = url_for('prefix.ListFunc')

    return render_template_string(
        """
        <!doctype html>
        <html lang="en">
        <head>
            <meta charset="utf-8">
            <meta name="viewport" content="width=device-width, initial-scale=1">
            <title>Onboarding Failed</title>
            <style>
                body { font-family: Arial, sans-serif; background: #f8fafc; color: #111827; margin: 0; padding: 24px; }
                .card { max-width: 640px; margin: 64px auto; background: #ffffff; border: 1px solid #e5e7eb; border-radius: 10px; padding: 24px; }
                h1 { margin-top: 0; font-size: 24px; }
                p { line-height: 1.5; }
                pre { white-space: pre-wrap; background: #f3f4f6; border: 1px solid #e5e7eb; padding: 12px; border-radius: 8px; }
                button { margin-top: 12px; padding: 10px 16px; border: 0; border-radius: 8px; cursor: pointer; background: #2563eb; color: white; font-size: 14px; }
            </style>
        </head>
        <body>
            <div class="card">
                <h1>Tenant onboarding failed</h1>
                <p>Your sign-in succeeded, but we could not finish creating/loading your tenant.</p>
                <pre>{{ error_message }}</pre>
                <form method="post" action="{{ url_for('prefix.onboard_retry') }}">
                    <input type="hidden" name="redirectpath" value="{{ target }}">
                    {% if invite_code %}
                    <input type="hidden" name="invite_code" value="{{ invite_code }}">
                    {% endif %}
                    <button type="submit">Try Again</button>
                </form>
            </div>
        </body>
        </html>
        """,
        error_message=error_message,
        target=target,
        invite_code=invite_code,
    ), 502


def post_login_flow(sub: str, access_token: str, redirectpath: str, invite_code: str):
    target = redirectpath if redirectpath != '' else url_for('prefix.ListFunc')

    if invite_code == '':
        invite_code = session.get('pending_invite_code', '')
    if invite_code != '':
        session['pending_invite_code'] = invite_code

    # TODO(invite): branch this flow when invite APIs are implemented:
    # - if invite_code exists, call join endpoint flow
    # - otherwise run personal-tenant onboarding flow
    onboard_info = call_onboard_with_retry(access_token, sub)
    store_onboard_session(sub, onboard_info)
    return target, invite_code

def is_token_expired():
    # Check if token exists and has expiration time
    access_token = str(session.get('access_token', '') or '')
    if access_token == "":
        return True

    expires_at = float(session.get('token_expires_at', 0) or 0)
    if expires_at <= 0:
        return True

    return expires_at < time.time()

def refresh_token_if_needed():
    access_token = str(session.get('access_token', '') or '')
    if access_token == "":
        return False

    if is_token_expired():
        refresh_token = str(session.get('refresh_token', '') or '')
        if refresh_token == "":
            return False
        try:
            new_token = keycloak.fetch_access_token(
                refresh_token=refresh_token,
                grant_type='refresh_token'
            )
            new_access_token = str(new_token.get('access_token', '') or '')
            if new_access_token == '':
                raise Exception("refresh token response missing access_token")

            session['access_token'] = new_access_token
            new_refresh_token = str(new_token.get('refresh_token', '') or '')
            if new_refresh_token != '':
                session['refresh_token'] = new_refresh_token
            session['token_expires_at'] = token_expires_at_from_oauth_token(new_token)
            compact_legacy_session_payload()
            return True
        except Exception as e:
            # Handle refresh error (e.g., invalid refresh token)
            print(f"Token refresh failed: {e}")
            session.pop('access_token', None)
            session.pop('refresh_token', None)
            session.pop('token_expires_at', None)
            compact_legacy_session_payload()
            return False
    return True

def not_require_login(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        compact_legacy_session_payload()
        access_token = session.get('access_token', '')
        if access_token == "":
            return func(*args, **kwargs)

        current_path = request.url
        redirect_uri = external_url('prefix.login', redirectpath=current_path)
        if is_token_expired() and not refresh_token_if_needed():
            return redirect(redirect_uri)

        return func(*args, **kwargs)
    return wrapper

def require_login(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        compact_legacy_session_payload()
        current_path = request.url
        redirect_uri = external_url('prefix.login', redirectpath=current_path)
        if session.get('access_token', '') == '':
            return redirect(redirect_uri)
        if is_token_expired() and not refresh_token_if_needed():
            return redirect(redirect_uri)

        return func(*args, **kwargs)
    return wrapper


def require_login_unless_gateway_aligned_anonymous_enabled(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        compact_legacy_session_payload()
        current_path = request.url
        redirect_uri = external_url('prefix.login', redirectpath=current_path)
        if session.get('access_token', '') == '':
            if DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS:
                return func(*args, **kwargs)
            return redirect(redirect_uri)
        if is_token_expired() and not refresh_token_if_needed():
            return redirect(redirect_uri)

        return func(*args, **kwargs)
    return wrapper


def is_gateway_aligned_anonymous_request():
    return DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS and session.get('access_token', '') == ''


def is_inferx_admin_user():
    if session.get('username', '') == 'inferx_admin':
        return True

    if session.get('access_token', '') == '':
        return False

    try:
        roles = listroles()
    except Exception:
        return False

    if not isinstance(roles, list):
        return False

    for role in roles:
        obj_type = str(role.get('objType', '')).lower()
        role_name = str(role.get('role', '')).lower()
        tenant = role.get('tenant', '')
        if obj_type == 'tenant' and role_name == 'admin' and tenant == 'system':
            return True

    return False


def has_any_tenant_or_namespace_admin_role():
    if session.get('access_token', '') == '':
        return False

    try:
        roles = listroles()
    except Exception:
        return False

    if not isinstance(roles, list):
        return False

    for role in roles:
        obj_type = str(role.get('objType', '')).lower()
        role_name = str(role.get('role', '')).lower()
        if role_name == 'admin' and obj_type in ('tenant', 'namespace'):
            return True

    return False


def has_inferx_admin_role(roles):
    if not isinstance(roles, list):
        return False

    for role in roles:
        obj_type = str(role.get('objType', '')).lower().strip()
        role_name = str(role.get('role', '')).lower().strip()
        tenant = str(role.get('tenant', '')).lower().strip()
        if obj_type == 'tenant' and role_name == 'admin' and tenant == 'system':
            return True

    return False


PUBLIC_TENANT_NAME = "public"


def is_public_tenant_name(tenant_name: str) -> bool:
    return str(tenant_name or "").strip().lower() == PUBLIC_TENANT_NAME


def can_view_public_tenant(roles=None):
    if session.get('username', '') == 'inferx_admin':
        return True

    if roles is not None:
        return has_inferx_admin_role(roles)

    return is_inferx_admin_user()


def can_access_public_tenant_in_dashboard(roles=None):
    if is_gateway_aligned_anonymous_request():
        return True

    return can_view_public_tenant(roles)


def resource_item_tenant_name(item) -> str:
    if not isinstance(item, dict):
        return ""

    tenant = str(item.get('tenant', '') or '').strip()
    if tenant != "":
        return tenant

    func = item.get('func')
    if isinstance(func, dict):
        return str(func.get('tenant', '') or '').strip()

    return ""


def filter_public_tenant_resource_items(items, include_public: bool):
    if include_public or not isinstance(items, list):
        return items

    return [
        item
        for item in items
        if not is_public_tenant_name(resource_item_tenant_name(item))
    ]


def filter_public_tenant_tenants(tenants, include_public: bool):
    if include_public or not isinstance(tenants, list):
        return tenants

    return [
        tenant
        for tenant in tenants
        if isinstance(tenant, dict)
        and not is_public_tenant_name(str(tenant.get('name', '') or '').strip())
    ]


def deny_public_tenant_request(tenant: str):
    if is_public_tenant_name(tenant) and not can_access_public_tenant_in_dashboard():
        return Response("No permission", status=403)

    return None


def has_admin_role_for_model(roles, target_tenant: str, target_namespace: str = ""):
    if not isinstance(roles, list):
        return False

    search_tenant = str(target_tenant or '').lower().strip()
    search_namespace = str(target_namespace or '').lower().strip()

    for role in roles:
        obj_type = str(role.get('objType', '')).lower().strip()
        role_name = str(role.get('role', '')).lower().strip()
        rb_tenant = str(role.get('tenant', '')).lower().strip()
        rb_namespace = str(role.get('namespace', '')).lower().strip()

        is_system_admin = obj_type == 'tenant' and rb_tenant == 'system' and role_name == 'admin'
        is_tenant_admin = obj_type == 'tenant' and rb_tenant == search_tenant and role_name == 'admin'
        is_namespace_admin = (
            obj_type == 'namespace'
            and rb_tenant == search_tenant
            and rb_namespace == search_namespace
            and role_name == 'admin'
        )

        if is_system_admin or is_tenant_admin or is_namespace_admin:
            return True

    return False


def require_admin(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        if not is_inferx_admin_user():
            return Response("No permission", status=403)
        return func(*args, **kwargs)
    return wrapper


def require_admin_unless_gateway_aligned_anonymous_enabled(func):
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        if DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS:
            return func(*args, **kwargs)
        if not is_inferx_admin_user():
            return Response("No permission", status=403)
        return func(*args, **kwargs)
    return wrapper

@prefix_bp.route('/login')
def login():
    redirectpath=request.args.get('redirectpath', '')
    invite_code=request.args.get('invite_code', '')
    idp_hint = str(request.args.get('idp', '') or '').strip()
    if idp_hint == '':
        idp_hint = str(request.args.get('idp_hint', '') or '').strip()
    return begin_auth_redirect(redirectpath, invite_code, idp_hint=idp_hint)


def begin_auth_redirect(redirectpath: str, invite_code: str, kc_action: str = '', idp_hint: str = ''):
    nonce = generate_token(20)
    session['keycloak_nonce'] = nonce
    if invite_code != '':
        session['pending_invite_code'] = invite_code
    redirect_uri = external_url('prefix.auth_callback', redirectpath=redirectpath, invite_code=invite_code)
    authorize_params = {
        "redirect_uri": redirect_uri,
        "nonce": nonce,
    }
    if kc_action != '':
        authorize_params["kc_action"] = kc_action
    if idp_hint != '':
        authorize_params["kc_idp_hint"] = idp_hint
    return keycloak.authorize_redirect(**authorize_params)


def build_login_entry_url(redirectpath: str, invite_code: str, idp: str = '') -> str:
    values = {}
    if redirectpath != '':
        values["redirectpath"] = redirectpath
    if invite_code != '':
        values["invite_code"] = invite_code
    if idp != '':
        values["idp"] = idp
    return url_for('prefix.login', **values)


@prefix_bp.route('/signup')
@prefix_bp.route('/register')
def signup():
    redirectpath=request.args.get('redirectpath', '')
    invite_code=request.args.get('invite_code', '')
    default_login_url = build_login_entry_url(redirectpath, invite_code)
    google_signup_url = default_login_url
    github_signup_url = default_login_url

    if KEYCLOAK_GOOGLE_IDP_ALIAS != '':
        google_signup_url = build_login_entry_url(redirectpath, invite_code, idp=KEYCLOAK_GOOGLE_IDP_ALIAS)
    if KEYCLOAK_GITHUB_IDP_ALIAS != '':
        github_signup_url = build_login_entry_url(redirectpath, invite_code, idp=KEYCLOAK_GITHUB_IDP_ALIAS)

    return render_template(
        'signup.html',
        google_signup_url=google_signup_url,
        github_signup_url=github_signup_url,
        login_url=default_login_url,
        has_google=KEYCLOAK_GOOGLE_IDP_ALIAS != '',
        has_github=KEYCLOAK_GITHUB_IDP_ALIAS != '',
    )

@prefix_bp.route('auth/callback')
def auth_callback():
    try:
        # Retrieve token and validate nonce
        token = keycloak.authorize_access_token()
        nonce = session.pop('keycloak_nonce', None)

        redirectpath=request.args.get('redirectpath', '')
        invite_code=request.args.get('invite_code', '')
        if invite_code == '':
            invite_code = session.get('pending_invite_code', '')
    
        if not nonce:
            raise Exception("Missing nonce in session")

        userinfo = keycloak.parse_id_token(token, nonce=nonce)  # Validate nonce
        sub = userinfo.get('sub', '')
        if sub == '':
            raise Exception("Missing sub claim in ID token")

        username = str(userinfo.get('preferred_username', '')).strip()
        if username == '':
            username = str(userinfo.get('email', '')).strip()

        session['username'] = username
        session['display_name'] = select_user_display_name(userinfo)
        session['access_token'] = str(token.get('access_token', '') or '')
        session['refresh_token'] = str(token.get('refresh_token', '') or '')
        session['token_expires_at'] = token_expires_at_from_oauth_token(token)
        session['id_token'] = str(token.get('id_token', '') or '')
        session['sub'] = sub
        compact_legacy_session_payload()
        log_session_cookie_size_comparison(token, userinfo)

        target = redirectpath if redirectpath != '' else url_for('prefix.ListFunc')
        try:
            target, invite_code = post_login_flow(
                sub,
                session['access_token'],
                redirectpath,
                invite_code,
            )
        except Exception as onboard_error:
            app.logger.error("Onboard failed for sub %s: %s", sub, onboard_error)
            return render_onboard_error(target, str(onboard_error), invite_code)

        return redirect(target)
    except Exception as e:
        return f"Authentication failed: {str(e)}", 403


@prefix_bp.route('/onboard/retry', methods=['POST'])
@require_login
def onboard_retry():
    redirectpath = request.form.get('redirectpath', '')
    target = redirectpath if redirectpath != '' else url_for('prefix.ListFunc')
    invite_code = request.form.get('invite_code', '')
    if invite_code == '':
        invite_code = session.get('pending_invite_code', '')
    sub = session.get('sub', '')

    access_token = session.get('access_token', '')
    try:
        target, invite_code = post_login_flow(
            sub,
            access_token,
            redirectpath,
            invite_code,
        )
        return redirect(target)
    except Exception as e:
        app.logger.error("Onboard retry failed for sub %s: %s", sub, e)
        return render_onboard_error(target, str(e), invite_code)

@prefix_bp.route('/logout')
def logout():
    # Keycloak logout endpoint
    end_session_endpoint = keycloak.load_server_metadata().get(
        'end_session_endpoint',
        f"{KEYCLOAK_URL}/realms/{KEYCLOAK_REALM_NAME}/protocol/openid-connect/logout",
    )

    id_token = session.get('id_token', '')
    # return redirect(end_session_endpoint)

    session.clear()

    logout_url = f"{end_session_endpoint}?post_logout_redirect_uri={external_url('prefix.Home')}"
    if id_token:
        logout_url += f"&id_token_hint={id_token}"
        
    return redirect(logout_url)

def getapikeys():
    access_token = session.get('access_token', '')
    # Include the access token in the Authorization header
    headers = {'Authorization': f'Bearer {access_token}'}
    
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    apikeys = json.loads(resp.content)

    return apikeys

@prefix_bp.route('/admin')
@require_login
def apikeys():
    return render_template(
        "admin.html"
    )

@prefix_bp.route('/generate_apikeys', methods=['GET'])
@require_login
def generate_apikeys():
    apikeys = getapikeys()
    return apikeys


@prefix_bp.route('/apikeys', methods=['PUT'])
@require_login
def create_apikey():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = request.get_json()
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.put(url, headers=headers, json=req)
    return (resp.text, resp.status_code, {'Content-Type': resp.headers.get('Content-Type', 'application/json')})

@prefix_bp.route('/apikeys', methods=['DELETE'])
@require_login
def delete_apikey():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = request.get_json()
    url = "{}/apikey/".format(apihostaddr)
    resp = requests.delete(url, headers=headers, json=req)
    return (resp.text, resp.status_code, {'Content-Type': resp.headers.get('Content-Type', 'application/json')})

def read_markdown_file(filename):
    """Read and convert Markdown file to HTML"""
    with open(filename, "r", encoding="utf-8") as f:
        content = f.read()
    return markdown.markdown(content)


def ReadFuncLog(namespace: str, funcId: str) -> str:
    req = qobjs_pb2.ReadFuncLogReq(
        namespace=namespace,
        funcName=funcId,
    )

    channel = grpc.insecure_channel("127.0.0.1:1237")
    stub = qobjs_pb2_grpc.QMetaServiceStub(channel)
    res = stub.ReadFuncLog(req)
    return res.content


def listfuncs(tenant: str, namespace: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/functions/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url, headers=headers)
    funcs = json.loads(resp.content)  

    return funcs

def list_tenantusers(role: str, tenant: str):
    role = str(role or "").strip()
    tenant = str(tenant or "").strip()
    if role == "" or tenant == "":
        return []

    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/rbac/tenantusers/{}/{}/".format(apihostaddr, role, tenant)
    try:
        resp = requests.get(url, headers=headers, timeout=30)
    except requests.exceptions.RequestException as e:
        app.logger.warning("list_tenantusers request failed role=%s tenant=%s: %s", role, tenant, e)
        return []

    if not resp.ok:
        app.logger.warning(
            "list_tenantusers non-OK response role=%s tenant=%s status=%s body=%r",
            role,
            tenant,
            resp.status_code,
            (resp.text or "")[:240],
        )
        return []

    body = (resp.text or "").strip()
    if body == "":
        app.logger.warning(
            "list_tenantusers empty response role=%s tenant=%s status=%s",
            role,
            tenant,
            resp.status_code,
        )
        return []

    try:
        funcs = json.loads(body)
    except Exception as e:
        app.logger.warning(
            "list_tenantusers invalid JSON role=%s tenant=%s status=%s body=%r error=%s",
            role,
            tenant,
            resp.status_code,
            body[:240],
            e,
        )
        return []

    return funcs

def list_namespaceusers(role: str, tenant: str, namespace: str):
    role = str(role or "").strip()
    tenant = str(tenant or "").strip()
    namespace = str(namespace or "").strip()
    if role == "" or tenant == "" or namespace == "":
        return []

    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/rbac/namespaceusers/{}/{}/{}/".format(apihostaddr, role, tenant, namespace)
    try:
        resp = requests.get(url, headers=headers, timeout=30)
    except requests.exceptions.RequestException as e:
        app.logger.warning(
            "list_namespaceusers request failed role=%s tenant=%s namespace=%s: %s",
            role,
            tenant,
            namespace,
            e,
        )
        return []

    if not resp.ok:
        app.logger.warning(
            "list_namespaceusers non-OK response role=%s tenant=%s namespace=%s status=%s body=%r",
            role,
            tenant,
            namespace,
            resp.status_code,
            (resp.text or "")[:240],
        )
        return []

    body = (resp.text or "").strip()
    if body == "":
        app.logger.warning(
            "list_namespaceusers empty response role=%s tenant=%s namespace=%s status=%s",
            role,
            tenant,
            namespace,
            resp.status_code,
        )
        return []

    try:
        funcs = json.loads(body)
    except Exception as e:
        app.logger.warning(
            "list_namespaceusers invalid JSON role=%s tenant=%s namespace=%s status=%s body=%r error=%s",
            role,
            tenant,
            namespace,
            resp.status_code,
            body[:240],
            e,
        )
        return []

    return funcs

def getfunc(tenant: str, namespace: str, funcname: str):
    resp, func = getfunc_response(tenant, namespace, funcname)
    if func is None:
        raise ValueError(
            f"invalid function response status={resp.status_code} tenant={tenant} namespace={namespace} name={funcname}"
        )
    return func


def getfunc_response(tenant: str, namespace: str, funcname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/function/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url, headers=headers)
    return resp, response_json_or_none(resp)


def listsnapshots(tenant: str, namespace: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/snapshots/{}/{}/".format(apihostaddr, tenant, namespace)
    resp = requests.get(url, headers=headers)
    func = json.loads(resp.content)
    return func


def listnodes():
    url = "{}/nodes/".format(apihostaddr)
    resp = requests.get(url)
    nodes = json.loads(resp.content)

    return nodes


def getnode(name: str):
    url = "{}/node/{}/".format(apihostaddr, name)
    resp = requests.get(url)
    func = json.loads(resp.content)

    return func


def json_error(message: str, status: int = 400):
    return jsonify({"error": message}), status


def response_json_or_none(resp):
    body = (resp.text or "").strip()
    if body == "":
        return None

    try:
        return resp.json()
    except ValueError:
        return None


def dashboard_href(endpoint: str, **params) -> str:
    href = url_for(endpoint)
    query = {}
    for key, value in params.items():
        text = str(value or "").strip()
        if text != "":
            query[key] = text

    if len(query) == 0:
        return href

    return f"{href}?{urlencode(query)}"


def extract_upstream_error_message(resp, payload) -> str:
    if isinstance(payload, dict):
        for key in ("error", "message", "detail"):
            value = payload.get(key)
            if isinstance(value, str) and value.strip() != "":
                return value.strip()

    if isinstance(payload, str) and payload.strip() != "":
        return payload.strip()

    if resp is None:
        return ""

    text = str(resp.text or "").strip()
    if text == "" or text.startswith("<"):
        return ""

    if len(text) > 240:
        return text[:237] + "..."

    return text


def is_upstream_resource_unavailable(resp, payload) -> bool:
    if resp is None:
        return False

    if resp.status_code in (404, 410):
        return True

    if resp.status_code != 400:
        return False

    detail = extract_upstream_error_message(resp, payload).lower()
    if detail == "":
        return False

    unavailable_markers = (
        "notexist",
        "not found",
        "doesn't exist",
        "doesnt exist",
        "no such",
    )
    return any(marker in detail for marker in unavailable_markers)


def render_resource_unavailable_page(
    *,
    resource_kind: str,
    resource_name: str,
    tenant: str = "",
    namespace: str = "",
    message: str,
    suggestion: str = "",
    primary_href: str,
    primary_label: str,
    secondary_href: str = "",
    secondary_label: str = "",
    detail: str = "",
    status: int = 404,
):
    return (
        render_template(
            "resource_unavailable.html",
            resource_kind=resource_kind,
            resource_name=resource_name,
            tenant=tenant,
            namespace=namespace,
            message=message,
            suggestion=suggestion,
            primary_href=primary_href,
            primary_label=primary_label,
            secondary_href=secondary_href,
            secondary_label=secondary_label,
            detail=detail,
        ),
        status,
    )


def render_resource_load_error_page(
    *,
    resource_kind: str,
    resource_name: str,
    tenant: str = "",
    namespace: str = "",
    primary_href: str,
    primary_label: str,
    secondary_href: str = "",
    secondary_label: str = "",
    upstream_status: int = 502,
    detail: str = "",
):
    suggestion = f"The backend returned HTTP {upstream_status}."
    if detail != "":
        suggestion += " You can go back and try again later."

    return render_resource_unavailable_page(
        resource_kind=resource_kind,
        resource_name=resource_name,
        tenant=tenant,
        namespace=namespace,
        message=f"This {resource_kind.lower()} page could not be loaded right now.",
        suggestion=suggestion,
        primary_href=primary_href,
        primary_label=primary_label,
        secondary_href=secondary_href,
        secondary_label=secondary_label,
        detail=detail,
        status=502,
    )


def gateway_headers(include_json: bool = False):
    access_token = session.get('access_token', '')
    headers = {}
    if access_token != "":
        headers["Authorization"] = f"Bearer {access_token}"
    if include_json:
        headers["Content-Type"] = "application/json"
    return headers


def get_node_max_vram_mb():
    if MAX_GPU_VRAM_MB_OVERRIDE > 0:
        return MAX_GPU_VRAM_MB_OVERRIDE

    nodes = listnodes()
    max_vram = 0
    iter_nodes = nodes if isinstance(nodes, list) else []
    for node in iter_nodes:
        try:
            vram = int(node["object"]["resources"]["GPUs"]["vRam"])
            if vram > max_vram:
                max_vram = vram
        except Exception:
            continue
    return max_vram


def get_node_capacity_limits():
    nodes = listnodes()
    max_vram = 0
    max_gpu_count = 0
    iter_nodes = nodes if isinstance(nodes, list) else []
    for node in iter_nodes:
        try:
            gpu_obj = (((node.get("object") or {}).get("resources") or {}).get("GPUs")) or {}
            gpu_map = gpu_obj.get("map", {})
            if isinstance(gpu_map, dict):
                gpu_count = len(gpu_map)
                if gpu_count > max_gpu_count:
                    max_gpu_count = gpu_count
            vram = int(gpu_obj["vRam"])
            if vram > max_vram:
                max_vram = vram
        except Exception:
            continue
    max_gpu_count = resolve_effective_max_gpu_count(max_gpu_count)
    if MAX_GPU_VRAM_MB_OVERRIDE > 0:
        max_vram = MAX_GPU_VRAM_MB_OVERRIDE
    return max_vram, max_gpu_count


def strip_reserved_command_args(commands):
    filtered = []
    i = 0
    while i < len(commands):
        token = commands[i]
        if token == "--model":
            i += 2
            continue
        if isinstance(token, str) and token.startswith("--model="):
            i += 1
            continue
        if token == "--tensor-parallel-size":
            i += 2
            continue
        if isinstance(token, str) and token.startswith("--tensor-parallel-size="):
            i += 1
            continue
        filtered.append(token)
        i += 1
    return filtered


def is_vllm_omni_image(image):
    return isinstance(image, str) and image.startswith("vllm/vllm-omni:")


def strip_omni_generated_command_args(commands):
    filtered = []
    for token in commands:
        if token in ("--trust-remote-code", "--omni"):
            continue
        filtered.append(token)
    return filtered


def build_generated_runtime_commands(hf_model, image, gpu_count, partial_commands):
    extra_commands = strip_reserved_command_args(partial_commands)
    if is_vllm_omni_image(image):
        extra_commands = strip_omni_generated_command_args(extra_commands)
        full_commands = [
            "vllm",
            "serve",
            hf_model,
            "--trust-remote-code",
            "--omni",
        ]
        full_commands.extend(extra_commands)
        return full_commands

    full_commands = ["--model", hf_model]
    full_commands.extend(extra_commands)
    full_commands.append(f"--tensor-parallel-size={gpu_count}")
    return full_commands


def extract_model_arg_from_commands(commands):
    i = 0
    while i < len(commands):
        token = commands[i]
        if token == "--model":
            if i + 1 < len(commands):
                return str(commands[i + 1])
            return ""
        if isinstance(token, str) and token.startswith("--model="):
            return token.split("=", 1)[1]
        i += 1
    return ""


def strip_reserved_envs(envs):
    filtered = []
    for pair in envs:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key = pair[0]
        value = pair[1]
        if not isinstance(key, str) or not isinstance(value, str):
            continue
        if key in RESERVED_ENV_KEYS:
            continue
        filtered.append([key, value])
    return filtered


def validate_partial_spec(spec, max_node_vram, max_node_gpu_count=0, editor_mode="basic"):
    if not isinstance(spec, dict):
        raise ValueError("`spec` must be an object")

    allowed_spec_keys = {"image", "commands", "resources", "envs", "policy", "sample_query"}
    unknown_spec_keys = set(spec.keys()) - allowed_spec_keys
    if unknown_spec_keys:
        raise ValueError(f"Unknown key in `spec`: {sorted(unknown_spec_keys)[0]}")

    for required_key in ("image", "commands", "resources"):
        if required_key not in spec:
            raise ValueError(f"Missing required `spec.{required_key}`")

    image = spec.get("image")
    if not isinstance(image, str) or image not in VLLM_IMAGE_WHITELIST:
        raise ValueError("`spec.image` must be one of the whitelisted vllm image tags")

    commands = spec.get("commands")
    if not isinstance(commands, list) or any(not isinstance(item, str) for item in commands):
        raise ValueError("`spec.commands` must be an array of strings")

    resources = spec.get("resources")
    if not isinstance(resources, dict):
        raise ValueError("`spec.resources` must be an object")
    if set(resources.keys()) != {"GPU"}:
        disallowed = sorted(set(resources.keys()) - {"GPU"})
        if disallowed:
            raise ValueError(f"Forbidden field in `spec.resources`: {disallowed[0]}")
        raise ValueError("`spec.resources.GPU` is required")

    gpu = resources.get("GPU")
    if not isinstance(gpu, dict):
        raise ValueError("`spec.resources.GPU` must be an object")
    allowed_gpu_keys = {"Count", "vRam"}
    if set(gpu.keys()) - allowed_gpu_keys:
        raise ValueError(f"Forbidden field in `spec.resources.GPU`: {sorted(set(gpu.keys()) - allowed_gpu_keys)[0]}")
    if "Count" not in gpu or "vRam" not in gpu:
        raise ValueError("`spec.resources.GPU.Count` and `spec.resources.GPU.vRam` are required")

    gpu_count = gpu.get("Count")
    vram = gpu.get("vRam")
    if not isinstance(gpu_count, int) or isinstance(gpu_count, bool):
        raise ValueError("`spec.resources.GPU.Count` must be an integer")
    if gpu_count not in GPU_RESOURCE_LOOKUP:
        allowed_counts = ", ".join(str(v) for v in SUPPORTED_GPU_COUNTS)
        raise ValueError(f"`spec.resources.GPU.Count` must be one of: {allowed_counts}")
    if max_node_gpu_count > 0 and gpu_count > max_node_gpu_count:
        raise ValueError(f"`spec.resources.GPU.Count` must be <= node max GPU count ({max_node_gpu_count})")
    if not isinstance(vram, int) or isinstance(vram, bool):
        raise ValueError("`spec.resources.GPU.vRam` must be an integer (MB)")
    if vram <= 0:
        raise ValueError("`spec.resources.GPU.vRam` must be > 0")
    if max_node_vram > 0 and vram > max_node_vram:
        raise ValueError(f"`spec.resources.GPU.vRam` must be <= node max ({max_node_vram})")

    envs = spec.get("envs", [])
    if envs is None:
        envs = []
    if not isinstance(envs, list):
        raise ValueError("`spec.envs` must be an array of [key, value] pairs")
    normalized_envs = []
    for idx, pair in enumerate(envs):
        if not isinstance(pair, list) or len(pair) != 2:
            raise ValueError(f"`spec.envs[{idx}]` must be [key, value]")
        key, value = pair
        if not isinstance(key, str) or not isinstance(value, str):
            raise ValueError(f"`spec.envs[{idx}]` key/value must be strings")
        normalized_envs.append([key, value])

    normalized_policy = None
    if "policy" in spec:
        policy = spec.get("policy")
        if not isinstance(policy, dict):
            raise ValueError("`spec.policy` must be an object")
        if set(policy.keys()) != {"Obj"}:
            forbidden = sorted(set(policy.keys()) - {"Obj"})
            if forbidden:
                raise ValueError(f"Forbidden field in `spec.policy`: {forbidden[0]}")
            raise ValueError("`spec.policy.Obj` is required")
        obj = policy.get("Obj")
        if not isinstance(obj, dict):
            raise ValueError("`spec.policy.Obj` must be an object")
        allowed_policy_obj_keys = {"queue_timeout", "scalein_timeout"}
        unknown_policy_keys = set(obj.keys()) - allowed_policy_obj_keys
        if unknown_policy_keys:
            raise ValueError(f"Unknown key in `spec.policy.Obj`: {sorted(unknown_policy_keys)[0]}")

        policy_obj = {}
        if "queue_timeout" in obj:
            value = obj["queue_timeout"]
            if not isinstance(value, (int, float)) or isinstance(value, bool):
                raise ValueError("`spec.policy.Obj.queue_timeout` must be a number")
            policy_obj["queue_timeout"] = float(value)
        if "scalein_timeout" in obj:
            value = obj["scalein_timeout"]
            if not isinstance(value, (int, float)) or isinstance(value, bool):
                raise ValueError("`spec.policy.Obj.scalein_timeout` must be a number")
            policy_obj["scalein_timeout"] = float(value)

        normalized_policy = {"Obj": policy_obj} if policy_obj else None

    normalized_sample_query = None
    if "sample_query" in spec:
        sample_query = spec.get("sample_query")
        if not isinstance(sample_query, dict):
            raise ValueError("`spec.sample_query` must be an object")
        # Deep-copy through JSON to keep only JSON-compatible values.
        normalized_sample_query = json.loads(json.dumps(sample_query))

    normalized_commands = [str(item) for item in commands] if editor_mode == "advanced" else strip_reserved_command_args(commands)

    return {
        "image": image,
        "commands": normalized_commands,
        "resources": {"GPU": {"Count": gpu_count, "vRam": vram}},
        "envs": strip_reserved_envs(normalized_envs),
        "policy": normalized_policy,
        "sample_query": normalized_sample_query,
    }


def build_sample_query(hf_model: str):
    return {
        "apiType": "text2text",
        "prompt": "write a quick sort algorithm.",
        "prompts": list(DEFAULT_SAMPLE_QUERY_PROMPTS),
        "path": "v1/completions",
        "body": {
            "model": hf_model,
            "max_tokens": "1000",
            "temperature": "0",
            "stream": "true",
        },
    }


def build_full_spec(hf_model: str, partial_spec, editor_mode="basic"):
    gpu_count = partial_spec["resources"]["GPU"]["Count"]
    gpu_vram = partial_spec["resources"]["GPU"]["vRam"]
    resource_row = GPU_RESOURCE_LOOKUP[gpu_count]

    if editor_mode == "advanced":
        full_commands = [str(item) for item in partial_spec["commands"]]
    else:
        full_commands = build_generated_runtime_commands(
            hf_model=hf_model,
            image=partial_spec["image"],
            gpu_count=gpu_count,
            partial_commands=partial_spec["commands"],
        )

    sample_query = partial_spec.get("sample_query")
    if isinstance(sample_query, dict):
        resolved_sample_query = json.loads(json.dumps(sample_query))
        body = resolved_sample_query.get("body")
        if not isinstance(body, dict):
            body = {}
            resolved_sample_query["body"] = body
        body["model"] = hf_model
    else:
        resolved_sample_query = build_sample_query(hf_model)

    spec = {
        "image": partial_spec["image"],
        "commands": full_commands,
        "resources": {
            "CPU": resource_row["CPU"],
            "Mem": resource_row["Mem"],
            "GPU": {
                "Type": "Any",
                "Count": gpu_count,
                "vRam": gpu_vram,
            },
        },
        "envs": [list(item) for item in DEFAULT_MODEL_ENVS] + partial_spec["envs"],
        "endpoint": dict(FIXED_ENDPOINT),
        "sample_query": resolved_sample_query,
        "standby": dict(FIXED_STANDBY),
    }

    policy = partial_spec.get("policy")
    if policy and policy.get("Obj"):
        full_policy_obj = dict(EMBEDDED_POLICY_REQUIRED_DEFAULTS)
        full_policy_obj.update(policy["Obj"])
        # runtime_config is no longer used by the dashboard create/edit flow.
        # Strip it defensively even if future UI changes accidentally include it.
        full_policy_obj.pop("runtime_config", None)
        spec["policy"] = {"Obj": full_policy_obj}

    return spec


def project_func_for_edit(full_func):
    if not isinstance(full_func, dict) or "func" not in full_func:
        raise ValueError("Invalid function response")
    func_obj = full_func.get("func")
    if not isinstance(func_obj, dict):
        raise ValueError("Invalid function object")

    tenant = str(func_obj.get("tenant", "")).strip()
    namespace = str(func_obj.get("namespace", "")).strip()
    name = str(func_obj.get("name", "")).strip()
    spec = (((func_obj.get("object") or {}).get("spec")) or {})
    if not isinstance(spec, dict):
        raise ValueError("Function spec missing")

    commands = spec.get("commands", [])
    if not isinstance(commands, list):
        commands = []
    image = spec.get("image", VLLM_IMAGE_WHITELIST[0])
    if not isinstance(image, str) or image == "":
        image = VLLM_IMAGE_WHITELIST[0]

    full_commands_for_advanced = [str(item) for item in commands]
    if is_vllm_omni_image(image):
        omni_partial = [str(item) for item in commands]
        if len(omni_partial) >= 3 and omni_partial[0] == "vllm" and omni_partial[1] == "serve":
            omni_partial = omni_partial[3:]
        omni_partial = strip_omni_generated_command_args(omni_partial)
        clean_commands = [str(item) for item in strip_reserved_command_args(omni_partial)]
    else:
        clean_commands = [str(item) for item in strip_reserved_command_args(commands)]

    envs = spec.get("envs", [])
    if not isinstance(envs, list):
        envs = []
    clean_envs = []
    for pair in envs:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key, value = pair
        if isinstance(key, str) and isinstance(value, str) and key not in RESERVED_ENV_KEYS:
            clean_envs.append([key, value])

    gpu_spec = (((spec.get("resources") or {}).get("GPU")) or {})
    gpu_count = gpu_spec.get("Count", 1)
    gpu_vram = gpu_spec.get("vRam", 0)
    if not isinstance(gpu_count, int):
        gpu_count = 1
    if not isinstance(gpu_vram, int):
        gpu_vram = 0

    policy_obj = ((((spec.get("policy") or {}).get("Obj")) or {}))
    projected_policy_obj = {}
    if isinstance(policy_obj, dict):
        if "queue_timeout" in policy_obj and isinstance(policy_obj["queue_timeout"], (int, float)) and not isinstance(policy_obj["queue_timeout"], bool):
            projected_policy_obj["queue_timeout"] = float(policy_obj["queue_timeout"])
        if "scalein_timeout" in policy_obj and isinstance(policy_obj["scalein_timeout"], (int, float)) and not isinstance(policy_obj["scalein_timeout"], bool):
            projected_policy_obj["scalein_timeout"] = float(policy_obj["scalein_timeout"])

    sample_query = spec.get("sample_query", {})
    hf_model = ""
    if isinstance(sample_query, dict):
        body = sample_query.get("body", {})
        if isinstance(body, dict):
            model_value = body.get("model", "")
            if isinstance(model_value, str):
                hf_model = model_value.strip()
    if hf_model == "":
        hf_model = extract_model_arg_from_commands(commands).strip()

    projected_sample_query = None
    if isinstance(sample_query, dict):
        projected_sample_query = json.loads(json.dumps(sample_query))

    basic_spec = {
        "image": image,
        "commands": clean_commands,
        "resources": {"GPU": {"Count": gpu_count, "vRam": gpu_vram}},
        "envs": clean_envs,
        **({"sample_query": projected_sample_query} if projected_sample_query is not None else {}),
        **({"policy": {"Obj": projected_policy_obj}} if projected_policy_obj else {}),
    }
    projected_spec = json.loads(json.dumps(basic_spec))
    projected_spec["commands"] = full_commands_for_advanced
    advanced_spec = json.loads(json.dumps(projected_spec))

    return {
        "tenant": tenant,
        "namespace": namespace,
        "name": name,
        "hf_model": hf_model,
        "spec": projected_spec,
        "basic_spec": basic_spec,
        "advanced_spec": advanced_spec,
    }


def parse_edit_key(edit_key: str):
    parts = [part.strip() for part in (edit_key or "").split("/")]
    if len(parts) != 3 or any(part == "" for part in parts):
        raise ValueError("`edit` must be `<tenant>/<namespace>/<name>`")
    return parts[0], parts[1], parts[2]

def listroles():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/rbac/roles/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    roles = json.loads(resp.content)

    return roles

def listtenants():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/objects/tenant/system/system/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    tenants = json.loads(resp.content)

    return tenants

def listnamespaces():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/objects/namespace///".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    namespaces = json.loads(resp.content)

    return namespaces

def listpods(tenant: str, namespace: str, funcname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/pods/{}/{}/{}/".format(apihostaddr, tenant, namespace, funcname)
    resp = requests.get(url, headers=headers)
    pods = json.loads(resp.content)

    return pods


def getpod(tenant: str, namespace: str, podname: str):
    resp, pod = getpod_response(tenant, namespace, podname)
    if pod is None:
        raise ValueError(
            f"invalid pod response status={resp.status_code} tenant={tenant} namespace={namespace} name={podname}"
        )
    return pod


def getpod_response(tenant: str, namespace: str, podname: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/pod/{}/".format(apihostaddr, podname)
    resp = requests.get(url, headers=headers)
    return resp, response_json_or_none(resp)


def getpodaudit(tenant: str, namespace: str, fpname: str, fprevision: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/podauditlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, fpname, fprevision, id
    )
    resp = requests.get(url, headers=headers)
    logs = json.loads(resp.content)

    return logs

def GetSnapshotAudit(tenant: str, namespace: str, funcname: str, revision: int):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/SnapshotSchedule/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, funcname, revision
    )
    resp = requests.get(url, headers=headers)
    fails = json.loads(resp.content)
    return fails

def GetFailLogs(tenant: str, namespace: str, funcname: str, revision: int):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/faillogs/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision
    )
    resp = requests.get(url, headers=headers)
    fails = json.loads(resp.content)

    return fails


def GetFailLog(tenant: str, namespace: str, funcname: str, revision: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/faillog/{}/{}/{}/{}/{}".format(
        apihostaddr, tenant, namespace, funcname, revision, id
    )
    resp = requests.get(url, headers=headers)
    
    fail = json.loads(resp.content)
    fail["log"] = fail["log"].replace("\n", "<br>")
    return fail["log"]


def readpodlog(tenant: str, namespace: str, funcname: str, version: int, id: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/podlog/{}/{}/{}/{}/{}/".format(
        apihostaddr, tenant, namespace, funcname, version, id
    )
    resp = requests.get(url, headers=headers)
    log = resp.content.decode()
    log = log.replace("\n", "<br>")
    log = log.replace("    ", "&emsp;")
    return log


def getrest(tenant: str, namespace: str, name: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    req = "{}/sampleccall/{}/{}/{}/".format(apihostaddr, tenant, namespace, name)
    resp = requests.get(req, stream=False, headers=headers).text
    return resp


def build_sample_rest_call_for_ui(tenant: str, namespace: str, funcname: str, sample_query, apikey: str):
    if not isinstance(sample_query, dict):
        return ""

    path = str(sample_query.get("path", "v1/completions") or "v1/completions").strip()
    path = path.lstrip("/")
    body = sample_query.get("body", {})
    if not isinstance(body, dict):
        body = {}
    body_for_ui = dict(body)

    prompt = sample_query.get("prompt")
    api_type = str(sample_query.get("apiType", "") or "").strip().lower()
    if (
        isinstance(prompt, str)
        and prompt.strip() != ""
        and api_type == "text2text"
        and "prompt" not in body_for_ui
        and "messages" not in body_for_ui
        and "input" not in body_for_ui
    ):
        body_for_ui["prompt"] = prompt

    token = str(apikey or "").strip()
    if token == "":
        token = "<INFERENCE_API_KEY(Find or create one on Admin|Apikeys page)>"

    body_json = json.dumps(body_for_ui, ensure_ascii=False)
    body_json = body_json.replace("'", "'\"'\"'")
    base_url = normalize_public_api_base_url()
    url = f"{base_url}/funccall/{tenant}/{namespace}/{funcname}/{path}"
    tenant_name = str(tenant or "").strip().lower()
    include_auth_header = tenant_name != "public"
    auth_header_line = ""
    if include_auth_header:
        auth_header_line = f"  -H 'Authorization: Bearer {token}' \\\n"

    return (
        f"curl -X POST {url} \\\n"
        f"  -H 'Content-Type: application/json' \\\n"
        f"{auth_header_line}"
        f"  -d '{body_json}'"
    )


@prefix_bp.route('/text2img', methods=['POST'])
@not_require_login
def text2img():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {
            "Content-Type": "application/json",
        }
    else:
        headers = {
            'Authorization': f'Bearer {access_token}',
            "Content-Type": "application/json",
        }
    req = request.get_json()
    
    prompt = req["prompt"]
    tenant = req.get("tenant")
    namespace = req.get("namespace")
    funcname = req.get("funcname")
    
    func = getfunc(tenant, namespace, funcname)

    sample = func["func"]["object"]["spec"]["sample_query"]

    import copy
    postreq = copy.deepcopy(sample["body"])

    # Locate and replace the prompt within the nested messages structure
    try:
        # Most models follow: messages[0] -> content[0] -> text
        # We replace the placeholder text with the user-provided prompt
        postreq["messages"][0]["content"][0]["text"] = prompt
    except (KeyError, IndexError):
        # Fallback in case the structure is slightly different
        print("Warning: Could not find nested text field, falling back to top-level prompt.")
        postreq["prompt"] = prompt

    url = "{}/funccall/{}/{}/{}/{}".format(apihostaddr, tenant, namespace, funcname, sample["path"] )

    # Stream the response from OpenAI API
    resp = requests.post(url, headers=headers, json=postreq, stream=True)

    # excluded_headers = ['content-encoding', 'content-length', 'transfer-encoding', 'connection']
    excluded_headers = []
    headers = [(name, value) for (name, value) in resp.raw.headers.items() if name.lower() not in excluded_headers]
    return Response(resp.iter_content(1024000), resp.status_code, headers)


@prefix_bp.route('/text2audio', methods=['POST'])
@not_require_login
def text2audio():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {
            "Content-Type": "application/json",
        }
    else:
        headers = {
            'Authorization': f'Bearer {access_token}',
            "Content-Type": "application/json",
        }
    req = request.get_json()
    
    prompt = req["prompt"]
    tenant = req.get("tenant")
    namespace = req.get("namespace")
    funcname = req.get("funcname")
    
    func = getfunc(tenant, namespace, funcname)

    sample = func["func"]["object"]["spec"]["sample_query"]

    import copy
    postreq = copy.deepcopy(sample["body"])

    # Locate and replace the prompt within the nested messages structure
    try:
        # Most models follow: messages[0] -> content[0] -> text
        # We replace the placeholder text with the user-provided prompt
        postreq["input"] = prompt
    except (KeyError, IndexError):
        # Fallback in case the structure is slightly different
        print("Warning: Could not find nested text field, falling back to top-level prompt.")
        postreq["prompt"] = prompt

    url = "{}/funccall/{}/{}/{}/{}".format(apihostaddr, tenant, namespace, funcname, sample["path"] )

    # Stream the response from OpenAI API
    resp = requests.post(url, headers=headers, json=postreq, stream=True)

    # excluded_headers = ['content-encoding', 'content-length', 'transfer-encoding', 'connection']
    excluded_headers = []
    headers = [(name, value) for (name, value) in resp.raw.headers.items() if name.lower() not in excluded_headers]
    return Response(resp.iter_content(1024000), resp.status_code, headers)


@prefix_bp.route('/generate_tenants', methods=['GET'])
@require_login_unless_gateway_aligned_anonymous_enabled
def generate_tenants():
    tenants = listtenants()
    tenants = filter_public_tenant_tenants(
        tenants,
        include_public=can_access_public_tenant_in_dashboard(),
    )
    print("tenants ", tenants)
    return tenants

@prefix_bp.route('/generate_namespaces', methods=['GET'])
@require_login_unless_gateway_aligned_anonymous_enabled
def generate_namespaces():
    namespaces = listnamespaces()
    namespaces = filter_public_tenant_resource_items(
        namespaces,
        include_public=can_access_public_tenant_in_dashboard(),
    )
    print("namespaces ", namespaces)
    return namespaces

@prefix_bp.route('/generate_roles', methods=['GET'])
@require_login
def generate_roles():
    roles = listroles()
    print("roles ", roles)
    return roles

@prefix_bp.route('/generate_funcs', methods=['GET'])
@require_login_unless_gateway_aligned_anonymous_enabled
def generate_funcs():
    funcs = listfuncs("", "")
    funcs = filter_public_tenant_resource_items(
        funcs,
        include_public=can_access_public_tenant_in_dashboard(),
    )
    return funcs

@prefix_bp.route('/generate_tenantuser', methods=['GET'])
@require_login
def generate_tenantuser():
    role = request.args.get('role')
    tenant = request.args.get('tenant')
    users = list_tenantusers(role, tenant)
    return users

@prefix_bp.route('/generate_namespaceuser', methods=['GET'])
@require_login
def generate_namespaceuser():
    role = request.args.get('role')
    tenant = request.args.get('tenant')
    namespace = request.args.get('namespace')
    users = list_namespaceusers(role, tenant, namespace)
    return users

@prefix_bp.route('/generate', methods=['POST'])
@not_require_login
def generate():
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {
            "Content-Type": "application/json",
        }
    else:
        headers = {
            'Authorization': f'Bearer {access_token}',
            "Content-Type": "application/json",
        }
    # Parse input JSON from the request
    req = request.get_json()
    
    prompt = req["prompt"]
    tenant = req.get("tenant")
    namespace = req.get("namespace")
    funcname = req.get("funcname")
    
    func = getfunc(tenant, namespace, funcname)

    sample = func["func"]["object"]["spec"]["sample_query"]
    map = sample["body"]

    postreq = {
        "prompt": prompt
    }

    isOpenAi = sample["apiType"] == "openai"

    if sample["apiType"] == "llava":
        postreq["image"] = req.get("image")

    for index, (key, value) in enumerate(map.items()):
        postreq[key] = value

    url = "{}/funccall/{}/{}/{}/{}".format(apihostaddr, tenant, namespace, funcname, sample["path"] )

    # Stream the response from OpenAI API
    response = requests.post(url, headers=headers, json=postreq, stream=True)
    headers = response.headers
    def stream_openai():
        try:
            if response.status_code == 200:
                if isOpenAi:
                    # Iterate over streamed chunks and yield them
                    for data in response.iter_lines():
                        if data:
                            s = data.decode("utf-8")
                            lines = s.split("data:")
                            for line in lines:  
                                if "[DONE]" in line:
                                    continue
                                if len(line) != 0:
                                    # Parse the line as JSON
                                    parsed_line = json.loads(line)
                                    # Extract and print the content delta
                                    if "choices" in parsed_line:
                                        delta = parsed_line["choices"][0]["text"]
                                        yield delta
                                    else:
                                        yield line
                else:
                    for chunk in response.iter_content(chunk_size=1):
                        if chunk:
                            yield(chunk)
            else:
                for chunk in response.iter_content(chunk_size=1):
                    if chunk:
                        yield(chunk)


        except Exception as e:
            yield f"Error: {str(e)}"

    responseheaders = {
        "tcpconn_latency_header": headers["tcpconn_latency_header"],
        "ttft_latency_header": headers["ttft_latency_header"]
    }

    # Return a streaming response
    return Response(stream_openai(), headers = responseheaders, content_type='text/plain')



def stream_response(response):
    try:
        for chunk in response.iter_content(chunk_size=128):
            yield chunk
    finally:
        response.close()


def parse_inferx_timeout_seconds(raw_value, default_value: float = 60.0, min_value: float = 1.0, max_value: float = 600.0) -> float:
    try:
        value = float(str(raw_value or "").strip())
    except Exception:
        value = default_value

    if value < min_value:
        return min_value
    if value > max_value:
        return max_value
    return value

@prefix_bp.route('/proxy/<path:path>', methods=['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'])
@not_require_login
def proxy(path):
    # TODO(auth-hardening): migrate dashboard JS calls to send explicit Authorization
    # headers (for example via a dedicated fetch wrapper) and stop implicit session
    # token injection in this generic proxy endpoint.
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    normalized_path = path.lstrip('/')
    # Keep public funccall requests anonymous. Some gateway configs reject
    # dashboard session tokens on /funccall while allowing unauthenticated
    # access for public tenant models.
    is_public_funccall = normalized_path.startswith("funccall/public/")
    has_client_auth = any(key.lower() == 'authorization' for key in headers)
    if access_token != "" and not has_client_auth and not is_public_funccall:
        headers["Authorization"] = f'Bearer {access_token}'
    
    # Construct the full URL for the backend request
    url = f"{apihostaddr}/{path}"
    # Keep proxy read-timeout slightly above client-declared inference timeout so
    # backend timeout responses are returned directly when possible.
    requested_timeout_sec = parse_inferx_timeout_seconds(request.headers.get("X-Inferx-Timeout"), default_value=60.0)
    proxy_read_timeout_sec = min(requested_timeout_sec + 5.0, 600.0)
    connect_timeout_sec = 10.0

    try:
        resp = requests.request(
            method=request.method,
            url=url,
            headers=headers,
            data=request.get_data(),
            cookies=request.cookies,
            allow_redirects=False,
            timeout=(connect_timeout_sec, proxy_read_timeout_sec),
            stream=True
        )
    except requests.exceptions.Timeout:
        return Response(
            f"Upstream request timed out after {proxy_read_timeout_sec:.0f}s (requested timeout={requested_timeout_sec:.0f}s).",
            status=504,
            mimetype='text/plain',
        )
    except requests.exceptions.RequestException as e:
        return Response(f"Error connecting to backend server: {e}", status=502)
    
    # Exclude hop-by-hop headers as per RFC 2616 section 13.5.1
    excluded_headers = ['content-encoding', 'transfer-encoding', 'connection']
    headers = [(name, value) for name, value in resp.raw.headers.items() if name.lower() not in excluded_headers]
    
    # Create a Flask response object with the backend server's response
    response = Response(stream_response(resp), resp.status_code, headers)
    return response

@prefix_bp.route('/proxy1/<path:path>', methods=['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'])
@require_login
def proxy1(path):
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    if access_token != "":
        headers["Authorization"] = f'Bearer {access_token}'
    
    # Construct the full URL for the backend request
    url = f"{apihostaddr}/{path}"

    try:
        resp = requests.request(
            method=request.method,
            url=url,
            headers=headers,
            params=request.args,
            data=request.get_data(),
            cookies=request.cookies,
            allow_redirects=False,
            timeout=60,
            stream=False
        )
    except requests.exceptions.RequestException as e:
        print("error ....")
        return Response(f"Error connecting to backend server: {e}", status=502, mimetype='text/plain')
    
    response = Response(resp.content, resp.status_code, mimetype='text/plain')
    # for name, value in resp.headers.items():
    #     if name.lower() not in ['content-encoding', 'transfer-encoding', 'connection']:
    #         response.headers[name] = value

    return response
    


@app.route("/healthz")
def healthz():
    return ("ok", 200)


@prefix_bp.route("/intro")
@require_login
def md():
    # name = request.args.get("name")
    name = 'home.md'
    md_content = read_markdown_file("doc/"+name)
    return render_template(
        "markdown.html", md_content=md_content
    )

@prefix_bp.route('/doc/<path:filename>')
@require_login
def route_build_files(filename):
    root_dir = os.path.dirname(os.getcwd()) + "/doc"
    return send_from_directory(root_dir, filename)

@prefix_bp.route("/funclog")
@require_login
def funclog():
    namespace = request.args.get("namespace")
    funcId = request.args.get("funcId")
    funcName = request.args.get("funcName")
    log = ReadFuncLog(namespace, funcId)
    output = log.replace("\n", "<br>")
    return render_template(
        "log.html", namespace=namespace, funcId=funcId, funcName=funcName, log=output
    )


@prefix_bp.route("/nodes_max_vram", methods=["GET"])
@require_login
def nodes_max_vram():
    try:
        max_vram, max_gpu_count = get_node_capacity_limits()
        return jsonify({"max_vram": max_vram, "max_gpu_count": max_gpu_count})
    except Exception as e:
        return json_error(f"failed to read nodes: {e}", 502)


@prefix_bp.route("/func_create", methods=["GET"])
@require_login
def FuncCreate():
    edit_key = request.args.get("edit", "").strip()
    initial_model_data = None
    page_mode = "create"

    if edit_key != "":
        try:
            tenant, namespace, name = parse_edit_key(edit_key)
        except ValueError as e:
            return json_error(str(e), 400)

        deny_resp = deny_public_tenant_request(tenant)
        if deny_resp is not None:
            return deny_resp

        try:
            full_func = getfunc(tenant, namespace, name)
            deny_resp = deny_public_tenant_request(resource_item_tenant_name(full_func))
            if deny_resp is not None:
                return deny_resp
            initial_model_data = project_func_for_edit(full_func)
            page_mode = "update"
        except Exception as e:
            return json_error(f"failed to load model for edit: {e}", 502)

    return render_template(
        "func_create.html",
        page_mode=page_mode,
        edit_key=edit_key,
        initial_model_data=initial_model_data,
        image_options=VLLM_IMAGE_WHITELIST,
        gpu_count_options=SUPPORTED_GPU_COUNTS,
        default_advanced_sample_query_template=build_sample_query("Qwen/Qwen2.5-72B-Instruct"),
    )


@prefix_bp.route("/func_save", methods=["POST"])
@require_login
def func_save():
    req = request.get_json(silent=True)
    if not isinstance(req, dict):
        return json_error("Request body must be a JSON object", 400)

    allowed_top_level_keys = {"mode", "tenant", "namespace", "name", "hf_model", "spec", "editor_mode"}
    unknown_top_level_keys = set(req.keys()) - allowed_top_level_keys
    if unknown_top_level_keys:
        return json_error(f"Unknown top-level key: {sorted(unknown_top_level_keys)[0]}", 400)

    for key in ("mode", "tenant", "namespace", "name", "hf_model", "spec"):
        if key not in req:
            return json_error(f"Missing required field: `{key}`", 400)

    mode = req.get("mode")
    if mode not in ("create", "update"):
        return json_error("`mode` must be \"create\" or \"update\"", 400)
    editor_mode = req.get("editor_mode", "basic")
    if editor_mode not in ("basic", "advanced"):
        return json_error("`editor_mode` must be \"basic\" or \"advanced\"", 400)

    tenant = req.get("tenant")
    namespace = req.get("namespace")
    name = req.get("name")
    hf_model = req.get("hf_model")
    app.logger.info(
        "func_save received hf_model=%r tenant=%r namespace=%r name=%r mode=%r editor_mode=%r",
        hf_model,
        tenant,
        namespace,
        name,
        mode,
        editor_mode,
    )
    spec = req.get("spec")

    for field_name, field_value in (("tenant", tenant), ("namespace", namespace), ("name", name), ("hf_model", hf_model)):
        if not isinstance(field_value, str) or field_value.strip() == "":
            return json_error(f"`{field_name}` must be a non-empty string", 400)

    tenant = tenant.strip()
    namespace = namespace.strip()
    name = name.strip()
    hf_model = hf_model.strip()
    app.logger.info(
        "func_save normalized hf_model=%r tenant=%r namespace=%r name=%r mode=%r editor_mode=%r",
        hf_model,
        tenant,
        namespace,
        name,
        mode,
        editor_mode,
    )

    try:
        max_node_vram, max_node_gpu_count = get_node_capacity_limits()
    except Exception as e:
        return json_error(f"failed to read node capacity limits: {e}", 502)

    try:
        partial_spec = validate_partial_spec(
            spec,
            max_node_vram,
            max_node_gpu_count=max_node_gpu_count,
            editor_mode=editor_mode,
        )
        full_spec = build_full_spec(hf_model, partial_spec, editor_mode=editor_mode)
        app.logger.info(
            "func_save built spec model fields editor_mode=%r command_model=%r sample_model=%r",
            editor_mode,
            (
                full_spec.get("commands", [None, None])[1]
                if isinstance(full_spec.get("commands"), list) and len(full_spec.get("commands", [])) > 1
                else None
            ),
            (
                (((full_spec.get("sample_query") or {}).get("body") or {}).get("model"))
                if isinstance(full_spec.get("sample_query"), dict)
                else None
            ),
        )
    except ValueError as e:
        return json_error(str(e), 400)
    except Exception as e:
        return json_error(f"failed to build full spec: {e}", 500)

    gateway_req = {
        "type": "function",
        "tenant": tenant,
        "namespace": namespace,
        "name": name,
        "object": {
            "spec": full_spec
        }
    }

    gateway_method = "PUT" if mode == "create" else "POST"
    gateway_url = f"{apihostaddr}/object/"
    app.logger.info(
        "func_save forwarding method=%s url=%s command_model=%r sample_model=%r",
        gateway_method,
        gateway_url,
        (
            gateway_req["object"]["spec"].get("commands", [None, None])[1]
            if isinstance(gateway_req["object"]["spec"].get("commands"), list)
            and len(gateway_req["object"]["spec"].get("commands", [])) > 1
            else None
        ),
        ((((gateway_req["object"]["spec"].get("sample_query") or {}).get("body") or {}).get("model"))),
    )
    try:
        resp = requests.request(
            gateway_method,
            gateway_url,
            headers=gateway_headers(include_json=True),
            json=gateway_req,
            timeout=60,
        )
    except requests.exceptions.RequestException as e:
        return json_error(f"Error connecting to gateway: {e}", 502)

    return (
        resp.text,
        resp.status_code,
        {"Content-Type": resp.headers.get("Content-Type", "application/json")},
    )


@prefix_bp.route("/")
@not_require_login
def Home():
    if session.get('access_token', '') != '':
        return redirect(url_for('prefix.ListFunc'))
    if DASHBOARD_GATEWAY_ALIGNED_ANONYMOUS_ACCESS:
        return redirect(url_for('prefix.ListFunc'))

    return render_template(
        "entry.html",
        signup_url=url_for('prefix.signup'),
        login_url=url_for('prefix.login'),
    )


@prefix_bp.route("/listfunc")
@require_login_unless_gateway_aligned_anonymous_enabled
def ListFunc():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    funcs = None
    if tenant is None:
        funcs = listfuncs("", "")
    elif namespace is None:
        funcs = listfuncs(tenant, "")
    else:
        funcs = listfuncs(tenant, namespace)

    roles = []
    if session.get('access_token', '') != '':
        try:
            roles = listroles()
        except Exception:
            roles = []
    if not isinstance(roles, list):
        roles = []

    is_inferx_admin = can_view_public_tenant(roles)
    funcs = filter_public_tenant_resource_items(
        funcs,
        include_public=can_access_public_tenant_in_dashboard(roles),
    )

    count = 0
    gpucount = 0
    vram = 0
    cpu = 0 
    memory = 0
    for func in funcs:
        try:
            row_func = func.get('func', {}) if isinstance(func, dict) else {}
            row_tenant = str(row_func.get('tenant', '') or '')
            row_namespace = str(row_func.get('namespace', '') or '')
            if isinstance(func, dict):
                func['can_edit_delete'] = has_admin_role_for_model(roles, row_tenant, row_namespace)
        except Exception:
            if isinstance(func, dict):
                func['can_edit_delete'] = False
        count += 1
        gpucount += func['func']['object']["spec"]["resources"]["GPU"]["Count"]
        vram += func['func']['object']["spec"]["resources"]["GPU"]["Count"] * func['func']['object']["spec"]["resources"]["GPU"]["vRam"]
        cpu += func['func']['object']["spec"]["resources"]["CPU"]
        memory += func['func']['object']["spec"]["resources"]["Mem"]

    summary = {}
    summary["model_count"] = count
    summary["gpucount"] = gpucount
    summary["vram"] = vram
    summary["cpu"] = cpu
    summary["memory"] = memory
    

    can_manage_models = any(
        str(role.get('role', '')).lower() == 'admin'
        and str(role.get('objType', '')).lower() in ('tenant', 'namespace')
        for role in roles
    )
    return render_template(
        "func_list.html",
        funcs=funcs,
        summary=summary,
        can_manage_models=can_manage_models,
        is_inferx_admin=is_inferx_admin,
    )


@prefix_bp.route("/listsnapshot")
@require_login_unless_gateway_aligned_anonymous_enabled
def ListSnapshot():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    snapshots = None
    if tenant is None:
        snapshots = listsnapshots("", "")
    elif namespace is None:
        snapshots = listsnapshots(tenant, "")
    else:
        snapshots = listsnapshots(tenant, namespace)

    snapshots = filter_public_tenant_resource_items(
        snapshots,
        include_public=can_access_public_tenant_in_dashboard(),
    )

    return render_template("snapshot_list.html", snapshots=snapshots)


@prefix_bp.route("/func", methods=("GET", "POST"))
@require_login_unless_gateway_aligned_anonymous_enabled
def GetFunc():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    func_resp, func = getfunc_response(tenant, namespace, name)
    if is_upstream_resource_unavailable(func_resp, func):
        return render_resource_unavailable_page(
            resource_kind="Model",
            resource_name=name,
            tenant=tenant,
            namespace=namespace,
            message="This model is no longer available in the dashboard.",
            suggestion="It may have been deleted, moved, or you may no longer have access to it.",
            primary_href=dashboard_href("prefix.ListFunc", tenant=tenant, namespace=namespace),
            primary_label="Back to Models",
            secondary_href=dashboard_href("prefix.ListFunc"),
            secondary_label="All Models",
            detail=extract_upstream_error_message(func_resp, func),
            status=404,
        )
    if not func_resp.ok:
        return render_resource_load_error_page(
            resource_kind="Model",
            resource_name=name,
            tenant=tenant,
            namespace=namespace,
            primary_href=dashboard_href("prefix.ListFunc", tenant=tenant, namespace=namespace),
            primary_label="Back to Models",
            secondary_href=dashboard_href("prefix.ListFunc"),
            secondary_label="All Models",
            upstream_status=func_resp.status_code,
            detail=extract_upstream_error_message(func_resp, func),
        )

    deny_resp = deny_public_tenant_request(resource_item_tenant_name(func))
    if deny_resp is not None:
        return deny_resp

    try:
        sample = func["func"]["object"]["spec"]["sample_query"]
    except (KeyError, TypeError):
        return render_resource_unavailable_page(
            resource_kind="Model",
            resource_name=name,
            tenant=tenant,
            namespace=namespace,
            message="This model is no longer available in the dashboard.",
            suggestion="It may have been deleted, moved, or the backend no longer has full details for it.",
            primary_href=dashboard_href("prefix.ListFunc", tenant=tenant, namespace=namespace),
            primary_label="Back to Models",
            secondary_href=dashboard_href("prefix.ListFunc"),
            secondary_label="All Models",
            detail=extract_upstream_error_message(func_resp, func),
            status=404,
        )
    map = sample["body"]
    apiType = sample["apiType"]
    isAdmin = func["isAdmin"]

    version = func["func"]["object"]["spec"]["version"]
    funcpolicy = func["policy"]
    fails = GetFailLogs(tenant, namespace, name, version)
    snapshotaudit = GetSnapshotAudit(tenant, namespace, name, version)

    local_tz = pytz.timezone("America/Los_Angeles")  # or use tzlocal.get_localzone()
    for a in snapshotaudit:
        dt = datetime.fromisoformat(a["updatetime"].replace("Z", "+00:00"))
        a["updatetime"] = dt.astimezone(local_tz).strftime("%Y-%m-%d %H:%M:%S")

    for a in fails:
        dt = datetime.fromisoformat(a["createtime"].replace("Z", "+00:00"))
        a["createtime"] = dt.astimezone(local_tz).strftime("%Y-%m-%d %H:%M:%S")

    # Show the same filtered partial spec used by the new create/edit flow.
    full_funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)
    try:
        func_edit_data = project_func_for_edit(func)
        funcspec = json.dumps(func_edit_data["spec"], indent=4)
    except Exception:
        # Fallback for unexpected legacy shapes so the page still renders.
        func_edit_data = None
        funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)

    onboarding_apikey = str(session.get("onboarding_inference_apikey", "") or "").strip()
    sample_rest_call_for_ui = build_sample_rest_call_for_ui(
        tenant=tenant,
        namespace=namespace,
        funcname=name,
        sample_query=sample,
        apikey=onboarding_apikey,
    )
    if sample_rest_call_for_ui != "":
        func["sampleRestCall"] = sample_rest_call_for_ui

    is_inferx_admin = is_inferx_admin_user()

    return render_template(
        "func.html",
        tenant=tenant,
        namespace=namespace,
        name=name,
        func=func,
        fails=fails,
        snapshotaudit=snapshotaudit,
        funcspec=funcspec,
        full_funcspec=full_funcspec,
        func_edit_data=func_edit_data,
        apiType=apiType,
        map=map,
        isAdmin=isAdmin,
        is_inferx_admin=is_inferx_admin,
        funcpolicy=funcpolicy,
        path=sample["path"]
    )

@prefix_bp.route("/listnode")
@require_login_unless_gateway_aligned_anonymous_enabled
@require_admin_unless_gateway_aligned_anonymous_enabled
def ListNode():
    nodes = listnodes()

    for node in nodes:
        gpus_obj = node['object']['resources']['GPUs']

        #Preformmated string for display
        gpus_pretty = json.dumps(gpus_obj, indent=4).replace("\n", "<br>").replace("    ", "&emsp;")
        node['object']['resources']['GPUs_str'] = gpus_pretty  #store separately

    return render_template("node_list.html", nodes=nodes)

@prefix_bp.route("/node")
@require_login_unless_gateway_aligned_anonymous_enabled
@require_admin_unless_gateway_aligned_anonymous_enabled
def GetNode():
    name = request.args.get("name")
    node = getnode(name)

    nodestr = json.dumps(node["object"], indent=4)
    nodestr = nodestr.replace("\n", "<br>")
    nodestr = nodestr.replace("    ", "&emsp;")

    return render_template("node.html", name=name, node=nodestr)


@prefix_bp.route("/listpod")
@require_login_unless_gateway_aligned_anonymous_enabled
def ListPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")

    pods = None
    if tenant is None:
        pods = listpods("", "", "")
    elif namespace is None:
        pods = listpods(tenant, "", "")
    else:
        pods = listpods(tenant, namespace, "")

    is_inferx_admin = can_view_public_tenant()
    pods = filter_public_tenant_resource_items(
        pods,
        include_public=can_access_public_tenant_in_dashboard(),
    )

    return render_template(
        "pod_list.html",
        pods=pods,
        is_inferx_admin=is_inferx_admin,
    )


@prefix_bp.route("/pod")
@require_login_unless_gateway_aligned_anonymous_enabled
def GetPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    podname = request.args.get("name")

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    pod_resp, pod = getpod_response(tenant, namespace, podname)
    if is_upstream_resource_unavailable(pod_resp, pod):
        return render_resource_unavailable_page(
            resource_kind="Pod",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            message="This pod is no longer available in the dashboard.",
            suggestion="It may have completed, been garbage collected, or belonged to a snapshot that is no longer active.",
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Pods",
            secondary_href=dashboard_href("prefix.ListSnapshot", tenant=tenant, namespace=namespace),
            secondary_label="View Snapshots",
            detail=extract_upstream_error_message(pod_resp, pod),
            status=404,
        )
    if not pod_resp.ok:
        return render_resource_load_error_page(
            resource_kind="Pod",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Pods",
            secondary_href=dashboard_href("prefix.ListSnapshot", tenant=tenant, namespace=namespace),
            secondary_label="View Snapshots",
            upstream_status=pod_resp.status_code,
            detail=extract_upstream_error_message(pod_resp, pod),
        )

    deny_resp = deny_public_tenant_request(resource_item_tenant_name(pod))
    if deny_resp is not None:
        return deny_resp

    try:
        funcname = pod["object"]["spec"]["funcname"]
        version = pod["object"]["spec"]["fprevision"]
        id = pod["object"]["spec"]["id"]
    except (KeyError, TypeError):
        return render_resource_unavailable_page(
            resource_kind="Pod",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            message="This pod is no longer available in the dashboard.",
            suggestion="It may have completed, been garbage collected, or belonged to a snapshot that is no longer active.",
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Pods",
            secondary_href=dashboard_href("prefix.ListSnapshot", tenant=tenant, namespace=namespace),
            secondary_label="View Snapshots",
            detail=extract_upstream_error_message(pod_resp, pod),
            status=404,
        )
    log = readpodlog(tenant, namespace, funcname, version, id)

    audits = getpodaudit(tenant, namespace, funcname, version, id)
    local_tz = pytz.timezone("America/Los_Angeles")  # or use tzlocal.get_localzone()
    for a in audits:
        dt = datetime.fromisoformat(a["updatetime"].replace("Z", "+00:00"))
        a["updatetime"] = dt.astimezone(local_tz).strftime("%Y-%m-%d %H:%M:%S")
        
    funcs = listfuncs(tenant, namespace)
    return render_template(
        "pod.html",
        tenant=tenant,
        namespace=namespace,
        podname=podname,
        funcname=funcname,
        audits=audits,
        log=log,
        funcs = funcs,
    )


@prefix_bp.route("/failpod")
@require_login_unless_gateway_aligned_anonymous_enabled
def GetFailPod():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")
    version = request.args.get("version")
    id = request.args.get("id")

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    log = GetFailLog(tenant, namespace, name, version, id)

    audits = getpodaudit(tenant, namespace, name, version, id)
    return render_template(
        "pod.html",
        tenant=tenant,
        namespace=namespace,
        podname=name,
        audits=audits,
        log=log,
    )

#activate the BluePrint
app.register_blueprint(prefix_bp)

def run_http():
    app.run(host='0.0.0.0', port=1250, debug=True)


if __name__ == "__main__":
    if tls:
        # http_thread = Thread(target=run_http)
        # http_thread.start()
        app.run(host="0.0.0.0", port=1290, debug=True, ssl_context=('/etc/letsencrypt/live/inferx.net/fullchain.pem', '/etc/letsencrypt/live/inferx.net/privkey.pem'))
        # app.run(host="0.0.0.0", port=1239, ssl_context=('/etc/letsencrypt/live/quarksoft.io/fullchain.pem', '/etc/letsencrypt/live/quarksoft.io/privkey.pem'))
    else:
        app.run(host='0.0.0.0', port=1250, debug=True)
