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
import re
import time
from datetime import datetime, timezone
from urllib.parse import urlencode
import pytz

import requests
import markdown
import functools
try:
    import psycopg2
    from psycopg2.extras import RealDictCursor
except ImportError:
    psycopg2 = None
    RealDictCursor = None

from flask import (
    Blueprint,
    Flask,
    g,
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
    script_root = ""
    try:
        script_root = str(getattr(request, "script_root", "") or "").rstrip("/")
    except Exception:
        script_root = ""
    hosturl = f"{script_root}/" if script_root != "" else "/"
    dashboard_is_inferx_admin = False
    try:
        dashboard_is_inferx_admin = is_inferx_admin_user()
    except Exception:
        dashboard_is_inferx_admin = False

    return {
        "hosturl": hosturl,
        "slack_invite_url": SLACK_INVITE_URL,
        "dashboard_is_inferx_admin": dashboard_is_inferx_admin,
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

DEFAULT_VLLM_IMAGE_WHITELIST = [
    "vllm/vllm-openai:v0.12.0",
    "vllm/vllm-openai:v0.13.0",
    "vllm/vllm-openai:v0.14.0",
    "vllm/vllm-openai:v0.15.0",
    "vllm/vllm-openai:v0.16.0",
    "vllm/vllm-openai:v0.17.1",
    "vllm/vllm-omni:v0.14.0",
]
VLLM_IMAGE_WHITELIST_ENV = "INFERX_VLLM_IMAGE_WHITELIST"


def load_vllm_image_whitelist():
    raw = str(os.getenv(VLLM_IMAGE_WHITELIST_ENV, "") or "").strip()
    if raw == "":
        return list(DEFAULT_VLLM_IMAGE_WHITELIST)

    def _invalid(reason: str):
        raise ValueError(f"{VLLM_IMAGE_WHITELIST_ENV} {reason}")

    parsed_items = None
    try:
        parsed_json = json.loads(raw)
        if parsed_json is not None and not isinstance(parsed_json, list):
            _invalid("must be a JSON array or a newline-delimited string")
        if isinstance(parsed_json, list):
            parsed_items = parsed_json
    except json.JSONDecodeError:
        parsed_items = [line.strip() for line in raw.splitlines() if line.strip() != ""]

    try:
        if not isinstance(parsed_items, list) or len(parsed_items) == 0:
            _invalid("must contain at least one image")

        normalized = []
        seen = set()
        for index, item in enumerate(parsed_items):
            if not isinstance(item, str):
                _invalid(f"entry {index} must be a string")
            value = item.strip()
            if value == "":
                _invalid(f"entry {index} must be a non-empty string")
            if value in seen:
                continue
            seen.add(value)
            normalized.append(value)

        if len(normalized) == 0:
            _invalid("must contain at least one non-empty image")

        return normalized
    except Exception as e:
        app.logger.warning(
            "failed to parse %s (%s), using default whitelist: %s",
            VLLM_IMAGE_WHITELIST_ENV,
            e,
            DEFAULT_VLLM_IMAGE_WHITELIST,
        )
        return list(DEFAULT_VLLM_IMAGE_WHITELIST)


VLLM_IMAGE_WHITELIST = load_vllm_image_whitelist()

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
CATALOG_RESERVED_ENV_KEYS = {
    "HF_HUB_OFFLINE",
    "TRANSFORMERS_OFFLINE",
}
CATALOG_PLATFORM_ENV_KEYS = RESERVED_ENV_KEYS | CATALOG_RESERVED_ENV_KEYS
CATALOG_ALLOWED_API_TYPES = {"text2text", "image2text", "audio2text", "text2img", "text2audio"}
CATALOG_ALLOWED_STANDBY_TYPES = {"File", "Mem", "Blob"}
CATALOG_ALLOWED_MOUNT_HOSTPATH_PREFIXES = ("/opt/inferx/",)
CATALOG_HF_CACHE_MOUNTPATH = "/root/.cache/huggingface"
CATALOG_EDITABLE_ENTRY_FIELDS = {
    "display_name",
    "provider",
    "modality",
    "brief_intro",
    "detailed_intro",
    "parameter_count_b",
    "is_moe",
    "tags",
    "recommended_use_cases",
    "default_func_spec",
}
CATALOG_CREATE_REQUIRED_FIELDS = {
    "display_name",
    "provider",
    "modality",
    "brief_intro",
    "source_model_id",
    "default_func_spec",
}
SECRDB_ADDR = os.getenv("SECRDB_ADDR", "postgresql://secret:123456@localhost:5431/secretdb").strip()
CATALOG_FULL_SPEC_REQUIRED_KEYS = {
    "image",
    "commands",
    "resources",
    "envs",
    "endpoint",
    "sample_query",
    "standby",
}
CATALOG_ENTRY_SELECT_COLUMNS = """
    SELECT
        id,
        slug,
        display_name,
        provider,
        modality,
        brief_intro,
        detailed_intro,
        source_kind,
        source_model_id,
        parameter_count_b,
        is_moe,
        tags,
        recommended_use_cases,
        default_func_spec,
        is_active,
        catalog_version,
        createtime,
        updatetime
    FROM CatalogModel
"""

FIXED_ENDPOINT = {"port": 8000, "schema": "Http", "probe": "/health"}
FIXED_STANDBY = {"gpu": "File", "pageable": "File", "pinned": "File"}
EMBEDDED_POLICY_DEFAULTS = {
    "min_replica": 0,
    "max_replica": 1,
    "standby_per_node": 1,
    "parallel": 50,
    "queue_len": 100,
    "queue_timeout": 30.0,
    "scaleout_policy": {"WaitQueueRatio": {"wait_ratio": 0.1}},
    "scalein_timeout": 1.0,
    "runtime_config": {"graph_sync": False},
}
CATALOG_RECOMMENDED_USE_CASE_GROUPS = [
    {
        "label": "General",
        "items": [
            "Chatbot",
            "General assistant",
            "Reasoning assistant",
            "Personal assistant",
            "Agent / tool use",
            "Workflow automation",
            "Research assistant",
            "Knowledge assistant",
        ],
    },
    {
        "label": "Search And Retrieval",
        "items": [
            "RAG",
            "Enterprise search",
            "Knowledge base Q&A",
            "FAQ assistant",
            "Semantic search",
            "Document retrieval",
            "Visual document retrieval",
            "Product search",
        ],
    },
    {
        "label": "Writing And Content",
        "items": [
            "Writing assistant",
            "Content generation",
            "Copywriting",
            "Email drafting",
            "Report generation",
            "Summarization",
            "Paraphrasing",
            "Grammar and style correction",
        ],
    },
    {
        "label": "Coding And Technical",
        "items": [
            "Coding assistant",
            "Code completion",
            "Code generation",
            "Code explanation",
            "Code review",
            "Test generation",
            "SQL generation",
            "Data analysis assistant",
        ],
    },
    {
        "label": "Classification And Extraction",
        "items": [
            "Classification",
            "Tagging",
            "Sentiment analysis",
            "Moderation",
            "Entity extraction",
            "Information extraction",
            "Form extraction",
            "Document extraction",
        ],
    },
    {
        "label": "Documents",
        "items": [
            "OCR",
            "Document Q&A",
            "Contract review",
            "Invoice processing",
            "Resume parsing",
            "Table extraction",
            "Document understanding",
            "Knowledge ingestion",
        ],
    },
    {
        "label": "Customer Support And Business",
        "items": [
            "Customer support",
            "Ticket triage",
            "Helpdesk assistant",
            "Sales assistant",
            "CRM note generation",
            "Call center analytics",
            "Meeting notes",
            "Internal operations assistant",
        ],
    },
    {
        "label": "Language",
        "items": [
            "Translation",
            "Localization",
            "Speech translation",
            "Multilingual chat",
            "Multilingual search",
        ],
    },
    {
        "label": "Audio And Voice",
        "items": [
            "Transcription",
            "Speech-to-text",
            "Text-to-speech",
            "Voice assistant",
            "Audio summarization",
            "Voice cloning",
            "Dubbing",
            "Audio classification",
        ],
    },
    {
        "label": "Vision",
        "items": [
            "Image captioning",
            "Visual Q&A",
            "Image understanding",
            "Product tagging",
            "Object detection",
            "Image classification",
            "Image segmentation",
            "Image search",
        ],
    },
    {
        "label": "Image And Video Generation",
        "items": [
            "Image generation",
            "Image editing",
            "Marketing creative generation",
            "Design ideation",
            "Video understanding",
            "Video summarization",
            "Video generation",
            "Video editing assistance",
        ],
    },
    {
        "label": "Structured And Domain",
        "items": [
            "Forecasting",
            "Anomaly detection",
            "Recommendation",
            "Tabular analysis",
            "Robotics",
            "Graph analysis",
            "Scientific assistant",
            "Healthcare documentation",
        ],
    },
]
CATALOG_TAG_GROUPS = [
    {
        "label": "Capability And Behavior",
        "items": [
            "reasoning",
            "tool-calling",
            "instruction-tuned",
            "chat",
            "multilingual",
            "long-context",
            "structured-output",
            "function-calling",
        ],
    },
    {
        "label": "Domain",
        "items": [
            "coding",
            "legal",
            "finance",
            "healthcare",
            "education",
            "science",
            "enterprise",
            "customer-support",
        ],
    },
    {
        "label": "Modality And I/O",
        "items": [
            "vision",
            "speech",
            "audio",
            "multimodal",
            "image-generation",
            "video-generation",
            "ocr",
            "embeddings",
        ],
    },
    {
        "label": "Deployment And Performance",
        "items": [
            "low-latency",
            "high-throughput",
            "edge-friendly",
            "quantized",
            "fp8",
            "moe",
            "serverless-friendly",
            "batch-inference",
        ],
    },
    {
        "label": "Ecosystem And Compatibility",
        "items": [
            "OpenClaw friendly",
            "OpenAI-compatible",
            "vLLM",
            "Transformers",
            "agentic",
            "RAG-ready",
            "enterprise-ready",
            "API-ready",
        ],
    },
    {
        "label": "Safety And Quality",
        "items": [
            "safety-tuned",
            "moderation",
            "benchmarked",
            "production-ready",
            "instruction-following",
            "stable",
            "distilled",
            "fine-tuned",
        ],
    },
]
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
    username = str(session.get('username', '') or '').strip().lower()
    if username in ('inferx_admin', 'inferxadmin'):
        return True

    if session.get('access_token', '') == '':
        return False

    try:
        roles = listroles()
    except Exception:
        return False

    return has_inferx_admin_role(roles)


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


def build_catalog_slug_conflict_message(entry_payload=None):
    payload = entry_payload if isinstance(entry_payload, dict) else {}
    slug = str(payload.get("slug", "") or "").strip()
    provider = str(payload.get("provider", "") or "").strip()
    display_name = str(payload.get("display_name", "") or "").strip()

    if slug == "":
        return "A catalog model with this slug already exists."

    generation_detail = "Slug is generated as `<provider-slug>--<display-name-slug>` from Provider and Display Name."
    if provider != "" and display_name != "":
        try:
            provider_slug = normalize_catalog_slug_component(provider, "provider")
            display_name_slug = normalize_catalog_slug_component(display_name, "display_name")
            generation_detail = (
                f"Slug is generated as `{provider_slug}--{display_name_slug}` "
                f"from provider `{provider}` and display_name `{display_name}`."
            )
        except ValueError:
            generation_detail = (
                f"Slug is generated from provider `{provider}` and display_name `{display_name}` "
                "after slug normalization."
            )

    return (
        f"A catalog model with slug `{slug}` already exists. "
        f"{generation_detail} Change Provider or Display Name to generate a different slug."
    )


def catalog_admin_error_response(message, *, entry_payload=None):
    normalized = str(message or "")
    if "duplicate key value violates unique constraint" in normalized:
        if "source_model_id" in normalized:
            return json_error("The database still enforces a unique source_model_id constraint. Run the catalog migration before creating multiple variants for the same upstream model.", 409)
        if "slug" in normalized:
            return json_error(build_catalog_slug_conflict_message(entry_payload), 409)
        return json_error("A catalog model with the same unique key already exists.", 409)
    return json_error(normalized, 500)


def render_catalog_admin_page(
    *,
    initial_edit_entry_id=None,
    initial_prefill_entry=None,
    initial_prefill_error="",
):
    try:
        entries = list_catalog_entries(active_only=False)
    except Exception as e:
        return json_error(f"failed to load admin catalog entries: {e}", 500)

    selected_entry = None
    if initial_edit_entry_id is not None:
        try:
            selected_entry = query_catalog_entry_by_id(int(initial_edit_entry_id), active_only=False)
        except LookupError as e:
            return json_error(str(e), 404)
        except Exception as e:
            return json_error(f"failed to load catalog model {initial_edit_entry_id}: {e}", 500)

    catalog_back_href = dashboard_href("prefix.CatalogList")
    catalog_back_label = "Back to Catalog"
    page_title = "New Catalog Model"
    page_subtitle = "Admin-only editor for catalog models. New models start unpublished until you publish them from the catalog detail page."
    if selected_entry is not None:
        page_title = "Edit"
        display_name = str(selected_entry.get("display_name", "")).strip()
        if display_name != "":
            page_subtitle = f"Editing {display_name}."
        if str(selected_entry.get("slug", "")).strip() != "":
            catalog_back_href = dashboard_href("prefix.CatalogDetail", slug=selected_entry["slug"])
            catalog_back_label = "Back to Catalog Model"

    return render_template(
        "admin_catalog.html",
        catalog_entries=entries,
        initial_edit_entry_id=initial_edit_entry_id,
        initial_edit_entry=selected_entry,
        initial_prefill_entry=initial_prefill_entry,
        initial_prefill_error=str(initial_prefill_error or ""),
        catalog_tag_groups=CATALOG_TAG_GROUPS,
        catalog_use_case_groups=CATALOG_RECOMMENDED_USE_CASE_GROUPS,
        catalog_back_href=catalog_back_href,
        catalog_back_label=catalog_back_label,
        catalog_admin_page_title=page_title,
        catalog_admin_page_subtitle=page_subtitle,
    )


@prefix_bp.route('/admin/catalog')
@require_login
@require_admin
def AdminCatalog():
    edit_raw = str(request.args.get("edit", "") or "").strip()
    if edit_raw != "":
        try:
            catalog_id = parse_catalog_id(edit_raw)
        except ValueError as e:
            return json_error(str(e), 400)
        return redirect(url_for('prefix.CatalogAdminEdit', catalog_id=catalog_id))
    return redirect(url_for('prefix.CatalogAdminNew'))


@prefix_bp.route('/catalog/new')
@require_login
@require_admin
def CatalogAdminNew():
    from_func_raw = str(request.args.get("from_func", "") or "").strip()
    if from_func_raw == "":
        return render_catalog_admin_page(
            initial_edit_entry_id=None,
            initial_prefill_entry=None,
            initial_prefill_error="",
        )

    prefill_error = ""
    prefill_entry = None
    try:
        try:
            tenant, namespace, name = parse_edit_key(from_func_raw)
        except ValueError:
            raise ValueError("`from_func` must be `<tenant>/<namespace>/<name>`")
        full_func = getfunc(tenant, namespace, name)
        func_obj = (full_func.get("func") or {}) if isinstance(full_func, dict) else {}
        func_name = str(func_obj.get("name", "")).strip() or name
        full_spec = (((func_obj.get("object") or {}).get("spec")) or {})
        if not isinstance(full_spec, dict):
            raise ValueError("live function spec is missing")
        hf_model = ""
        try:
            hf_model = resolve_effective_model_target_from_spec(
                full_spec,
                commands_label="live function commands",
                sample_query_label="live function sample_query.body.model",
            ).strip()
        except Exception as e:
            prefill_error = (
                "Save2Catalog could not resolve model-identifying fields automatically: "
                f"{e}. The live runtime spec was still copied; fill in Display Name, Provider, "
                "and Source Model ID manually."
            )
        if hf_model == "" and prefill_error == "":
            prefill_error = (
                "Save2Catalog could not resolve model-identifying fields automatically. "
                "The live runtime spec was still copied; fill in Display Name, Provider, "
                "and Source Model ID manually."
            )
        prefill_entry = build_catalog_prefill_from_func(hf_model, full_spec, func_name)
    except Exception as e:
        prefill_error = f"Save2Catalog prefill failed: {e}"

    return render_catalog_admin_page(
        initial_edit_entry_id=None,
        initial_prefill_entry=prefill_entry,
        initial_prefill_error=prefill_error,
    )


@prefix_bp.route('/catalog/<int:catalog_id>/edit')
@require_login
@require_admin
def CatalogAdminEdit(catalog_id: int):
    return render_catalog_admin_page(initial_edit_entry_id=catalog_id)


@prefix_bp.route('/admin/catalog/entries', methods=['GET'])
@require_login
@require_admin
def AdminCatalogEntries():
    try:
        entries = list_catalog_entries(active_only=False)
    except Exception as e:
        return json_error(f"failed to load catalog entries: {e}", 500)
    return jsonify({"entries": entries})


@prefix_bp.route('/admin/catalog/entries', methods=['PUT'])
@require_login
@require_admin
def AdminCatalogCreate():
    req = request.get_json(silent=True)
    payload = None
    try:
        payload = normalize_catalog_admin_payload(req or {})
        entry = insert_catalog_entry(payload)
    except ValueError as e:
        return json_error(str(e), 400)
    except RuntimeError as e:
        return catalog_admin_error_response(str(e), entry_payload=payload)
    return jsonify({"entry": entry}), 201


@prefix_bp.route('/admin/catalog/entries/<int:catalog_id>', methods=['POST'])
@require_login
@require_admin
def AdminCatalogUpdate(catalog_id: int):
    req = request.get_json(silent=True)
    payload = None
    try:
        existing_entry = query_catalog_entry_by_id(catalog_id, active_only=False)
        payload = normalize_catalog_admin_payload(req or {}, existing_entry=existing_entry)
        entry = update_catalog_entry(catalog_id, payload)
    except LookupError as e:
        return json_error(str(e), 404)
    except ValueError as e:
        return json_error(str(e), 400)
    except RuntimeError as e:
        return catalog_admin_error_response(str(e), entry_payload=payload)
    return jsonify({"entry": entry})


@prefix_bp.route('/admin/catalog/entries/<int:catalog_id>', methods=['DELETE'])
@require_login
@require_admin
def AdminCatalogDelete(catalog_id: int):
    try:
        entry = delete_catalog_entry(catalog_id)
    except LookupError as e:
        return json_error(str(e), 404)
    except RuntimeError as e:
        return catalog_admin_error_response(str(e))
    return jsonify({"entry": entry, "deleted": True})


@prefix_bp.route('/admin/catalog/entries/<int:catalog_id>/publish', methods=['POST'])
@require_login
@require_admin
def AdminCatalogPublish(catalog_id: int):
    return catalog_admin_set_active_response(catalog_id, is_active=True)


def catalog_admin_set_active_response(catalog_id: int, *, is_active: bool):
    try:
        entry = set_catalog_entry_active(catalog_id, is_active=is_active)
    except LookupError as e:
        return json_error(str(e), 404)
    except RuntimeError as e:
        return catalog_admin_error_response(str(e))
    return jsonify({"entry": entry})


@prefix_bp.route('/admin/catalog/entries/<int:catalog_id>/unpublish', methods=['POST'])
@require_login
@require_admin
def AdminCatalogUnpublish(catalog_id: int):
    return catalog_admin_set_active_response(catalog_id, is_active=False)


@prefix_bp.route('/admin/catalog/entries/<int:catalog_id>/deactivate', methods=['POST'])
@require_login
@require_admin
def AdminCatalogDeactivate(catalog_id: int):
    return catalog_admin_set_active_response(catalog_id, is_active=False)

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
    values = {}
    for key, value in params.items():
        text = str(value or "").strip()
        if text != "":
            values[key] = text

    return url_for(endpoint, **values)


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


def strip_model_command_args(commands):
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


def looks_like_huggingface_model_target_token(value):
    normalized = str(value or "").strip()
    if normalized == "":
        return False
    if normalized.startswith(("/", "./", "../")):
        return False
    if "://" in normalized:
        return False
    return re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*(?:/[A-Za-z0-9][A-Za-z0-9._-]*)*", normalized) is not None


def extract_positional_model_target_from_commands(commands):
    normalized_commands = [str(item).strip() for item in commands] if isinstance(commands, list) else []
    if len(normalized_commands) == 0:
        return ""

    # Some custom images keep the entrypoint in the image and accept the model
    # id as the first positional argument, followed by flags.
    first_token = normalized_commands[0]
    if not looks_like_huggingface_model_target_token(first_token):
        return ""

    if len(normalized_commands) >= 2:
        second_token = normalized_commands[1].strip()
        if second_token != "" and not second_token.startswith("-"):
            return ""

    return first_token


def find_effective_model_targets_in_commands(commands, *, label="commands"):
    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    values = []
    seen = set()

    def _add_target(value):
        normalized = str(value).strip()
        if normalized == "":
            return
        if normalized not in seen:
            values.append(normalized)
            seen.add(normalized)

    positional_target = extract_positional_model_target_from_commands(normalized_commands)
    if positional_target != "":
        _add_target(positional_target)

    if len(normalized_commands) >= 2 and normalized_commands[0] == "vllm" and normalized_commands[1] == "serve":
        if len(normalized_commands) < 3:
            raise ValueError(f"{label} contains `vllm serve` without a model value")
        omni_model = normalized_commands[2].strip()
        if omni_model == "" or omni_model.startswith("-"):
            raise ValueError(f"{label} contains `vllm serve` without a valid model value")
        _add_target(omni_model)

    i = 0
    while i < len(normalized_commands):
        token = normalized_commands[i]
        if token == "--model":
            if i + 1 >= len(normalized_commands):
                raise ValueError(f"{label} contains `--model` without a value")
            model_value = normalized_commands[i + 1].strip()
            if model_value == "":
                raise ValueError(f"{label} contains `--model` without a value")
            _add_target(model_value)
            i += 2
            continue
        if token.startswith("--model="):
            model_value = token.split("=", 1)[1].strip()
            if model_value == "":
                raise ValueError(f"{label} contains `--model=` without a value")
            _add_target(model_value)
        i += 1

    return values


def extract_sample_query_model_target(sample_query):
    if not isinstance(sample_query, dict):
        return ""
    body = sample_query.get("body")
    if not isinstance(body, dict):
        return ""
    model_value = body.get("model")
    if not isinstance(model_value, str):
        return ""
    return model_value.strip()


def resolve_effective_model_target_from_spec(
    spec,
    *,
    commands_label="commands",
    sample_query_label="sample_query.body.model",
):
    spec_obj = spec if isinstance(spec, dict) else {}
    commands = spec_obj.get("commands", [])
    command_targets = find_effective_model_targets_in_commands(
        commands,
        label=commands_label,
    )
    if len(command_targets) == 0:
        entrypoint = spec_obj.get("entrypoint")
        if isinstance(entrypoint, list) and all(isinstance(item, str) for item in entrypoint):
            combined_commands = [str(item) for item in entrypoint]
            if isinstance(commands, list):
                combined_commands.extend(str(item) for item in commands)
            command_targets = find_effective_model_targets_in_commands(
                combined_commands,
                label=f"{commands_label} with entrypoint",
            )
    if len(command_targets) > 1:
        raise ValueError(f"{commands_label} encodes multiple conflicting model targets")
    if len(command_targets) == 1:
        return command_targets[0]
    return extract_sample_query_model_target(spec_obj.get("sample_query"))


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


def split_image_name_and_tag(image):
    normalized = str(image or "").strip()
    if normalized == "":
        return "", ""
    last_slash = normalized.rfind("/")
    last_colon = normalized.rfind(":")
    if last_colon > last_slash:
        return normalized[:last_colon], normalized[last_colon + 1:]
    return normalized, ""


def validate_image_value(
    image,
    *,
    label,
    require_whitelist=True,
    locked_image_name=None,
    require_tag=False,
):
    if not isinstance(image, str) or image.strip() == "":
        raise ValueError(f"{label} must be a non-empty string")
    normalized = image.strip()
    image_name, image_tag = split_image_name_and_tag(normalized)
    if require_tag and image_tag == "":
        raise ValueError(f"{label} must include a tag/version")
    if locked_image_name is not None:
        if image_name != locked_image_name:
            raise ValueError(f"{label} must keep image name `{locked_image_name}` and may only change the tag/version")
        return normalized
    if require_whitelist and normalized not in VLLM_IMAGE_WHITELIST:
        raise ValueError(f"{label} must be one of the whitelisted vllm image tags")
    return normalized


def normalize_partial_embedded_policy_obj(policy_obj, *, label="`spec.policy.Obj`"):
    if not isinstance(policy_obj, dict):
        raise ValueError(f"{label} must be an object")

    allowed_policy_obj_keys = {
        "min_replica",
        "max_replica",
        "standby_per_node",
        "parallel",
        "queue_len",
        "queue_timeout",
        "scaleout_policy",
        "scalein_timeout",
        "runtime_config",
    }
    unknown_policy_keys = set(policy_obj.keys()) - allowed_policy_obj_keys
    if unknown_policy_keys:
        raise ValueError(f"Unknown key in {label}: {sorted(unknown_policy_keys)[0]}")

    normalized_obj = {}
    integer_fields = {
        "min_replica": 0,
        "max_replica": 1,
        "standby_per_node": 0,
        "parallel": 1,
        "queue_len": 1,
    }
    for field_name, minimum in integer_fields.items():
        if field_name not in policy_obj:
            continue
        value = policy_obj[field_name]
        if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
            raise ValueError(f"{label}.{field_name} must be an integer >= {minimum}")
        normalized_obj[field_name] = value

    for field_name in ("queue_timeout", "scalein_timeout"):
        if field_name not in policy_obj:
            continue
        value = policy_obj[field_name]
        if not isinstance(value, (int, float)) or isinstance(value, bool) or float(value) < 0:
            raise ValueError(f"{label}.{field_name} must be a number >= 0")
        normalized_obj[field_name] = float(value)

    if "scaleout_policy" in policy_obj:
        scaleout_policy = policy_obj["scaleout_policy"]
        if not isinstance(scaleout_policy, dict) or set(scaleout_policy.keys()) != {"WaitQueueRatio"}:
            raise ValueError(f"{label}.scaleout_policy must be `{{ \"WaitQueueRatio\": {{ ... }} }}`")
        wait_queue_ratio = scaleout_policy.get("WaitQueueRatio")
        if not isinstance(wait_queue_ratio, dict):
            raise ValueError(f"{label}.scaleout_policy.WaitQueueRatio must be an object")
        wait_ratio = wait_queue_ratio.get("wait_ratio")
        if not isinstance(wait_ratio, (int, float)) or isinstance(wait_ratio, bool) or float(wait_ratio) < 0:
            raise ValueError(f"{label}.scaleout_policy.WaitQueueRatio.wait_ratio must be a number >= 0")
        normalized_obj["scaleout_policy"] = {"WaitQueueRatio": {"wait_ratio": float(wait_ratio)}}

    if "runtime_config" in policy_obj:
        runtime_config = policy_obj["runtime_config"]
        if not isinstance(runtime_config, dict):
            raise ValueError(f"{label}.runtime_config must be an object")
        unknown_runtime_keys = set(runtime_config.keys()) - {"graph_sync"}
        if unknown_runtime_keys:
            raise ValueError(f"Unknown key in {label}.runtime_config: {sorted(unknown_runtime_keys)[0]}")
        normalized_runtime_config = {}
        if "graph_sync" in runtime_config:
            graph_sync = runtime_config["graph_sync"]
            if not isinstance(graph_sync, bool):
                raise ValueError(f"{label}.runtime_config.graph_sync must be a boolean")
            normalized_runtime_config["graph_sync"] = graph_sync
        normalized_obj["runtime_config"] = normalized_runtime_config

    min_replica = normalized_obj.get("min_replica")
    max_replica = normalized_obj.get("max_replica")
    if (
        isinstance(min_replica, int)
        and isinstance(max_replica, int)
        and max_replica < min_replica
    ):
        raise ValueError(f"{label}.max_replica must be >= min_replica")

    return normalized_obj


def build_full_embedded_policy_obj(policy_obj, *, label="`spec.policy.Obj`"):
    merged_policy_obj = clone_json_value(EMBEDDED_POLICY_DEFAULTS)
    normalized_partial = normalize_partial_embedded_policy_obj(policy_obj or {}, label=label)
    for key, value in normalized_partial.items():
        merged_policy_obj[key] = clone_json_value(value)
    return normalize_partial_embedded_policy_obj(merged_policy_obj, label=label)


def validate_partial_spec(
    spec,
    max_node_vram,
    max_node_gpu_count=0,
    editor_mode="basic",
    *,
    require_image_whitelist=True,
    locked_image_name=None,
):
    if not isinstance(spec, dict):
        raise ValueError("`spec` must be an object")

    allowed_spec_keys = {"image", "commands", "resources", "envs", "policy", "sample_query"}
    unknown_spec_keys = set(spec.keys()) - allowed_spec_keys
    if unknown_spec_keys:
        raise ValueError(f"Unknown key in `spec`: {sorted(unknown_spec_keys)[0]}")

    for required_key in ("image", "commands", "resources"):
        if required_key not in spec:
            raise ValueError(f"Missing required `spec.{required_key}`")

    image = validate_image_value(
        spec.get("image"),
        label="`spec.image`",
        require_whitelist=require_image_whitelist,
        locked_image_name=locked_image_name,
        require_tag=locked_image_name is not None,
    )

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

    normalized_sample_query = None
    if "sample_query" in spec:
        sample_query = spec.get("sample_query")
        if not isinstance(sample_query, dict):
            raise ValueError("`spec.sample_query` must be an object")
        normalized_sample_query = clone_json_value(sample_query)

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
        policy_obj = normalize_partial_embedded_policy_obj(obj, label="`spec.policy.Obj`")
        normalized_policy = {"Obj": policy_obj} if policy_obj else None

    normalized_commands = [str(item) for item in commands] if editor_mode == "advanced" else strip_reserved_command_args(commands)

    normalized_spec = {
        "image": image,
        "commands": normalized_commands,
        "resources": {"GPU": {"Count": gpu_count, "vRam": vram}},
        "envs": strip_reserved_envs(normalized_envs),
        "policy": normalized_policy,
    }
    if normalized_sample_query is not None:
        normalized_spec["sample_query"] = normalized_sample_query
    return normalized_spec


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


def build_resolved_sample_query(hf_model: str, sample_query):
    normalized_hf_model = str(hf_model or "").strip()
    if not isinstance(sample_query, dict):
        if normalized_hf_model == "":
            return {}
        return build_sample_query(normalized_hf_model)

    resolved_sample_query = clone_json_value(sample_query)
    if normalized_hf_model == "":
        return resolved_sample_query
    if not isinstance(resolved_sample_query.get("body"), dict):
        resolved_sample_query["body"] = {}
    resolved_sample_query["body"]["model"] = normalized_hf_model
    return resolved_sample_query


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
    full_commands = sync_tensor_parallel_size(full_commands, gpu_count)

    resolved_sample_query = build_resolved_sample_query(
        hf_model,
        partial_spec.get("sample_query"),
    )

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
        full_policy_obj = build_full_embedded_policy_obj(
            policy["Obj"],
            label="`spec.policy.Obj`",
        )
        spec["policy"] = {"Obj": full_policy_obj}

    return spec


def clone_json_value(value):
    return json.loads(json.dumps(value))


def parse_named_command_arg(commands, flag_name):
    if not isinstance(commands, list):
        return ""
    i = 0
    while i < len(commands):
        token = commands[i]
        if token == flag_name:
            if i + 1 < len(commands):
                return str(commands[i + 1]).strip()
            return ""
        if isinstance(token, str) and token.startswith(f"{flag_name}="):
            return token.split("=", 1)[1].strip()
        i += 1
    return ""


def upsert_named_command_arg(commands, flag_name, value):
    normalized_value = str(value).strip()
    if normalized_value == "":
        raise ValueError(f"`{flag_name}` value must be non-empty")

    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    merged_commands = []
    inserted = False
    i = 0
    while i < len(normalized_commands):
        token = normalized_commands[i]
        if token == flag_name:
            if not inserted:
                merged_commands.extend([flag_name, normalized_value])
                inserted = True
            i += 2 if i + 1 < len(normalized_commands) else 1
            continue
        if isinstance(token, str) and token.startswith(f"{flag_name}="):
            if not inserted:
                merged_commands.append(f"{flag_name}={normalized_value}")
                inserted = True
            i += 1
            continue
        merged_commands.append(token)
        i += 1

    if not inserted:
        merged_commands.extend([flag_name, normalized_value])

    return merged_commands


def sync_tensor_parallel_size(commands, gpu_count):
    if not isinstance(gpu_count, int) or isinstance(gpu_count, bool) or gpu_count <= 0:
        raise ValueError("gpu_count must be a positive integer when syncing `--tensor-parallel-size`")
    return upsert_named_command_arg(commands, "--tensor-parallel-size", str(gpu_count))


def extract_locked_command_prefix_tokens(commands):
    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    if len(normalized_commands) >= 3 and normalized_commands[0] == "vllm" and normalized_commands[1] == "serve":
        return normalized_commands[:3]

    prefix_tokens = []
    for token in normalized_commands:
        if token == "--model" or (isinstance(token, str) and token.startswith("--model=")):
            break
        if isinstance(token, str) and token.startswith("-"):
            break
        prefix_tokens.append(token)
    return prefix_tokens


def strip_matching_command_prefix_tokens(commands, prefix_tokens):
    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    normalized_prefix = [str(item) for item in prefix_tokens] if isinstance(prefix_tokens, list) else []
    if normalized_prefix and normalized_commands[:len(normalized_prefix)] == normalized_prefix:
        return normalized_commands[len(normalized_prefix):]
    return normalized_commands


def extract_locked_model_command_tokens(commands):
    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    i = 0
    while i < len(normalized_commands):
        token = normalized_commands[i]
        if token == "--model":
            if i + 1 < len(normalized_commands):
                return [token, normalized_commands[i + 1]]
            return [token]
        if token.startswith("--model="):
            return [token]
        i += 1
    return []


def extract_named_command_arg_tokens(commands, flag_name):
    normalized_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    matched_tokens = []
    i = 0
    while i < len(normalized_commands):
        token = normalized_commands[i]
        if token == flag_name:
            matched_tokens.append(token)
            if i + 1 < len(normalized_commands):
                matched_tokens.append(normalized_commands[i + 1])
                i += 2
                continue
            i += 1
            continue
        if token.startswith(f"{flag_name}="):
            matched_tokens.append(token)
        i += 1
    return matched_tokens


def merge_catalog_runtime_commands(base_commands, submitted_commands, *, image, gpu_count, editor_mode="basic", sync_tensor_parallel=True):
    normalized_base = [str(item) for item in base_commands] if isinstance(base_commands, list) else []
    normalized_submitted = [str(item) for item in submitted_commands] if isinstance(submitted_commands, list) else []
    prefix_tokens = extract_locked_command_prefix_tokens(normalized_base)
    editable_tokens = strip_matching_command_prefix_tokens(normalized_submitted, prefix_tokens)

    if is_vllm_omni_image(image):
        editable_tokens = strip_omni_generated_command_args(editable_tokens)

    if editor_mode == "basic":
        editable_tokens = strip_reserved_command_args(editable_tokens)
    else:
        editable_tokens = strip_model_command_args(editable_tokens)

    merged_commands = []
    merged_commands.extend(prefix_tokens)

    locked_model_tokens = extract_locked_model_command_tokens(normalized_base)
    if locked_model_tokens:
        merged_commands.extend(locked_model_tokens)

    if is_vllm_omni_image(image):
        for token in normalized_base:
            if token in ("--trust-remote-code", "--omni"):
                merged_commands.append(token)

    merged_commands.extend(editable_tokens)
    if not sync_tensor_parallel:
        if editor_mode == "basic":
            existing_tp_tokens = extract_named_command_arg_tokens(normalized_base, "--tensor-parallel-size")
            submitted_tp_tokens = extract_named_command_arg_tokens(normalized_submitted, "--tensor-parallel-size")
            if not extract_named_command_arg_tokens(merged_commands, "--tensor-parallel-size"):
                preserved_tp_tokens = submitted_tp_tokens or existing_tp_tokens
                if preserved_tp_tokens:
                    merged_commands.extend(preserved_tp_tokens)
        return merged_commands
    return sync_tensor_parallel_size(merged_commands, gpu_count)


def parse_catalog_id(value):
    if value is None or value == "":
        return None
    if isinstance(value, bool):
        raise ValueError("`catalog_id` must be a positive integer")

    if isinstance(value, int):
        catalog_id = value
    elif isinstance(value, str):
        raw = value.strip()
        if raw == "":
            return None
        try:
            catalog_id = int(raw)
        except ValueError:
            raise ValueError("`catalog_id` must be a positive integer")
    else:
        raise ValueError("`catalog_id` must be a positive integer")

    if catalog_id <= 0:
        raise ValueError("`catalog_id` must be a positive integer")
    return catalog_id


def normalize_catalog_source(catalog_source):
    if not isinstance(catalog_source, dict):
        return None

    catalog_id = catalog_source.get("catalog_id")
    catalog_version = catalog_source.get("catalog_version")
    try:
        catalog_id = int(catalog_id)
        catalog_version = int(catalog_version)
    except (TypeError, ValueError):
        return None

    if catalog_id <= 0 or catalog_version <= 0:
        return None

    return {
        "catalog_id": catalog_id,
        "catalog_version": catalog_version,
    }


def maybe_get_catalog_entry_metadata(catalog_source):
    normalized_source = normalize_catalog_source(catalog_source)
    if normalized_source is None:
        return {}

    try:
        entry = query_catalog_entry_by_id(normalized_source["catalog_id"], active_only=False)
    except Exception:
        return {}

    metadata = {}
    slug = str(entry.get("slug", "")).strip()
    if slug != "":
        metadata["catalog_slug"] = slug
    display_name = str(entry.get("display_name", "")).strip()
    if display_name != "":
        metadata["catalog_display_name"] = display_name
    return metadata


def ensure_catalog_db_available():
    if psycopg2 is None or RealDictCursor is None:
        raise RuntimeError("catalog support requires `psycopg2-binary` in the dashboard environment")
    if SECRDB_ADDR == "":
        raise RuntimeError("SECRDB_ADDR is empty")


def normalize_catalog_json_field(value):
    if isinstance(value, str):
        return json.loads(value)
    return value


def format_catalog_datetime(value):
    if value is None:
        return ""
    try:
        dt = value
        if getattr(dt, "tzinfo", None) is not None:
            dt = dt.astimezone(timezone.utc)
            return dt.strftime("%Y-%m-%d %H:%M:%S UTC")
        return dt.strftime("%Y-%m-%d %H:%M:%S")
    except Exception:
        return str(value)


def normalize_catalog_datetime_value(value):
    if value is None:
        return None
    try:
        dt = value
        if getattr(dt, "tzinfo", None) is not None:
            return dt.astimezone(timezone.utc).isoformat()
        return dt.isoformat()
    except Exception:
        return str(value)


def normalize_catalog_api_type(api_type):
    normalized = str(api_type or "").strip()
    if normalized.lower() == "openai":
        return "text2text"
    return normalized


def build_catalog_default_spec_from_func(full_spec):
    spec = clone_json_value(full_spec if isinstance(full_spec, dict) else {})
    spec.pop("catalog_source", None)
    spec.pop("version", None)
    spec.pop("status", None)
    return spec


def build_catalog_prefill_from_func(hf_model: str, full_spec, func_name: str):
    catalog_spec = build_catalog_default_spec_from_func(full_spec)
    api_type = normalize_catalog_api_type(((full_spec.get("sample_query") or {}).get("apiType")) if isinstance(full_spec, dict) else "")
    modality_map = {
        "text2text": "text",
        "openai": "text",
        "image2text": "multimodal",
        "audio2text": "multimodal",
        "text2img": "image",
        "text2audio": "audio",
    }
    normalized_hf_model = str(hf_model or "").strip()
    provider = normalized_hf_model.split("/", 1)[0] if "/" in normalized_hf_model else ""
    display_name = normalized_hf_model.split("/")[-1] if normalized_hf_model != "" else ""
    return {
        "source_model_id": normalized_hf_model,
        "source_kind": "huggingface",
        "display_name": display_name,
        "provider": provider,
        "modality": modality_map.get(api_type, "text"),
        "default_func_spec": catalog_spec,
        "parameter_count_b": None,
        "brief_intro": "",
        "detailed_intro": "",
        "tags": [],
        "recommended_use_cases": [],
        "is_moe": False,
    }


def build_catalog_default_summary(default_func_spec):
    spec = default_func_spec if isinstance(default_func_spec, dict) else {}
    resources = spec.get("resources")
    if not isinstance(resources, dict):
        resources = {}
    gpu = resources.get("GPU")
    if not isinstance(gpu, dict):
        gpu = {}
    commands = spec.get("commands")
    if not isinstance(commands, list):
        commands = []
    sample_query = spec.get("sample_query")
    if not isinstance(sample_query, dict):
        sample_query = {}

    gpu_count = gpu.get("Count") if isinstance(gpu.get("Count"), int) and not isinstance(gpu.get("Count"), bool) else None
    gpu_vram = gpu.get("vRam") if isinstance(gpu.get("vRam"), int) and not isinstance(gpu.get("vRam"), bool) else None
    image = spec.get("image") if isinstance(spec.get("image"), str) else ""
    max_model_len = parse_named_command_arg(commands, "--max-model-len")
    max_model_len = max_model_len if max_model_len != "" else None
    api_type = normalize_catalog_api_type(sample_query.get("apiType"))

    summary_parts = []
    if gpu_count is not None:
        summary_parts.append(f"{gpu_count}xGPU")
    if gpu_vram is not None:
        summary_parts.append(f"{gpu_vram} MB")

    return {
        "default_api_type": api_type,
        "default_gpu_count": gpu_count,
        "default_gpu_vram": gpu_vram,
        "default_image": image,
        "default_max_model_len": max_model_len,
        "default_summary": " ".join(summary_parts),
    }


def normalize_catalog_entry_row(row):
    if not isinstance(row, dict):
        raise ValueError("Invalid catalog model row")

    entry = dict(row)
    for key in ("tags", "recommended_use_cases", "default_func_spec"):
        entry[key] = normalize_catalog_json_field(entry.get(key))

    if not isinstance(entry.get("tags"), list):
        entry["tags"] = []
    if not isinstance(entry.get("recommended_use_cases"), list):
        entry["recommended_use_cases"] = []
    if not isinstance(entry.get("default_func_spec"), dict):
        entry["default_func_spec"] = {}

    parameter_count = entry.get("parameter_count_b")
    if parameter_count is not None:
        try:
            entry["parameter_count_b"] = float(parameter_count)
        except Exception:
            entry["parameter_count_b"] = None

    entry["createtime_display"] = format_catalog_datetime(entry.get("createtime"))
    entry["updatetime_display"] = format_catalog_datetime(entry.get("updatetime"))
    entry["createtime"] = normalize_catalog_datetime_value(entry.get("createtime"))
    entry["updatetime"] = normalize_catalog_datetime_value(entry.get("updatetime"))
    entry.update(build_catalog_default_summary(entry.get("default_func_spec")))
    return entry


def query_catalog_entry_by_id(catalog_id: int, *, active_only=False):
    ensure_catalog_db_available()

    query = f"""
        {CATALOG_ENTRY_SELECT_COLUMNS}
        WHERE id = %s
        {"AND is_active = true" if active_only else ""}
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (catalog_id,))
                row = cursor.fetchone()
    except Exception as e:
        raise RuntimeError(f"failed to query catalog model {catalog_id}: {e}")

    if row is None:
        raise LookupError(f"catalog model {catalog_id} not found")

    return normalize_catalog_entry_row(dict(row))


def fetch_active_catalog_entry(catalog_id: int):
    return query_catalog_entry_by_id(catalog_id, active_only=True)


def query_catalog_entry_by_slug(slug: str, *, active_only=False):
    ensure_catalog_db_available()
    normalized_slug = str(slug or "").strip()
    if normalized_slug == "":
        raise ValueError("catalog slug is required")

    query = f"""
        {CATALOG_ENTRY_SELECT_COLUMNS}
        WHERE slug = %s
        {"AND is_active = true" if active_only else ""}
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (normalized_slug,))
                row = cursor.fetchone()
    except Exception as e:
        raise RuntimeError(f"failed to query catalog model `{normalized_slug}`: {e}")

    if row is None:
        raise LookupError(f"catalog model `{normalized_slug}` not found")

    return normalize_catalog_entry_row(dict(row))


def fetch_active_catalog_entry_by_slug(slug: str):
    return query_catalog_entry_by_slug(slug, active_only=True)


def require_catalog_entry_visible_to_current_user(entry):
    if bool((entry or {}).get("is_active")) or is_inferx_admin_user():
        return entry

    entry_slug = str((entry or {}).get("slug", "")).strip()
    entry_id = (entry or {}).get("id")
    if entry_slug != "":
        raise LookupError(f"catalog model `{entry_slug}` not found")
    raise LookupError(f"catalog model {entry_id} not found")


def fetch_catalog_entry_for_current_user(catalog_id: int):
    entry = query_catalog_entry_by_id(catalog_id, active_only=False)
    return require_catalog_entry_visible_to_current_user(entry)


def fetch_catalog_entry_by_slug_for_current_user(slug: str):
    entry = query_catalog_entry_by_slug(slug, active_only=False)
    return require_catalog_entry_visible_to_current_user(entry)


def list_catalog_entries_by_source_model_id(source_model_id: str, *, active_only=False):
    ensure_catalog_db_available()
    normalized_source_model_id = str(source_model_id or "").strip()
    if normalized_source_model_id == "":
        raise ValueError("catalog source_model_id is required")

    query = f"""
        {CATALOG_ENTRY_SELECT_COLUMNS}
        WHERE source_model_id = %s
        {"AND is_active = true" if active_only else ""}
        ORDER BY createtime DESC, id DESC
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (normalized_source_model_id,))
                rows = cursor.fetchall()
    except Exception as e:
        raise RuntimeError(f"failed to query catalog entries by source_model_id `{normalized_source_model_id}`: {e}")

    normalized_entries = []
    for row in rows:
        normalized_entries.append(normalize_catalog_entry_row(dict(row)))
    return normalized_entries


def query_catalog_entry_by_source_model_id(source_model_id: str, *, active_only=False):
    entries = list_catalog_entries_by_source_model_id(source_model_id, active_only=active_only)
    if not entries:
        normalized_source_model_id = str(source_model_id or "").strip()
        raise LookupError(f"catalog model for `{normalized_source_model_id}` not found")
    if len(entries) > 1:
        normalized_source_model_id = str(source_model_id or "").strip()
        raise LookupError(f"multiple catalog models exist for `{normalized_source_model_id}`; query by id or slug instead")
    return entries[0]


def list_catalog_entries(*, active_only=False):
    ensure_catalog_db_available()

    query = f"""
        {CATALOG_ENTRY_SELECT_COLUMNS}
        {"WHERE is_active = true" if active_only else ""}
        ORDER BY display_name ASC, id ASC
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query)
                rows = cursor.fetchall()
    except Exception as e:
        raise RuntimeError(f"failed to query catalog entries: {e}")

    normalized_entries = []
    for row in rows:
        normalized_entries.append(normalize_catalog_entry_row(dict(row)))
    return normalized_entries


def list_active_catalog_entries():
    return list_catalog_entries(active_only=True)


def normalize_catalog_admin_payload(req, *, existing_entry=None):
    if not isinstance(req, dict):
        raise ValueError("catalog payload must be an object")

    allowed_fields = set(CATALOG_EDITABLE_ENTRY_FIELDS) | {"source_kind", "source_model_id"}
    unknown_fields = sorted(set(req.keys()) - allowed_fields)
    if unknown_fields:
        raise ValueError(f"Unknown catalog field `{unknown_fields[0]}`")

    if existing_entry is not None:
        if "source_kind" in req and str(req.get("source_kind") or "").strip() != str(existing_entry.get("source_kind") or "").strip():
            raise ValueError("`source_kind` is immutable after creation")
        if "source_model_id" in req and str(req.get("source_model_id") or "").strip() != str(existing_entry.get("source_model_id") or "").strip():
            raise ValueError("`source_model_id` is immutable after creation")

    source_kind = normalize_catalog_string_field(
        (existing_entry or {}).get("source_kind") if existing_entry is not None else req.get("source_kind", "huggingface"),
        "source_kind",
    )
    source_model_id = normalize_catalog_string_field(
        (existing_entry or {}).get("source_model_id") if existing_entry is not None else req.get("source_model_id"),
        "source_model_id",
    )
    if existing_entry is None:
        missing_create_fields = sorted(
            field
            for field in CATALOG_CREATE_REQUIRED_FIELDS
            if field not in req or req.get(field) in (None, "")
        )
        if missing_create_fields:
            raise ValueError(f"`{missing_create_fields[0]}` is required")

    try:
        max_node_vram, max_node_gpu_count = get_node_capacity_limits()
    except Exception:
        max_node_vram, max_node_gpu_count = 0, 0

    default_func_spec = validate_catalog_template_spec(
        req.get("default_func_spec"),
        source_model_id=source_model_id,
        max_node_vram=max_node_vram,
        max_node_gpu_count=max_node_gpu_count,
    )

    display_name = normalize_catalog_string_field(req.get("display_name"), "display_name")
    provider = normalize_catalog_string_field(req.get("provider"), "provider")

    return {
        "slug": str((existing_entry or {}).get("slug", "")).strip() if existing_entry is not None else build_catalog_slug(provider, display_name),
        "display_name": display_name,
        "provider": provider,
        "modality": normalize_catalog_string_field(req.get("modality"), "modality"),
        "brief_intro": normalize_catalog_string_field(req.get("brief_intro"), "brief_intro"),
        "detailed_intro": normalize_catalog_string_field(req.get("detailed_intro"), "detailed_intro", allow_empty=True),
        "source_kind": source_kind,
        "source_model_id": source_model_id,
        "parameter_count_b": normalize_catalog_parameter_count(req.get("parameter_count_b")),
        "is_moe": bool(req.get("is_moe")),
        "tags": normalize_catalog_string_list(req.get("tags"), "tags"),
        "recommended_use_cases": normalize_catalog_string_list(req.get("recommended_use_cases"), "recommended_use_cases"),
        "default_func_spec": default_func_spec,
    }


def insert_catalog_entry(entry_payload):
    ensure_catalog_db_available()
    query = """
        INSERT INTO CatalogModel (
            slug,
            display_name,
            provider,
            modality,
            brief_intro,
            detailed_intro,
            source_kind,
            source_model_id,
            parameter_count_b,
            is_moe,
            tags,
            recommended_use_cases,
            default_func_spec,
            is_active
        ) VALUES (
            %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s::jsonb, %s::jsonb, %s::jsonb, %s
        )
        RETURNING *
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(
                    query,
                    (
                        entry_payload["slug"],
                        entry_payload["display_name"],
                        entry_payload["provider"],
                        entry_payload["modality"],
                        entry_payload["brief_intro"],
                        entry_payload["detailed_intro"],
                        entry_payload["source_kind"],
                        entry_payload["source_model_id"],
                        entry_payload["parameter_count_b"],
                        entry_payload["is_moe"],
                        json.dumps(entry_payload["tags"]),
                        json.dumps(entry_payload["recommended_use_cases"]),
                        json.dumps(entry_payload["default_func_spec"]),
                        False,
                    ),
                )
                row = cursor.fetchone()
            conn.commit()
    except Exception as e:
        raise RuntimeError(f"failed to create catalog model: {e}")
    return normalize_catalog_entry_row(dict(row))


def update_catalog_entry(catalog_id: int, entry_payload):
    ensure_catalog_db_available()
    query = """
        UPDATE CatalogModel
        SET
            display_name = %s,
            provider = %s,
            modality = %s,
            brief_intro = %s,
            detailed_intro = %s,
            parameter_count_b = %s,
            is_moe = %s,
            tags = %s::jsonb,
            recommended_use_cases = %s::jsonb,
            catalog_version = CASE WHEN default_func_spec IS NOT DISTINCT FROM %s::jsonb THEN catalog_version ELSE catalog_version + 1 END,
            default_func_spec = %s::jsonb
        WHERE id = %s
        RETURNING *
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(
                    query,
                    (
                        entry_payload["display_name"],
                        entry_payload["provider"],
                        entry_payload["modality"],
                        entry_payload["brief_intro"],
                        entry_payload["detailed_intro"],
                        entry_payload["parameter_count_b"],
                        entry_payload["is_moe"],
                        json.dumps(entry_payload["tags"]),
                        json.dumps(entry_payload["recommended_use_cases"]),
                        json.dumps(entry_payload["default_func_spec"]),
                        json.dumps(entry_payload["default_func_spec"]),
                        catalog_id,
                    ),
                )
                row = cursor.fetchone()
            conn.commit()
    except Exception as e:
        raise RuntimeError(f"failed to update catalog model {catalog_id}: {e}")
    if row is None:
        raise LookupError(f"catalog model {catalog_id} not found")
    return normalize_catalog_entry_row(dict(row))


def delete_catalog_entry(catalog_id: int):
    ensure_catalog_db_available()
    query = """
        DELETE FROM CatalogModel
        WHERE id = %s
        RETURNING *
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (catalog_id,))
                row = cursor.fetchone()
            conn.commit()
    except Exception as e:
        raise RuntimeError(f"failed to delete catalog model {catalog_id}: {e}")
    if row is None:
        raise LookupError(f"catalog model {catalog_id} not found")
    return normalize_catalog_entry_row(dict(row))


def deactivate_catalog_entry(catalog_id: int):
    return set_catalog_entry_active(catalog_id, is_active=False)


def set_catalog_entry_active(catalog_id: int, *, is_active: bool):
    ensure_catalog_db_available()
    existing_entry = query_catalog_entry_by_id(catalog_id, active_only=False)
    desired_active = bool(is_active)
    if bool(existing_entry.get("is_active")) == desired_active:
        return existing_entry

    query = """
        UPDATE CatalogModel
        SET is_active = %s
        WHERE id = %s
        RETURNING *
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (desired_active, catalog_id))
                row = cursor.fetchone()
            conn.commit()
    except Exception as e:
        action = "publish" if desired_active else "unpublish"
        raise RuntimeError(f"failed to {action} catalog model {catalog_id}: {e}")
    if row is None:
        raise LookupError(f"catalog model {catalog_id} not found")
    return normalize_catalog_entry_row(dict(row))


def collect_catalog_filter_options(entries):
    providers = sorted({str(entry.get("provider", "")).strip() for entry in entries if str(entry.get("provider", "")).strip() != ""})
    modalities = sorted({str(entry.get("modality", "")).strip() for entry in entries if str(entry.get("modality", "")).strip() != ""})
    api_types = sorted({str(entry.get("default_api_type", "")).strip() for entry in entries if str(entry.get("default_api_type", "")).strip() != ""})
    tags = sorted({
        str(tag).strip()
        for entry in entries
        for tag in (entry.get("tags") if isinstance(entry.get("tags"), list) else [])
        if str(tag).strip() != ""
    })
    use_cases = sorted({
        str(use_case).strip()
        for entry in entries
        for use_case in (entry.get("recommended_use_cases") if isinstance(entry.get("recommended_use_cases"), list) else [])
        if str(use_case).strip() != ""
    })
    return {
        "providers": providers,
        "modalities": modalities,
        "api_types": api_types,
        "tags": tags,
        "use_cases": use_cases,
    }


def tenant_display_label_for_ui(tenant_obj):
    if not isinstance(tenant_obj, dict):
        return ""

    spec = (((tenant_obj.get("object") or {}).get("spec")) or {})
    display_name = str(spec.get("display_name", spec.get("displayName", "")) or "").strip()
    tenant_name = str(tenant_obj.get("name", "") or "").strip()
    if display_name != "":
        return f"{display_name} ({tenant_name})" if tenant_name != "" and display_name != tenant_name else display_name
    return tenant_name


def role_grants_dashboard_resource_access(role):
    if not isinstance(role, dict):
        return False

    obj_type = str(role.get("objType", "") or "").strip().lower()
    role_name = str(role.get("role", "") or "").strip().lower()
    return obj_type in ("tenant", "namespace") and role_name in ("admin", "user")


def get_accessible_tenant_names_from_roles_for_create(roles):
    if not isinstance(roles, list) or has_inferx_admin_role(roles):
        return []

    tenant_names = []
    seen = set()
    for role in roles:
        if not role_grants_dashboard_resource_access(role):
            continue
        tenant_name = str(role.get("tenant", "") or "").strip()
        tenant_key = tenant_name.lower()
        if tenant_name == "" or tenant_key == "system" or tenant_key in seen:
            continue
        seen.add(tenant_key)
        tenant_names.append(tenant_name)
    return tenant_names


def get_explicit_accessible_namespace_names_from_roles_for_create(roles, tenant_name: str):
    normalized_tenant = str(tenant_name or "").strip()
    if normalized_tenant == "" or not isinstance(roles, list) or has_inferx_admin_role(roles):
        return []

    namespace_names = []
    seen = set()
    for role in roles:
        if not isinstance(role, dict):
            continue
        obj_type = str(role.get("objType", "") or "").strip().lower()
        role_name = str(role.get("role", "") or "").strip().lower()
        role_tenant = str(role.get("tenant", "") or "").strip()
        role_namespace = str(role.get("namespace", "") or "").strip()
        if obj_type != "namespace" or role_name not in ("admin", "user") or role_tenant != normalized_tenant:
            continue
        namespace_key = role_namespace.lower()
        if role_namespace == "" or namespace_key in seen:
            continue
        seen.add(namespace_key)
        namespace_names.append(role_namespace)
    return namespace_names


def build_catalog_deploy_target_selector_message(
    *,
    tenant_options,
    selected_tenant,
    namespace_options,
    selected_namespace,
    show_selected_tenant_in_summary=True,
):
    if not tenant_options:
        return "No accessible tenant is available for deployment."
    if str(selected_tenant or "").strip() == "":
        return "Select a tenant to enable Deploy Now."
    if not namespace_options:
        if show_selected_tenant_in_summary:
            return f"No accessible namespace is available under tenant `{selected_tenant}`."
        return "No accessible namespace is available for deployment."
    if str(selected_namespace or "").strip() == "":
        if show_selected_tenant_in_summary:
            return f"Select a namespace under tenant `{selected_tenant}` to enable Deploy Now."
        return "Select a namespace to enable Deploy Now."
    return ""


def build_catalog_deploy_target_unavailable_context(message: str):
    return {
        "tenant_options": [],
        "namespace_options_by_tenant": {},
        "namespace_names_by_tenant": {},
        "selected_tenant": "",
        "selected_namespace": "",
        "show_tenant_in_summary": True,
        "resolved": False,
        "message": str(message or "Deploy Now is unavailable.").strip() or "Deploy Now is unavailable.",
    }


def build_catalog_deploy_target_selector_context(*, roles=None, inferx_admin=None):
    if roles is None:
        try:
            roles = listroles()
        except Exception as e:
            app.logger.warning("catalog deploy target roles lookup failed: %s", e)
            return build_catalog_deploy_target_unavailable_context(
                "Deploy Now is temporarily unavailable because tenant access could not be loaded. Use Customize & Deploy instead."
            )

    if inferx_admin is None:
        inferx_admin = is_inferx_admin_user()

    include_public = can_access_public_tenant_in_dashboard(roles if not inferx_admin else None)
    tenant_rows = filter_public_tenant_tenants(
        listtenants(),
        include_public=include_public,
    )
    namespace_rows = filter_public_tenant_resource_items(
        listnamespaces(),
        include_public=include_public,
    )

    is_inferx_admin = bool(inferx_admin)
    accessible_tenant_names = get_accessible_tenant_names_from_roles_for_create(roles)
    accessible_tenant_name_set = {tenant_name.lower() for tenant_name in accessible_tenant_names}

    tenant_options = []
    tenant_name_set = set()
    for tenant_obj in tenant_rows if isinstance(tenant_rows, list) else []:
        tenant_name = str((tenant_obj or {}).get("name", "") or "").strip()
        tenant_key = tenant_name.lower()
        if tenant_name == "" or tenant_key == "system" or tenant_key in tenant_name_set:
            continue
        if not is_inferx_admin and tenant_key not in accessible_tenant_name_set:
            continue
        tenant_name_set.add(tenant_key)
        tenant_options.append({
            "value": tenant_name,
            "label": tenant_display_label_for_ui(tenant_obj) or tenant_name,
        })

    raw_namespace_options_by_tenant = {}
    namespace_seen_by_tenant = {}
    for namespace_obj in namespace_rows if isinstance(namespace_rows, list) else []:
        row_tenant = str((namespace_obj or {}).get("tenant", "") or "").strip()
        row_namespace = str((namespace_obj or {}).get("name", "") or "").strip()
        row_tenant_key = row_tenant.lower()
        row_namespace_key = row_namespace.lower()
        if row_tenant == "" or row_tenant_key == "system" or row_namespace == "":
            continue
        raw_namespace_options_by_tenant.setdefault(row_tenant, [])
        namespace_seen_by_tenant.setdefault(row_tenant, set())
        if row_namespace_key in namespace_seen_by_tenant[row_tenant]:
            continue
        namespace_seen_by_tenant[row_tenant].add(row_namespace_key)
        raw_namespace_options_by_tenant[row_tenant].append({
            "value": row_namespace,
            "label": row_namespace,
        })

    namespace_options_by_tenant = {}
    namespace_names_by_tenant = {}
    for tenant_option in tenant_options:
        tenant_name = tenant_option["value"]
        namespace_options = list(raw_namespace_options_by_tenant.get(tenant_name, []))
        explicit_namespace_names = get_explicit_accessible_namespace_names_from_roles_for_create(roles, tenant_name)
        if not is_inferx_admin and explicit_namespace_names:
            explicit_namespace_name_set = {namespace_name.lower() for namespace_name in explicit_namespace_names}
            namespace_options = [
                namespace_option
                for namespace_option in namespace_options
                if str(namespace_option.get("value", "") or "").strip().lower() in explicit_namespace_name_set
            ]
        namespace_options_by_tenant[tenant_name] = namespace_options
        namespace_names_by_tenant[tenant_name] = [
            str(namespace_option.get("value", "") or "").strip()
            for namespace_option in namespace_options
            if str(namespace_option.get("value", "") or "").strip() != ""
        ]

    active_tenant_name = str(session.get("active_tenant_name", session.get("tenant_name", "")) or "").strip()
    selected_tenant = ""
    if active_tenant_name != "":
        for tenant_option in tenant_options:
            tenant_value = str(tenant_option.get("value", "") or "").strip()
            if tenant_value != "" and tenant_value.lower() == active_tenant_name.lower():
                selected_tenant = tenant_value
                break
    if selected_tenant == "" and len(tenant_options) == 1:
        selected_tenant = str(tenant_options[0].get("value", "") or "").strip()

    namespace_options = namespace_options_by_tenant.get(selected_tenant, []) if selected_tenant != "" else []
    selected_namespace = ""
    if len(namespace_options) == 1:
        selected_namespace = str(namespace_options[0].get("value", "") or "").strip()

    show_tenant_in_summary = is_inferx_admin or len(tenant_options) != 1
    resolved = selected_tenant != "" and selected_namespace != ""
    return {
        "tenant_options": tenant_options,
        "namespace_options_by_tenant": namespace_options_by_tenant,
        "namespace_names_by_tenant": namespace_names_by_tenant,
        "selected_tenant": selected_tenant,
        "selected_namespace": selected_namespace,
        "show_tenant_in_summary": show_tenant_in_summary,
        "resolved": resolved,
        "message": build_catalog_deploy_target_selector_message(
            tenant_options=tenant_options,
            selected_tenant=selected_tenant,
            namespace_options=namespace_options,
            selected_namespace=selected_namespace,
            show_selected_tenant_in_summary=show_tenant_in_summary,
        ),
    }


def resolve_case_insensitive_catalog_target_value(valid_values, requested_value: str):
    requested = str(requested_value or "").strip()
    if requested == "":
        return ""

    if requested in valid_values:
        return requested

    requested_key = requested.lower()
    matches = [
        str(valid_value or "").strip()
        for valid_value in valid_values
        if str(valid_value or "").strip().lower() == requested_key
    ]
    if len(matches) == 1:
        return matches[0]
    return ""


def resolve_catalog_deploy_target_selection(selector_context, *, requested_tenant="", requested_namespace="", allow_implicit=False):
    tenant = str(requested_tenant or "").strip()
    namespace = str(requested_namespace or "").strip()
    namespace_names_by_tenant = selector_context.get("namespace_names_by_tenant", {}) if isinstance(selector_context, dict) else {}

    if tenant == "" and namespace == "":
        if allow_implicit and selector_context.get("resolved"):
            return (
                str(selector_context.get("selected_tenant", "") or "").strip(),
                str(selector_context.get("selected_namespace", "") or "").strip(),
            )
        raise ValueError(
            selector_context.get("message")
            or "Deploy Now requires a tenant and namespace selection. Use Customize & Deploy instead."
        )

    if tenant == "" or namespace == "":
        raise ValueError("Both `tenant` and `namespace` are required when selecting a Deploy Now target.")

    canonical_tenant = resolve_case_insensitive_catalog_target_value(namespace_names_by_tenant.keys(), tenant)
    if canonical_tenant == "":
        raise ValueError(f"Selected tenant `{tenant}` is no longer available or you no longer have access to it.")

    valid_namespace_names = namespace_names_by_tenant.get(canonical_tenant)
    if valid_namespace_names is None:
        raise ValueError(f"Selected tenant `{tenant}` is no longer available or you no longer have access to it.")

    canonical_namespace = resolve_case_insensitive_catalog_target_value(valid_namespace_names, namespace)
    if canonical_namespace == "":
        raise ValueError(
            f"Selected namespace `{namespace}` under tenant `{tenant}` is no longer available or you no longer have access to it."
        )

    return canonical_tenant, canonical_namespace


@prefix_bp.route("/catalog/deploy-target-context", methods=["GET"])
@require_login
def CatalogDeployTargetContext():
    try:
        return jsonify(build_catalog_deploy_target_selector_context())
    except RuntimeError as e:
        return json_error(str(e), 502)
    except Exception as e:
        return json_error(f"failed to load catalog deploy target context: {e}", 500)


def normalize_catalog_slug_component(value, field_name):
    normalized = str(value or "").strip().lower()
    if normalized == "":
        raise ValueError(f"`{field_name}` is required")
    normalized = re.sub(r"\s+", "-", normalized)
    normalized = re.sub(r"[^a-z0-9._-]+", "-", normalized)
    normalized = re.sub(r"-{2,}", "-", normalized).strip("-")
    if normalized == "":
        raise ValueError(f"`{field_name}` must contain at least one alphanumeric character")
    return normalized


def build_catalog_slug(provider, display_name):
    provider_slug = normalize_catalog_slug_component(provider, "provider")
    display_name_slug = normalize_catalog_slug_component(display_name, "display_name")
    return f"{provider_slug}--{display_name_slug}"


def build_catalog_default_func_name(catalog_entry):
    if not isinstance(catalog_entry, dict):
        raise ValueError("Invalid catalog model")

    source_model_id = str(catalog_entry.get("source_model_id", "")).strip()
    source_model_parts = [part.strip() for part in source_model_id.split("/") if part.strip() != ""]
    if source_model_parts:
        return source_model_parts[-1]

    display_name = str(catalog_entry.get("display_name", "")).strip()
    if display_name != "":
        return display_name

    slug = str(catalog_entry.get("slug", "")).strip()
    if slug != "":
        return slug

        raise ValueError("Catalog model is missing source_model_id, display_name, and slug")


def normalize_catalog_string_field(value, field_name, *, allow_empty=False):
    normalized = str(value or "").strip()
    if not allow_empty and normalized == "":
        raise ValueError(f"`{field_name}` is required")
    return normalized


def normalize_catalog_string_list(values, field_name):
    if values is None:
        return []
    if not isinstance(values, list):
        raise ValueError(f"`{field_name}` must be an array of strings")

    normalized = []
    seen = set()
    for idx, item in enumerate(values):
        if not isinstance(item, str):
            raise ValueError(f"`{field_name}[{idx}]` must be a string")
        token = item.strip()
        if token == "":
            continue
        lowered = token.lower()
        if lowered in seen:
            continue
        seen.add(lowered)
        normalized.append(token)
    return normalized


def normalize_catalog_parameter_count(value):
    if value is None or value == "":
        return None
    if isinstance(value, bool):
        raise ValueError("`parameter_count_b` must be a number")
    try:
        normalized = round(float(value), 2)
    except Exception:
        raise ValueError("`parameter_count_b` must be a number")
    if normalized <= 0:
        raise ValueError("`parameter_count_b` must be > 0")
    return normalized


def find_model_command_values(commands):
    values = []
    if not isinstance(commands, list):
        return values
    i = 0
    while i < len(commands):
        token = commands[i]
        if token == "--model":
            if i + 1 >= len(commands):
                values.append("")
                break
            values.append(str(commands[i + 1]).strip())
            i += 2
            continue
        if isinstance(token, str) and token.startswith("--model="):
            values.append(token.split("=", 1)[1].strip())
        i += 1
    return values


def validate_catalog_command_tokens(commands):
    unsafe_markers = ("&&", "||", ";", "|", "`", "$(", "\n", "\r")
    for idx, token in enumerate(commands):
        if any(marker in token for marker in unsafe_markers):
            raise ValueError(f"catalog `default_func_spec.commands[{idx}]` contains an unsafe shell token")


def validate_catalog_mounts(mounts):
    if mounts is None:
        return [], False
    if not isinstance(mounts, list):
        raise ValueError("catalog `default_func_spec.mounts` must be an array")

    normalized_mounts = []
    has_hf_cache_mount = False
    for idx, mount in enumerate(mounts):
        if not isinstance(mount, dict):
            raise ValueError(f"catalog `default_func_spec.mounts[{idx}]` must be an object")
        hostpath = normalize_catalog_string_field(mount.get("hostpath"), f"default_func_spec.mounts[{idx}].hostpath")
        mountpath = normalize_catalog_string_field(mount.get("mountpath"), f"default_func_spec.mounts[{idx}].mountpath")
        if not any(hostpath.startswith(prefix) for prefix in CATALOG_ALLOWED_MOUNT_HOSTPATH_PREFIXES):
            raise ValueError(f"catalog `default_func_spec.mounts[{idx}].hostpath` must start with an approved prefix")
        if mountpath == CATALOG_HF_CACHE_MOUNTPATH:
            has_hf_cache_mount = True
        normalized_mounts.append({"hostpath": hostpath, "mountpath": mountpath})
    return normalized_mounts, has_hf_cache_mount


def validate_catalog_policy(policy):
    if policy is None:
        return None
    if not isinstance(policy, dict):
        raise ValueError("catalog `default_func_spec.policy` must be an object")
    policy_keys = set(policy.keys())
    if policy_keys == {"Link"}:
        link = policy.get("Link")
        if not isinstance(link, dict):
            raise ValueError("catalog `default_func_spec.policy.Link` must be an object")
        normalized_link = {
            "objType": normalize_catalog_string_field(link.get("objType"), "default_func_spec.policy.Link.objType"),
            "namespace": normalize_catalog_string_field(link.get("namespace"), "default_func_spec.policy.Link.namespace"),
            "name": normalize_catalog_string_field(link.get("name"), "default_func_spec.policy.Link.name"),
        }
        return {"Link": normalized_link}
    if policy_keys == {"Obj"}:
        obj = policy.get("Obj")
        if not isinstance(obj, dict):
            raise ValueError("catalog `default_func_spec.policy.Obj` must be an object")
        normalized_obj = {}
        integer_fields = {
            "min_replica": 0,
            "max_replica": 1,
            "standby_per_node": 0,
            "parallel": 1,
            "queue_len": 1,
        }
        for field_name, minimum in integer_fields.items():
            if field_name not in obj:
                if field_name in ("min_replica", "max_replica", "standby_per_node"):
                    raise ValueError(f"catalog `default_func_spec.policy.Obj.{field_name}` is required")
                continue
            value = obj[field_name]
            if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
                comparator = ">=" if minimum == 0 else ">="
                raise ValueError(f"catalog `default_func_spec.policy.Obj.{field_name}` must be an integer {comparator} {minimum}")
            normalized_obj[field_name] = value
        if (
            "min_replica" in normalized_obj
            and "max_replica" in normalized_obj
            and normalized_obj["max_replica"] < normalized_obj["min_replica"]
        ):
            raise ValueError("catalog `default_func_spec.policy.Obj.max_replica` must be >= min_replica")

        for field_name in ("queue_timeout", "scalein_timeout"):
            if field_name not in obj:
                continue
            value = obj[field_name]
            if not isinstance(value, (int, float)) or isinstance(value, bool) or float(value) < 0:
                raise ValueError(f"catalog `default_func_spec.policy.Obj.{field_name}` must be a number >= 0")
            normalized_obj[field_name] = float(value)

        if "runtime_config" in obj:
            runtime_config = obj["runtime_config"]
            if not isinstance(runtime_config, dict):
                raise ValueError("catalog `default_func_spec.policy.Obj.runtime_config` must be an object")
            normalized_runtime_config = {}
            if "graph_sync" in runtime_config:
                if not isinstance(runtime_config["graph_sync"], bool):
                    raise ValueError("catalog `default_func_spec.policy.Obj.runtime_config.graph_sync` must be a boolean")
                normalized_runtime_config["graph_sync"] = runtime_config["graph_sync"]
            normalized_obj["runtime_config"] = normalized_runtime_config

        if "scaleout_policy" in obj:
            scaleout_policy = obj["scaleout_policy"]
            if not isinstance(scaleout_policy, dict) or set(scaleout_policy.keys()) != {"WaitQueueRatio"}:
                raise ValueError("catalog `default_func_spec.policy.Obj.scaleout_policy` must be `{ \"WaitQueueRatio\": { ... } }`")
            wait_queue_ratio = scaleout_policy.get("WaitQueueRatio")
            if not isinstance(wait_queue_ratio, dict):
                raise ValueError("catalog `default_func_spec.policy.Obj.scaleout_policy.WaitQueueRatio` must be an object")
            wait_ratio = wait_queue_ratio.get("wait_ratio")
            if not isinstance(wait_ratio, (int, float)) or isinstance(wait_ratio, bool) or float(wait_ratio) < 0:
                raise ValueError("catalog `default_func_spec.policy.Obj.scaleout_policy.WaitQueueRatio.wait_ratio` must be a number >= 0")
            normalized_obj["scaleout_policy"] = {"WaitQueueRatio": {"wait_ratio": float(wait_ratio)}}

        return {"Obj": normalized_obj}

    raise ValueError("catalog `default_func_spec.policy` must contain exactly one of `Obj` or `Link`")


def validate_catalog_template_spec(spec, *, source_model_id="", max_node_vram=0, max_node_gpu_count=0):
    if not isinstance(spec, dict):
        raise ValueError("catalog `default_func_spec` must be an object")

    normalized_spec = clone_json_value(spec)
    missing_keys = sorted(CATALOG_FULL_SPEC_REQUIRED_KEYS - set(spec.keys()))
    if missing_keys:
        raise ValueError(f"catalog `default_func_spec` is missing `{missing_keys[0]}`")

    image = validate_image_value(
        normalized_spec.get("image"),
        label="catalog `default_func_spec.image`",
        require_whitelist=False,
        require_tag=True,
    )
    normalized_spec["image"] = image

    commands = normalized_spec.get("commands")
    if not isinstance(commands, list) or any(not isinstance(item, str) for item in commands):
        raise ValueError("catalog `default_func_spec.commands` must be an array of strings")
    commands = [str(item) for item in commands]
    validate_catalog_command_tokens(commands)
    hf_model = resolve_effective_model_target_from_spec(
        normalized_spec,
        commands_label="catalog `default_func_spec.commands`",
        sample_query_label="catalog `default_func_spec.sample_query.body.model`",
    ).strip()
    if hf_model != "" and source_model_id != "" and hf_model != str(source_model_id).strip():
        raise ValueError("catalog `default_func_spec` effective model target must match `source_model_id`")
    normalized_spec["commands"] = commands

    resources = normalized_spec.get("resources")
    if not isinstance(resources, dict):
        raise ValueError("catalog `default_func_spec.resources` must be an object")
    cpu = resources.get("CPU")
    mem = resources.get("Mem")
    if not isinstance(cpu, int) or isinstance(cpu, bool) or cpu <= 0:
        raise ValueError("catalog `default_func_spec.resources.CPU` must be a positive integer")
    if not isinstance(mem, int) or isinstance(mem, bool) or mem <= 0:
        raise ValueError("catalog `default_func_spec.resources.Mem` must be a positive integer")
    for optional_key in ("ReadyMem", "CacheMem"):
        if optional_key in resources:
            optional_value = resources.get(optional_key)
            if not isinstance(optional_value, int) or isinstance(optional_value, bool) or optional_value < 0:
                raise ValueError(f"catalog `default_func_spec.resources.{optional_key}` must be a non-negative integer")
    gpu = resources.get("GPU")
    if not isinstance(gpu, dict):
        raise ValueError("catalog `default_func_spec.resources.GPU` must be an object")
    for key in ("Count", "vRam"):
        value = gpu.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise ValueError(f"catalog `default_func_spec.resources.GPU.{key}` must be a positive integer")
    if max_node_gpu_count > 0 and gpu["Count"] > max_node_gpu_count:
        raise ValueError(f"catalog `default_func_spec.resources.GPU.Count` must be <= node max GPU count ({max_node_gpu_count})")
    if max_node_vram > 0 and gpu["vRam"] > max_node_vram:
        raise ValueError(f"catalog `default_func_spec.resources.GPU.vRam` must be <= node max ({max_node_vram})")
    if "Type" in gpu and (not isinstance(gpu.get("Type"), str) or str(gpu.get("Type")).strip() == ""):
        raise ValueError("catalog `default_func_spec.resources.GPU.Type` must be a non-empty string when present")

    envs = normalized_spec.get("envs")
    if not isinstance(envs, list):
        raise ValueError("catalog `default_func_spec.envs` must be an array")
    normalized_envs = []
    env_keys = set()
    for idx, pair in enumerate(envs):
        if not isinstance(pair, list) or len(pair) != 2:
            raise ValueError(f"catalog `default_func_spec.envs[{idx}]` must be [key, value]")
        if not isinstance(pair[0], str) or not isinstance(pair[1], str):
            raise ValueError(f"catalog `default_func_spec.envs[{idx}]` key/value must be strings")
        key = pair[0].strip()
        value = pair[1]
        if key == "":
            raise ValueError(f"catalog `default_func_spec.envs[{idx}]` key must be non-empty")
        if key in env_keys:
            raise ValueError(f"catalog `default_func_spec.envs` contains duplicate key `{key}`")
        env_keys.add(key)
        normalized_envs.append([key, value])
    normalized_spec["envs"] = normalized_envs

    endpoint = normalized_spec.get("endpoint")
    if not isinstance(endpoint, dict):
        raise ValueError("catalog `default_func_spec.endpoint` must be an object")
    if not isinstance(endpoint.get("port"), int) or isinstance(endpoint.get("port"), bool) or endpoint.get("port") <= 0:
        raise ValueError("catalog `default_func_spec.endpoint.port` must be a positive integer")
    if not isinstance(endpoint.get("probe"), str) or str(endpoint.get("probe")).strip() == "":
        raise ValueError("catalog `default_func_spec.endpoint.probe` must be a non-empty string")
    if not isinstance(endpoint.get("schema"), str) or str(endpoint.get("schema")).strip() == "":
        raise ValueError("catalog `default_func_spec.endpoint.schema` must be a non-empty string")
    if "probeTimeout" in endpoint:
        probe_timeout = endpoint.get("probeTimeout")
        if not isinstance(probe_timeout, int) or isinstance(probe_timeout, bool) or probe_timeout <= 0:
            raise ValueError("catalog `default_func_spec.endpoint.probeTimeout` must be a positive integer")

    sample_query = normalized_spec.get("sample_query")
    if not isinstance(sample_query, dict):
        raise ValueError("catalog `default_func_spec.sample_query` must be an object")
    api_type = normalize_catalog_api_type(sample_query.get("apiType"))
    if api_type not in CATALOG_ALLOWED_API_TYPES:
        raise ValueError("catalog `default_func_spec.sample_query.apiType` must be a supported api type")
    if not isinstance(sample_query.get("path"), str) or str(sample_query.get("path")).strip() == "":
        raise ValueError("catalog `default_func_spec.sample_query.path` must be a non-empty string")
    if not isinstance(sample_query.get("prompt"), str) or str(sample_query.get("prompt")).strip() == "":
        raise ValueError("catalog `default_func_spec.sample_query.prompt` must be a non-empty string")
    body = sample_query.get("body")
    if not isinstance(body, dict) or len(body) == 0:
        raise ValueError("catalog `default_func_spec.sample_query.body` must be a non-empty object")
    if "model" in body:
        sample_query_model = body.get("model")
        if not isinstance(sample_query_model, str) or sample_query_model.strip() != hf_model:
            raise ValueError("catalog `default_func_spec.sample_query.body.model` must match the effective model target when present")
    normalized_spec["sample_query"]["apiType"] = api_type

    standby = normalized_spec.get("standby")
    if not isinstance(standby, dict):
        raise ValueError("catalog `default_func_spec.standby` must be an object")
    for field_name in ("gpu", "pageable", "pinned"):
        standby_value = standby.get(field_name)
        if standby_value not in CATALOG_ALLOWED_STANDBY_TYPES:
            raise ValueError(f"catalog `default_func_spec.standby.{field_name}` must be one of {sorted(CATALOG_ALLOWED_STANDBY_TYPES)}")

    normalized_mounts, has_hf_cache_mount = validate_catalog_mounts(normalized_spec.get("mounts"))
    if "mounts" in normalized_spec or normalized_mounts:
        normalized_spec["mounts"] = normalized_mounts

    entrypoint = normalized_spec.get("entrypoint")
    if entrypoint is not None:
        if not isinstance(entrypoint, list) or any(not isinstance(item, str) for item in entrypoint):
            raise ValueError("catalog `default_func_spec.entrypoint` must be an array of strings")

    normalized_policy = validate_catalog_policy(normalized_spec.get("policy"))
    if normalized_policy is not None:
        normalized_spec["policy"] = normalized_policy

    return normalized_spec


def merge_protected_envs(base_envs, customer_envs, protected_env_keys):
    ordered_keys = []
    merged_envs = {}
    base_protected = {}

    def _add_pair(key, value):
        if key not in merged_envs:
            ordered_keys.append(key)
        merged_envs[key] = value

    for pair in base_envs if isinstance(base_envs, list) else []:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key, value = pair
        if not isinstance(key, str) or not isinstance(value, str):
            continue
        _add_pair(key, value)
        if key in protected_env_keys:
            base_protected[key] = value

    for pair in customer_envs if isinstance(customer_envs, list) else []:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key, value = pair
        if not isinstance(key, str) or not isinstance(value, str):
            continue
        if key in protected_env_keys:
            if key not in base_protected:
                _add_pair(key, value)
            continue
        _add_pair(key, value)

    for key, value in base_protected.items():
        _add_pair(key, value)

    return [[key, merged_envs[key]] for key in ordered_keys]


def build_updated_full_spec_from_saved_spec(
    hf_model: str,
    spec,
    saved_spec,
    *,
    max_node_vram,
    max_node_gpu_count=0,
    editor_mode="basic",
    protected_env_keys=None,
    require_image_whitelist=True,
    locked_image_name=None,
):
    partial_spec = validate_partial_spec(
        spec,
        max_node_vram,
        max_node_gpu_count=max_node_gpu_count,
        editor_mode=editor_mode,
        require_image_whitelist=require_image_whitelist,
        locked_image_name=locked_image_name,
    )
    submitted_full_spec = build_full_spec(hf_model, partial_spec, editor_mode=editor_mode)
    saved_spec_obj = clone_json_value(saved_spec) if isinstance(saved_spec, dict) else {}
    merged_spec = clone_json_value(saved_spec_obj)
    resolved_protected_env_keys = set(protected_env_keys or RESERVED_ENV_KEYS)

    merged_spec["image"] = submitted_full_spec["image"]
    merged_spec["commands"] = clone_json_value(submitted_full_spec["commands"])

    saved_resources = merged_spec.get("resources")
    if not isinstance(saved_resources, dict):
        saved_resources = {}
    submitted_resources = submitted_full_spec.get("resources")
    if isinstance(submitted_resources, dict):
        submitted_gpu = submitted_resources.get("GPU")
        saved_gpu = saved_resources.get("GPU")
        if not isinstance(saved_gpu, dict):
            saved_gpu = {}
        if isinstance(submitted_gpu, dict):
            for key, value in submitted_gpu.items():
                if key in ("Count", "Type", "vRam") or key not in saved_gpu:
                    saved_gpu[key] = value
        if saved_gpu:
            saved_resources["GPU"] = saved_gpu
        for key in ("CPU", "Mem"):
            if key in submitted_resources:
                saved_resources[key] = clone_json_value(submitted_resources[key])
    merged_spec["resources"] = saved_resources

    merged_spec["envs"] = merge_protected_envs(
        saved_spec_obj.get("envs", []),
        submitted_full_spec.get("envs", []),
        resolved_protected_env_keys,
    )

    saved_sample_query = saved_spec_obj.get("sample_query")
    if "sample_query" in partial_spec:
        resolved_sample_query = clone_json_value(submitted_full_spec["sample_query"])
    elif isinstance(saved_sample_query, dict):
        resolved_sample_query = clone_json_value(saved_sample_query)
    else:
        resolved_sample_query = build_sample_query(hf_model)
    merged_spec["sample_query"] = resolved_sample_query

    saved_policy = saved_spec_obj.get("policy")
    submitted_policy = partial_spec.get("policy")
    submitted_full_policy = submitted_full_spec.get("policy")
    if submitted_policy and submitted_policy.get("Obj"):
        if (
            isinstance(saved_policy, dict)
            and isinstance(saved_policy.get("Obj"), dict)
            and isinstance(submitted_full_policy, dict)
            and isinstance(submitted_full_policy.get("Obj"), dict)
        ):
            resolved_policy_obj = clone_json_value(saved_policy["Obj"])
            resolved_policy_obj.update(clone_json_value(submitted_policy["Obj"]))
            merged_spec["policy"] = {
                "Obj": build_full_embedded_policy_obj(
                    resolved_policy_obj,
                    label="merged `spec.policy.Obj`",
                )
            }
        elif isinstance(submitted_full_policy, dict):
            merged_spec["policy"] = {
                "Obj": build_full_embedded_policy_obj(
                    submitted_full_policy.get("Obj", {}),
                    label="merged `spec.policy.Obj`",
                )
            }
    elif editor_mode == "basic" and isinstance(saved_policy, dict) and isinstance(saved_policy.get("Obj"), dict):
        merged_spec.pop("policy", None)
    elif isinstance(saved_policy, dict):
        merged_spec["policy"] = clone_json_value(saved_policy)
    else:
        merged_spec.pop("policy", None)

    return merged_spec


def build_catalog_customized_full_spec(
    hf_model: str,
    spec,
    base_spec,
    *,
    max_node_vram,
    max_node_gpu_count=0,
    editor_mode="basic",
    catalog_source=None,
):
    base_spec_obj = clone_json_value(base_spec) if isinstance(base_spec, dict) else {}
    locked_image_name, locked_image_tag = split_image_name_and_tag(base_spec_obj.get("image"))
    if locked_image_name == "":
        raise ValueError("catalog `default_func_spec.image` must be a non-empty tagged image reference")
    if locked_image_tag == "":
        raise ValueError("catalog `default_func_spec.image` must include a tag/version")
    resolved_hf_model = str(hf_model or "").strip()
    partial_spec = validate_partial_spec(
        spec,
        max_node_vram,
        max_node_gpu_count=max_node_gpu_count,
        editor_mode=editor_mode,
        require_image_whitelist=False,
        locked_image_name=locked_image_name,
    )
    merged_spec = clone_json_value(base_spec_obj)
    merged_spec["image"] = partial_spec["image"]
    merged_spec["commands"] = merge_catalog_runtime_commands(
        base_spec_obj.get("commands", []),
        partial_spec.get("commands", []),
        image=merged_spec["image"],
        gpu_count=partial_spec["resources"]["GPU"]["Count"],
        editor_mode=editor_mode,
        sync_tensor_parallel=resolved_hf_model != "",
    )

    merged_resources = merged_spec.get("resources")
    if not isinstance(merged_resources, dict):
        merged_resources = {}
    merged_gpu = merged_resources.get("GPU")
    if not isinstance(merged_gpu, dict):
        merged_gpu = {}
    submitted_gpu = (((partial_spec.get("resources") or {}).get("GPU")) or {})
    if isinstance(submitted_gpu, dict):
        for key, value in submitted_gpu.items():
            if key in ("Count", "Type", "vRam") or key not in merged_gpu:
                merged_gpu[key] = clone_json_value(value)
    if merged_gpu:
        merged_resources["GPU"] = merged_gpu
    merged_spec["resources"] = merged_resources

    merged_spec["envs"] = merge_protected_envs(
        base_spec_obj.get("envs", []),
        partial_spec.get("envs", []),
        CATALOG_PLATFORM_ENV_KEYS,
    )

    base_sample_query = base_spec_obj.get("sample_query")
    if "sample_query" in partial_spec:
        resolved_sample_query = build_resolved_sample_query(
            resolved_hf_model,
            partial_spec.get("sample_query"),
        )
        if isinstance(resolved_sample_query, dict) and resolved_sample_query:
            merged_spec["sample_query"] = resolved_sample_query
        elif isinstance(base_sample_query, dict):
            merged_spec["sample_query"] = clone_json_value(base_sample_query)
        elif resolved_hf_model != "":
            merged_spec["sample_query"] = build_sample_query(resolved_hf_model)
        else:
            merged_spec.pop("sample_query", None)
    elif isinstance(base_sample_query, dict):
        merged_spec["sample_query"] = clone_json_value(base_sample_query)
    elif resolved_hf_model != "":
        merged_spec["sample_query"] = build_sample_query(resolved_hf_model)
    else:
        merged_spec.pop("sample_query", None)

    base_policy = base_spec_obj.get("policy")
    submitted_policy = partial_spec.get("policy")
    if submitted_policy and submitted_policy.get("Obj"):
        if isinstance(base_policy, dict) and isinstance(base_policy.get("Obj"), dict):
            resolved_policy_obj = clone_json_value(base_policy["Obj"])
            resolved_policy_obj.update(clone_json_value(submitted_policy["Obj"]))
            merged_spec["policy"] = {
                "Obj": build_full_embedded_policy_obj(
                    resolved_policy_obj,
                    label="merged catalog `spec.policy.Obj`",
                )
            }
        else:
            merged_spec["policy"] = {
                "Obj": build_full_embedded_policy_obj(
                    submitted_policy["Obj"],
                    label="merged catalog `spec.policy.Obj`",
                )
            }
    elif editor_mode == "basic" and isinstance(base_policy, dict) and isinstance(base_policy.get("Obj"), dict):
        merged_spec.pop("policy", None)
    elif isinstance(base_policy, dict):
        merged_spec["policy"] = clone_json_value(base_policy)
    else:
        merged_spec.pop("policy", None)

    if catalog_source is not None:
        merged_spec["catalog_source"] = dict(catalog_source)

    return merged_spec


def project_spec_for_editor(
    spec,
    *,
    tenant="",
    namespace="",
    name="",
    hf_model="",
    editable_env_exclusions=None,
    extra_fields=None,
):
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

    excluded_env_keys = editable_env_exclusions if editable_env_exclusions is not None else RESERVED_ENV_KEYS

    envs = spec.get("envs", [])
    if not isinstance(envs, list):
        envs = []
    clean_envs = []
    for pair in envs:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key, value = pair
        if isinstance(key, str) and isinstance(value, str) and key not in excluded_env_keys:
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
    if isinstance(policy_obj, dict) and policy_obj:
        projected_policy_obj = build_full_embedded_policy_obj(
            policy_obj,
            label="function `policy.Obj`",
        )

    resolved_hf_model = hf_model.strip() if isinstance(hf_model, str) else ""
    if resolved_hf_model == "":
        resolved_hf_model = resolve_effective_model_target_from_spec(
            spec,
            commands_label="function commands",
            sample_query_label="function sample_query.body.model",
        ).strip()

    sample_query = spec.get("sample_query")
    projected_sample_query = clone_json_value(sample_query) if isinstance(sample_query, dict) else None

    basic_spec = {
        "image": image,
        "commands": clean_commands,
        "resources": {"GPU": {"Count": gpu_count, "vRam": gpu_vram}},
        "envs": clean_envs,
        **({"policy": {"Obj": projected_policy_obj}} if projected_policy_obj else {}),
        **({"sample_query": projected_sample_query} if projected_sample_query is not None else {}),
    }
    projected_spec = json.loads(json.dumps(basic_spec))
    projected_spec["commands"] = full_commands_for_advanced
    advanced_spec = json.loads(json.dumps(projected_spec))

    payload = {
        "tenant": tenant,
        "namespace": namespace,
        "name": name,
        "hf_model": resolved_hf_model,
        "spec": projected_spec,
        "basic_spec": basic_spec,
        "advanced_spec": advanced_spec,
    }
    if isinstance(extra_fields, dict):
        payload.update(extra_fields)
    return payload


def json_values_equal_for_basic_compatibility(lhs, rhs):
    if isinstance(lhs, bool) or isinstance(rhs, bool):
        return isinstance(lhs, bool) and isinstance(rhs, bool) and lhs == rhs

    lhs_is_number = isinstance(lhs, (int, float)) and not isinstance(lhs, bool)
    rhs_is_number = isinstance(rhs, (int, float)) and not isinstance(rhs, bool)
    if lhs_is_number or rhs_is_number:
        return lhs_is_number and rhs_is_number and float(lhs) == float(rhs)

    if isinstance(lhs, str) or isinstance(rhs, str):
        return isinstance(lhs, str) and isinstance(rhs, str) and lhs == rhs

    if lhs is None or rhs is None:
        return lhs is None and rhs is None

    return type(lhs) is type(rhs) and lhs == rhs


def find_first_basic_compatibility_difference(lhs, rhs, path="spec"):
    if isinstance(lhs, dict) and isinstance(rhs, dict):
        lhs_keys = set(lhs.keys())
        rhs_keys = set(rhs.keys())
        for key in sorted(lhs_keys - rhs_keys):
            return f"{path}.{key}"
        for key in sorted(rhs_keys - lhs_keys):
            return f"{path}.{key}"
        for key in sorted(lhs_keys):
            diff_path = find_first_basic_compatibility_difference(lhs[key], rhs[key], f"{path}.{key}")
            if diff_path is not None:
                return diff_path
        return None

    if isinstance(lhs, list) and isinstance(rhs, list):
        if len(lhs) != len(rhs):
            return f"{path}.length"
        for idx, (lhs_item, rhs_item) in enumerate(zip(lhs, rhs)):
            diff_path = find_first_basic_compatibility_difference(lhs_item, rhs_item, f"{path}[{idx}]")
            if diff_path is not None:
                return diff_path
        return None

    if json_values_equal_for_basic_compatibility(lhs, rhs):
        return None
    return path


def assess_basic_mode_compatibility_for_edit(
    saved_spec,
    projected_payload,
    *,
    catalog_source=None,
):
    saved_spec_obj = clone_json_value(saved_spec) if isinstance(saved_spec, dict) else {}
    projected_obj = projected_payload if isinstance(projected_payload, dict) else {}
    basic_spec = clone_json_value(projected_obj.get("basic_spec") if isinstance(projected_obj.get("basic_spec"), dict) else {})

    try:
        if catalog_source is not None:
            hf_model = resolve_effective_model_target_from_spec(
                saved_spec_obj,
                commands_label="existing catalog-backed function commands",
                sample_query_label="existing catalog-backed function sample_query.body.model",
            ).strip()
            rebuilt_spec = build_catalog_customized_full_spec(
                hf_model,
                basic_spec,
                saved_spec_obj,
                max_node_vram=0,
                max_node_gpu_count=0,
                editor_mode="basic",
                catalog_source=catalog_source,
            )
        else:
            hf_model = str(projected_obj.get("hf_model", "")).strip()
            if hf_model == "":
                hf_model = resolve_effective_model_target_from_spec(
                    saved_spec_obj,
                    commands_label="existing function commands",
                    sample_query_label="existing function sample_query.body.model",
                ).strip()
            if hf_model == "":
                raise ValueError("existing function spec is missing an effective model target")
            rebuilt_spec = build_updated_full_spec_from_saved_spec(
                hf_model,
                basic_spec,
                saved_spec_obj,
                max_node_vram=0,
                max_node_gpu_count=0,
                editor_mode="basic",
                protected_env_keys=RESERVED_ENV_KEYS,
                require_image_whitelist=True,
                locked_image_name=None,
            )
    except Exception as e:
        return {
            "basic_mode_supported": False,
            "initial_tab": "advanced",
            "basic_mode_incompatibility_reason": f"Basic mode is unavailable because the no-op Basic compatibility simulation failed: {e}",
        }

    diff_path = find_first_basic_compatibility_difference(saved_spec_obj, rebuilt_spec, path="spec")
    if diff_path is None:
        return {
            "basic_mode_supported": True,
            "initial_tab": "basic",
            "basic_mode_incompatibility_reason": "",
        }

    return {
        "basic_mode_supported": False,
        "initial_tab": "advanced",
        "basic_mode_incompatibility_reason": f"Basic mode is unavailable because a no-op Basic save would rewrite `{diff_path}`.",
    }


def project_catalog_entry_for_create(catalog_entry):
    if not isinstance(catalog_entry, dict):
        raise ValueError("Invalid catalog model")

    slug = str(catalog_entry.get("slug", "")).strip()
    if slug == "":
        raise ValueError("Catalog model slug is missing")

    catalog_source = {
        "catalog_id": int(catalog_entry["id"]),
        "catalog_version": int(catalog_entry["catalog_version"]),
    }
    spec = validate_catalog_template_spec(catalog_entry.get("default_func_spec"))
    default_name = build_catalog_default_func_name(catalog_entry)

    return project_spec_for_editor(
        spec,
        name=default_name,
        editable_env_exclusions=CATALOG_PLATFORM_ENV_KEYS,
        extra_fields={
            "catalog_source": catalog_source,
            "catalog_mode": True,
            "catalog_display_name": str(catalog_entry.get("display_name", "")).strip(),
            "catalog_slug": slug,
        },
    )


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

    catalog_source = normalize_catalog_source(spec.get("catalog_source"))
    extra_fields = {}
    editable_env_exclusions = RESERVED_ENV_KEYS
    if catalog_source is not None:
        extra_fields["catalog_source"] = catalog_source
        extra_fields["catalog_mode"] = True
        extra_fields.update(maybe_get_catalog_entry_metadata(catalog_source))
        editable_env_exclusions = CATALOG_PLATFORM_ENV_KEYS

    projected_payload = project_spec_for_editor(
        spec,
        tenant=tenant,
        namespace=namespace,
        name=name,
        editable_env_exclusions=editable_env_exclusions,
        extra_fields=extra_fields,
    )
    projected_payload.update(
        assess_basic_mode_compatibility_for_edit(
            spec,
            projected_payload,
            catalog_source=catalog_source,
        )
    )
    return projected_payload


def parse_edit_key(edit_key: str):
    parts = [part.strip() for part in (edit_key or "").split("/")]
    if len(parts) != 3 or any(part == "" for part in parts):
        raise ValueError("`edit` must be `<tenant>/<namespace>/<name>`")
    return parts[0], parts[1], parts[2]

def listroles():
    if hasattr(g, "_cached_dashboard_roles"):
        return g._cached_dashboard_roles

    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/rbac/roles/".format(apihostaddr)
    resp = requests.get(url, headers=headers)
    roles = json.loads(resp.content)
    g._cached_dashboard_roles = roles
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


@prefix_bp.route("/catalog", methods=["GET"])
@require_login
def CatalogList():
    try:
        is_inferx_admin = is_inferx_admin_user()
        entries = list_catalog_entries(active_only=not is_inferx_admin)
        filter_options = collect_catalog_filter_options(entries)
        deploy_target = build_catalog_deploy_target_selector_context()
        published_entry_count = sum(1 for entry in entries if bool(entry.get("is_active")))
        unpublished_entry_count = max(0, len(entries) - published_entry_count)
    except RuntimeError as e:
        return json_error(str(e), 502)
    except Exception as e:
        return json_error(f"failed to load catalog: {e}", 500)

    return render_template(
        "catalog_list.html",
        catalog_entries=entries,
        provider_options=filter_options["providers"],
        modality_options=filter_options["modalities"],
        api_type_options=filter_options["api_types"],
        tag_options=filter_options["tags"],
        use_case_options=filter_options["use_cases"],
        deploy_target=deploy_target,
        is_inferx_admin=is_inferx_admin,
        published_entry_count=published_entry_count,
        unpublished_entry_count=unpublished_entry_count,
    )


@prefix_bp.route("/catalog/<slug>", methods=["GET"])
@require_login
def CatalogDetail(slug):
    try:
        is_inferx_admin = is_inferx_admin_user()
        entry = fetch_catalog_entry_by_slug_for_current_user(slug)
        deploy_target = build_catalog_deploy_target_selector_context()
    except ValueError as e:
        return json_error(str(e), 400)
    except LookupError:
        return render_resource_unavailable_page(
            resource_kind="Catalog Model",
            resource_name=slug,
            message="This catalog model is no longer available.",
            suggestion="It may be unpublished or the URL may be outdated.",
            primary_href=dashboard_href("prefix.CatalogList"),
            primary_label="Back to Catalog",
            secondary_href=dashboard_href("prefix.ListFunc"),
            secondary_label="My Models",
            status=404,
        )
    except RuntimeError as e:
        return json_error(str(e), 502)
    except Exception as e:
        return json_error(f"failed to load catalog model: {e}", 500)

    catalog_funcspec = "{}"
    catalog_full_funcspec = "{}"
    try:
        full_catalog_spec = validate_catalog_template_spec(entry.get("default_func_spec"))
        catalog_edit_data = project_spec_for_editor(
            full_catalog_spec,
            editable_env_exclusions=CATALOG_PLATFORM_ENV_KEYS,
        )
        catalog_funcspec = json.dumps(catalog_edit_data["spec"], indent=4)
        catalog_full_funcspec = json.dumps(full_catalog_spec, indent=4)
    except Exception:
        if isinstance(entry, dict) and isinstance(entry.get("default_func_spec"), dict):
            catalog_full_funcspec = json.dumps(entry["default_func_spec"], indent=4)

    entry_for_view = clone_json_value(entry) if isinstance(entry, dict) else {}
    if not is_inferx_admin and isinstance(entry_for_view, dict):
        entry_for_view.pop("default_func_spec", None)

    return render_template(
        "catalog_detail.html",
        catalog_entry=entry_for_view,
        catalog_funcspec=catalog_funcspec,
        catalog_full_funcspec=catalog_full_funcspec,
        deploy_target=deploy_target,
        is_inferx_admin=is_inferx_admin,
    )


@prefix_bp.route("/catalog/deploy/<int:catalog_id>", methods=["POST"])
@require_login
def CatalogDeployNow(catalog_id: int):
    try:
        entry = fetch_catalog_entry_for_current_user(catalog_id)
        deploy_target = build_catalog_deploy_target_selector_context()
        req = request.get_json(silent=True)
        if req is None:
            req = {}
        if not isinstance(req, dict):
            return json_error("Request body must be a JSON object when selecting a deploy target.", 400)

        catalog_source = {
            "catalog_id": int(entry["id"]),
            "catalog_version": int(entry["catalog_version"]),
        }
        full_spec = validate_catalog_template_spec(entry.get("default_func_spec"))
        full_spec["catalog_source"] = dict(catalog_source)

        tenant, namespace = resolve_catalog_deploy_target_selection(
            deploy_target,
            requested_tenant=req.get("tenant", ""),
            requested_namespace=req.get("namespace", ""),
            allow_implicit=True,
        )
        name = build_catalog_default_func_name(entry)

        gateway_req = {
            "type": "function",
            "tenant": tenant,
            "namespace": namespace,
            "name": name,
            "object": {
                "spec": full_spec
            }
        }

        try:
            resp = requests.request(
                "PUT",
                f"{apihostaddr}/object/",
                headers=gateway_headers(include_json=True),
                json=gateway_req,
                timeout=60,
            )
        except requests.exceptions.RequestException as e:
            return json_error(f"Error connecting to gateway: {e}", 502)

        if resp.status_code == 409:
            return json_error(
                f"A model named `{name}` already exists in your namespace. Use Customize & Deploy to choose a different name.",
                409,
            )
        if not resp.ok:
            body = (resp.text or "").strip()
            if body == "":
                body = f"gateway returned HTTP {resp.status_code}"
            return json_error(body, resp.status_code)

        return jsonify({
            "name": name,
            "tenant": tenant,
            "namespace": namespace,
            "redirect_url": dashboard_href("prefix.GetFunc", tenant=tenant, namespace=namespace, name=name)
        })
    except LookupError as e:
        return json_error(str(e), 404)
    except RuntimeError as e:
        return json_error(str(e), 502)
    except ValueError as e:
        return json_error(str(e), 400)
    except Exception as e:
        return json_error(f"failed to deploy catalog model: {e}", 500)

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
    catalog_id_raw = request.args.get("catalog_id", "").strip()
    initial_model_data = None
    page_mode = "create"
    create_target_context = None

    if edit_key != "" and catalog_id_raw != "":
        return json_error("`edit` and `catalog_id` cannot be used together", 400)

    if catalog_id_raw != "":
        try:
            catalog_id = parse_catalog_id(catalog_id_raw)
            catalog_entry = fetch_catalog_entry_for_current_user(catalog_id)
            initial_model_data = project_catalog_entry_for_create(catalog_entry)
        except ValueError as e:
            return json_error(str(e), 400)
        except LookupError as e:
            return json_error(str(e), 404)
        except RuntimeError as e:
            return json_error(str(e), 502)
        except Exception as e:
            return json_error(f"failed to load catalog model: {e}", 500)
    elif edit_key != "":
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

    if page_mode == "create":
        try:
            create_target_context = build_catalog_deploy_target_selector_context()
        except Exception as e:
            app.logger.warning("func create deploy target lookup failed: %s", e)
            create_target_context = build_catalog_deploy_target_unavailable_context(
                "Tenant and namespace selection is temporarily unavailable because access could not be loaded."
            )

        if isinstance(initial_model_data, dict):
            default_tenant = str((create_target_context or {}).get("selected_tenant", "") or "").strip()
            default_namespace = str((create_target_context or {}).get("selected_namespace", "") or "").strip()
            if default_tenant != "" and str(initial_model_data.get("tenant", "") or "").strip() == "":
                initial_model_data["tenant"] = default_tenant
            if default_namespace != "" and str(initial_model_data.get("namespace", "") or "").strip() == "":
                initial_model_data["namespace"] = default_namespace

    return render_template(
        "func_create.html",
        page_mode=page_mode,
        edit_key=edit_key,
        initial_model_data=initial_model_data,
        create_target_context=create_target_context,
        image_options=VLLM_IMAGE_WHITELIST,
        gpu_count_options=SUPPORTED_GPU_COUNTS,
        default_sample_query_template=build_sample_query(""),
    )


@prefix_bp.route("/func_save", methods=["POST"])
@require_login
def func_save():
    req = request.get_json(silent=True)
    if not isinstance(req, dict):
        return json_error("Request body must be a JSON object", 400)

    allowed_top_level_keys = {"mode", "tenant", "namespace", "name", "hf_model", "spec", "editor_mode", "catalog_id"}
    unknown_top_level_keys = set(req.keys()) - allowed_top_level_keys
    if unknown_top_level_keys:
        return json_error(f"Unknown top-level key: {sorted(unknown_top_level_keys)[0]}", 400)

    for key in ("mode", "tenant", "namespace", "name", "spec"):
        if key not in req:
            return json_error(f"Missing required field: `{key}`", 400)

    try:
        catalog_id = parse_catalog_id(req.get("catalog_id"))
    except ValueError as e:
        return json_error(str(e), 400)

    mode = req.get("mode")
    if mode not in ("create", "update"):
        return json_error("`mode` must be \"create\" or \"update\"", 400)
    editor_mode = req.get("editor_mode", "basic")
    if editor_mode not in ("basic", "advanced"):
        return json_error("`editor_mode` must be \"basic\" or \"advanced\"", 400)

    tenant = req.get("tenant")
    namespace = req.get("namespace")
    name = req.get("name")
    hf_model = req.get("hf_model", "")
    app.logger.info(
        "func_save received hf_model=%r tenant=%r namespace=%r name=%r mode=%r editor_mode=%r catalog_id=%r",
        hf_model,
        tenant,
        namespace,
        name,
        mode,
        editor_mode,
        catalog_id,
    )
    spec = req.get("spec")

    for field_name, field_value in (("tenant", tenant), ("namespace", namespace), ("name", name)):
        if not isinstance(field_value, str) or field_value.strip() == "":
            return json_error(f"`{field_name}` must be a non-empty string", 400)
    if hf_model is None:
        hf_model = ""
    if not isinstance(hf_model, str):
        return json_error("`hf_model` must be a string", 400)

    tenant = tenant.strip()
    namespace = namespace.strip()
    name = name.strip()
    hf_model = hf_model.strip()
    app.logger.info(
        "func_save normalized hf_model=%r tenant=%r namespace=%r name=%r mode=%r editor_mode=%r catalog_id=%r",
        hf_model,
        tenant,
        namespace,
        name,
        mode,
        editor_mode,
        catalog_id,
    )

    if mode == "update" and catalog_id is not None:
        return json_error("`catalog_id` is only allowed when creating a new model from the catalog", 400)

    try:
        max_node_vram, max_node_gpu_count = get_node_capacity_limits()
    except Exception as e:
        return json_error(f"failed to read node capacity limits: {e}", 502)

    existing_spec = None
    existing_catalog_source = None
    if mode == "update":
        try:
            existing_full_func = getfunc(tenant, namespace, name)
            existing_spec = ((((existing_full_func.get("func") or {}).get("object") or {}).get("spec")) or {})
            if not isinstance(existing_spec, dict):
                return json_error("existing function spec is missing", 502)
            existing_catalog_source = normalize_catalog_source(existing_spec.get("catalog_source"))
        except Exception as e:
            return json_error(f"failed to load existing model: {e}", 502)

    try:
        if mode == "update":
            if existing_catalog_source is not None:
                hf_model = resolve_effective_model_target_from_spec(
                    existing_spec,
                    commands_label="existing catalog-backed function commands",
                    sample_query_label="existing catalog-backed function sample_query.body.model",
                ).strip()
                full_spec = build_catalog_customized_full_spec(
                    hf_model,
                    spec,
                    existing_spec,
                    max_node_vram=max_node_vram,
                    max_node_gpu_count=max_node_gpu_count,
                    editor_mode=editor_mode,
                    catalog_source=existing_catalog_source,
                )
            else:
                if hf_model == "":
                    hf_model = resolve_effective_model_target_from_spec(
                        existing_spec,
                        commands_label="existing function commands",
                        sample_query_label="existing function sample_query.body.model",
                    ).strip()
                if hf_model == "":
                    raise ValueError("existing function spec is missing an effective model target")
                full_spec = build_updated_full_spec_from_saved_spec(
                    hf_model,
                    spec,
                    existing_spec,
                    max_node_vram=max_node_vram,
                    max_node_gpu_count=max_node_gpu_count,
                    editor_mode=editor_mode,
                    protected_env_keys=RESERVED_ENV_KEYS,
                    require_image_whitelist=True,
                    locked_image_name=None,
                )
        elif catalog_id is not None:
            catalog_entry = fetch_catalog_entry_for_current_user(catalog_id)
            catalog_template_spec = validate_catalog_template_spec(catalog_entry.get("default_func_spec"))
            catalog_source = {
                "catalog_id": int(catalog_entry["id"]),
                "catalog_version": int(catalog_entry["catalog_version"]),
            }
            hf_model = resolve_effective_model_target_from_spec(
                catalog_template_spec,
                commands_label="catalog `default_func_spec.commands`",
                sample_query_label="catalog `default_func_spec.sample_query.body.model`",
            ).strip()
            full_spec = build_catalog_customized_full_spec(
                hf_model,
                spec,
                catalog_template_spec,
                max_node_vram=max_node_vram,
                max_node_gpu_count=max_node_gpu_count,
                editor_mode=editor_mode,
                catalog_source=catalog_source,
            )
        else:
            if hf_model == "":
                raise ValueError("`hf_model` must be a non-empty string")
            partial_spec = validate_partial_spec(
                spec,
                max_node_vram,
                max_node_gpu_count=max_node_gpu_count,
                editor_mode=editor_mode,
            )
            full_spec = build_full_spec(hf_model, partial_spec, editor_mode=editor_mode)
        app.logger.info(
            "func_save built spec model fields editor_mode=%r command_model=%r sample_model=%r catalog_source=%r",
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
            normalize_catalog_source(full_spec.get("catalog_source")),
        )
    except LookupError as e:
        return json_error(str(e), 404)
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
            row_spec = ((row_func.get('object') or {}).get('spec')) if isinstance(row_func.get('object'), dict) else {}
            if isinstance(func, dict):
                func['can_edit_delete'] = has_admin_role_for_model(roles, row_tenant, row_namespace)
                catalog_source = normalize_catalog_source(row_spec.get("catalog_source")) if isinstance(row_spec, dict) else None
                func['catalog_ui'] = None
                if catalog_source is not None:
                    func['catalog_ui'] = {
                        "created_from_catalog": True,
                    }
        except Exception:
            if isinstance(func, dict):
                func['can_edit_delete'] = False
                func['catalog_ui'] = None
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
@require_login
@require_admin
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

    return render_template(
        "snapshot_list.html",
        snapshots=snapshots,
        is_inferx_admin=True,
    )


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
    is_inferx_admin = is_inferx_admin_user()
    snapshot_secondary_href = ""
    snapshot_secondary_label = ""
    if is_inferx_admin:
        snapshot_secondary_href = dashboard_href("prefix.ListSnapshot", tenant=tenant, namespace=namespace)
        snapshot_secondary_label = "View Snapshots"

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
            secondary_href=snapshot_secondary_href,
            secondary_label=snapshot_secondary_label,
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
            secondary_href=snapshot_secondary_href,
            secondary_label=snapshot_secondary_label,
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
            secondary_href=snapshot_secondary_href,
            secondary_label=snapshot_secondary_label,
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
