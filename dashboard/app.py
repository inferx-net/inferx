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

import ast
import json
import ipaddress
import mimetypes
import os
import re
import socket
import time
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote, urlencode, urljoin, urlparse
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
    flash,
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
DEBUG_PROXY_GATEWAY_REQUESTS = os.getenv(
    "DEBUG_PROXY_GATEWAY_REQUESTS",
    "false",
).lower() in ("1", "true", "yes")
REMOTE_IMAGE_ALLOWED_MIME_TYPES = {"image/jpeg", "image/png", "image/webp"}
REMOTE_IMAGE_FETCH_MAX_BYTES = 10 * 1024 * 1024
REMOTE_AUDIO_ALLOWED_MIME_TYPES = {
    "audio/flac",
    "audio/m4a",
    "audio/mp3",
    "audio/mp4",
    "audio/mpeg",
    "audio/ogg",
    "audio/wav",
    "audio/wave",
    "audio/vnd.wave",
    "audio/webm",
    "audio/x-flac",
    "audio/x-m4a",
    "audio/x-mp3",
    "audio/x-wav",
}
REMOTE_AUDIO_FETCH_MAX_BYTES = 25 * 1024 * 1024
REMOTE_IMAGE_FETCH_REDIRECT_LIMIT = 3
REMOTE_IMAGE_FETCH_CONNECT_TIMEOUT_SEC = 10.0
REMOTE_IMAGE_FETCH_READ_TIMEOUT_SEC = 20.0
REMOTE_IMAGE_FETCH_HEADERS = {
    "Accept": "image/jpeg,image/png,image/webp,image/*;q=0.8,*/*;q=0.1",
    "User-Agent": "InferX-Dashboard/1.0 remote-image-fetch",
}
REMOTE_AUDIO_FETCH_HEADERS = {
    "Accept": "audio/wav,audio/x-wav,audio/mpeg,audio/mp4,audio/webm,audio/ogg,audio/*;q=0.8,*/*;q=0.1",
    "User-Agent": "InferX-Dashboard/1.0 remote-audio-fetch",
}


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
OPEN_CODE_CONFIG_TEMPLATE_PATH = Path(__file__).resolve().parent.parent / "integration" / "opencode.json"
KNOWLEDGEBASE_DIR = Path("/opt/inferx/kb")
SKILLS_DIR = Path("/opt/inferx/skills")
FUNCTION_MAPPINGS_ENV = "INFERX_FUNCTION_MAPPINGS"
METADATA_GEN_MODEL = os.getenv("METADATA_GEN_MODEL", "").strip()


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


def load_function_mappings():
    raw = str(os.getenv(FUNCTION_MAPPINGS_ENV, "") or "").strip()
    if raw == "":
        return []

    try:
        parsed = json.loads(raw)
    except Exception as e:
        app.logger.warning("failed to parse %s: %s", FUNCTION_MAPPINGS_ENV, e)
        return []

    entries = parsed.get("functionmappings") if isinstance(parsed, dict) else parsed
    if not isinstance(entries, list):
        app.logger.warning("%s must be a JSON array or an object with `functionmappings`", FUNCTION_MAPPINGS_ENV)
        return []

    normalized_entries = []
    for idx, entry in enumerate(entries):
        if not isinstance(entry, dict):
            app.logger.warning("%s[%d] must be an object", FUNCTION_MAPPINGS_ENV, idx)
            continue

        preset = str(entry.get("preset", "") or "").strip()
        catalog = str(entry.get("catalog", "") or "").strip()
        token_length = entry.get("token_length")
        if preset == "" or catalog == "":
            app.logger.warning("%s[%d] is missing `preset` or `catalog`", FUNCTION_MAPPINGS_ENV, idx)
            continue

        try:
            token_length = int(token_length)
        except Exception:
            app.logger.warning("%s[%d].token_length must be an integer", FUNCTION_MAPPINGS_ENV, idx)
            continue

        if token_length <= 0:
            app.logger.warning("%s[%d].token_length must be > 0", FUNCTION_MAPPINGS_ENV, idx)
            continue

        normalized_entries.append({
            "preset": preset,
            "catalog": catalog,
            "token_length": token_length,
        })

    return normalized_entries


FUNCTION_MAPPINGS = load_function_mappings()


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
    "HF_TOKEN",
    "HUGGING_FACE_HUB_TOKEN",
}
CATALOG_PLATFORM_ENV_KEYS = RESERVED_ENV_KEYS | CATALOG_RESERVED_ENV_KEYS
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
        target = url_for('prefix.EndpointList')

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
    target = redirectpath if redirectpath != '' else url_for('prefix.EndpointList')

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
    if session.get('access_token', '') == '':
        return True

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

        target = redirectpath if redirectpath != '' else url_for('prefix.EndpointList')
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
    target = redirectpath if redirectpath != '' else url_for('prefix.EndpointList')
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


def get_published_endpoint_response(tenant: str, slug: str):
    access_token = session.get('access_token', '')
    if access_token == "":
        headers = {}
    else:
        headers = {'Authorization': f'Bearer {access_token}'}
    url = "{}/published-endpoint/{}/{}/".format(apihostaddr, tenant, slug)
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


DASHBOARD_LOCAL_TZ = pytz.timezone("America/Los_Angeles")
MODEL_STATUS_READY = {"code": "ready", "label": "Ready"}
MODEL_STATUS_RESUMING = {"code": "resuming", "label": "Resuming..."}
MODEL_STATUS_STANDBY = {"code": "standby", "label": "Standby"}
MODEL_STATUS_RESTORING = {"code": "restoring", "label": "Restoring"}
MODEL_STATUS_PENDING = {"code": "pending", "label": "Pending"}
MODEL_STATUS_SNAPSHOTTING = {"code": "snapshotting", "label": "Creating Snapshot..."}
MODEL_STATUS_FAILED = {"code": "failed", "label": "Failed"}
RESTORE_ACTIVE_POD_STATES = {
    "Init",
    "PullingImage",
    "Creating",
    "Created",
    "Loading",
    "LoadingTimeout",
    "Restoring",
}
SNAPSHOT_ACTIVE_POD_STATES = {
    "Init",
    "PullingImage",
    "Creating",
    "Created",
    "Loading",
    "Snapshoting",
}


def format_dashboard_timestamp(value):
    if not isinstance(value, str) or value.strip() == "":
        return value

    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return value

    return dt.astimezone(DASHBOARD_LOCAL_TZ).strftime("%Y-%m-%d %H:%M:%S")


def format_dashboard_timestamps(rows, field_name: str):
    if not isinstance(rows, list):
        return rows

    for row in rows:
        if not isinstance(row, dict):
            continue
        row[field_name] = format_dashboard_timestamp(row.get(field_name))

    return rows


def infer_model_status(pods, func_status_state, fails=None):
    pod_list = pods if isinstance(pods, list) else []
    fail_list = fails if isinstance(fails, list) else []

    def pod_state(pod):
        return str((((pod or {}).get("object") or {}).get("status") or {}).get("state") or "").strip()

    def pod_create_type(pod):
        return str((((pod or {}).get("object") or {}).get("spec") or {}).get("create_type") or "").strip()

    if str(func_status_state or "").strip() == "Fail":
        return MODEL_STATUS_FAILED

    # Some failed revisions no longer have pods by the time the dashboard loads.
    # Current-version fail logs are the remaining durable signal in that case.
    if len(pod_list) == 0 and len(fail_list) > 0:
        return MODEL_STATUS_FAILED

    if any(pod_state(pod) == "Ready" for pod in pod_list):
        return MODEL_STATUS_READY

    if any(pod_state(pod) in {"Resuming", "ResumeDone"} for pod in pod_list):
        return MODEL_STATUS_RESUMING

    if any(pod_state(pod) in {"Standby", "MemHibernated"} for pod in pod_list):
        return MODEL_STATUS_STANDBY

    if any(
        pod_create_type(pod) == "Restore" and pod_state(pod) in RESTORE_ACTIVE_POD_STATES
        for pod in pod_list
    ):
        return MODEL_STATUS_RESTORING

    if any(
        pod_create_type(pod) == "Snapshot" and pod_state(pod) in SNAPSHOT_ACTIVE_POD_STATES
        for pod in pod_list
    ):
        return MODEL_STATUS_SNAPSHOTTING

    # `Normal` pods do not currently have a dedicated user-facing mapping. Keep
    # the fallback conservative for early/unmapped states outside the explicit
    # restore and snapshot transitions above.
    return MODEL_STATUS_PENDING


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


def is_upstream_permission_denied(resp, payload) -> bool:
    if resp is None:
        return False

    if resp.status_code in (401, 403):
        return True

    detail = extract_upstream_error_message(resp, payload).lower()
    if detail == "":
        return False

    denied_markers = (
        "permission denied",
        "no permission",
        "nopermission",
        "unauthorized",
        "forbidden",
    )
    return any(marker in detail for marker in denied_markers)


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


JSON_VALUED_COMMAND_FLAGS = (
    "--attention-config",
    "--compilation-config",
    "--ec-transfer-config",
    "--hf-overrides",
    "--ir-op-priority",
    "--kernel-config",
    "--kv-events-config",
    "--kv-transfer-config",
    "--limit-mm-per-prompt",
    "--mamba-config",
    "--media-io-kwargs",
    "--mm-processor-kwargs",
    "--model-loader-extra-config",
    "--override-generation-config",
    "--pooler-config",
    "--quantization-config",
    "--reasoning-config",
    "--speculative-config",
    "-ac",
    "-cc",
    "-sc",
)


def normalize_json_valued_command_arg_value(flag_name, raw_value, *, label="commands"):
    normalized_value = str(raw_value).strip()
    if normalized_value == "":
        raise ValueError(f"{label} contains `{flag_name}` without a value")

    looks_like_structured_value = (
        (normalized_value.startswith("{") and normalized_value.endswith("}"))
        or (normalized_value.startswith("[") and normalized_value.endswith("]"))
    )
    if not looks_like_structured_value:
        return normalized_value

    try:
        parsed_value = json.loads(normalized_value)
    except Exception:
        try:
            parsed_value = ast.literal_eval(normalized_value)
        except Exception as exc:
            raise ValueError(
                f"{label} contains `{flag_name}` with an invalid JSON object/array value"
            ) from exc

    if not isinstance(parsed_value, (dict, list)):
        raise ValueError(f"{label} contains `{flag_name}` with a non-object/array JSON value")

    return json.dumps(parsed_value, separators=(",", ":"))


def normalize_json_valued_command_args(commands, *, label="commands"):
    raw_commands = [str(item) for item in commands] if isinstance(commands, list) else []
    normalized_commands = []

    i = 0
    while i < len(raw_commands):
        raw_token = raw_commands[i]
        token = raw_token.strip()

        matched_flag = None
        for flag_name in JSON_VALUED_COMMAND_FLAGS:
            if token == flag_name or token.startswith(f"{flag_name}="):
                matched_flag = flag_name
                break

        if matched_flag is None:
            normalized_commands.append(raw_token)
            i += 1
            continue

        if token == matched_flag:
            if i + 1 >= len(raw_commands):
                raise ValueError(f"{label} contains `{matched_flag}` without a value")
            normalized_commands.append(matched_flag)
            normalized_commands.append(
                normalize_json_valued_command_arg_value(
                    matched_flag,
                    raw_commands[i + 1],
                    label=label,
                )
            )
            i += 2
            continue

        raw_value = token.split("=", 1)[1]
        normalized_commands.append(
            f"{matched_flag}={normalize_json_valued_command_arg_value(matched_flag, raw_value, label=label)}"
        )
        i += 1

    return normalized_commands


def is_standard_vllm_image(image):
    return isinstance(image, str) and (
        image.startswith("vllm/vllm-openai:")
        or image.startswith("vllm-openai-upgraded:")
    )


def is_vllm_omni_image(image):
    return isinstance(image, str) and image.startswith("vllm/vllm-omni:")


def is_vllm_runtime_image(image):
    return is_standard_vllm_image(image) or is_vllm_omni_image(image)


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


def command_contains_named_arg(commands, flag_name):
    normalized_commands = [str(item).strip() for item in commands] if isinstance(commands, list) else []
    for token in normalized_commands:
        if token == flag_name:
            return True
        if token.startswith(f"{flag_name}="):
            return True
    return False


def should_sync_tensor_parallel_size_for_advanced_commands(image, commands):
    normalized_commands = [str(item).strip() for item in commands] if isinstance(commands, list) else []
    if len(normalized_commands) == 0:
        return False
    if command_contains_named_arg(normalized_commands, "--tensor-parallel-size"):
        return True
    if len(normalized_commands) >= 2 and normalized_commands[0] == "vllm" and normalized_commands[1] == "serve":
        return True
    if not is_vllm_runtime_image(image):
        return False
    return extract_model_arg_from_commands(normalized_commands) != ""


def looks_like_huggingface_model_target_token(value):
    normalized = str(value or "").strip()
    if normalized == "":
        return False
    if normalized.startswith(("/", "./", "../")):
        return False
    if "://" in normalized:
        return False
    return re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*(?:/[A-Za-z0-9][A-Za-z0-9._-]*)*", normalized) is not None


def normalize_effective_model_target(value):
    normalized = str(value or "").strip()
    if normalized == "":
        return ""

    path_parts = [part for part in normalized.split("/") if part != ""]
    for idx, part in enumerate(path_parts):
        if not part.startswith("models--"):
            continue
        if idx + 2 >= len(path_parts) or path_parts[idx + 1] != "snapshots":
            continue
        repo_token = part[len("models--"):].strip()
        if repo_token == "":
            continue
        repo_parts = [segment.strip() for segment in repo_token.split("--") if segment.strip() != ""]
        if repo_parts:
            return "/".join(repo_parts)

    return normalized


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
        normalized = normalize_effective_model_target(value)
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
    return normalize_effective_model_target(model_value)


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
    normalized_commands = normalize_json_valued_command_args(
        normalized_commands,
        label="`spec.commands`",
    )

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


def build_text2text_chat_body_for_ui(body, prompt):
    body_for_ui = clone_json_value(body) if isinstance(body, dict) else {}
    body_for_ui.pop("prompt", None)

    prompt_text = str(prompt or "")
    messages = body_for_ui.get("messages")
    if not isinstance(messages, list) or len(messages) == 0:
        body_for_ui["messages"] = [
            {
                "role": "user",
                "content": prompt_text,
            }
        ]
        return body_for_ui

    normalized_messages = clone_json_value(messages)
    replaced = False
    for message in normalized_messages:
        if not isinstance(message, dict):
            continue
        role = str(message.get("role", "") or "").strip().lower()
        if role != "user":
            continue

        content = message.get("content")
        if isinstance(content, str):
            message["content"] = prompt_text
            replaced = True
            break

        if isinstance(content, list):
            new_content = []
            text_replaced = False
            for item in content:
                if (
                    not text_replaced
                    and isinstance(item, dict)
                    and str(item.get("type", "") or "").strip().lower() == "text"
                ):
                    updated_item = clone_json_value(item)
                    updated_item["text"] = prompt_text
                    new_content.append(updated_item)
                    text_replaced = True
                else:
                    new_content.append(clone_json_value(item))
            if not text_replaced:
                new_content.insert(0, {"type": "text", "text": prompt_text})
            message["content"] = new_content
            replaced = True
            break

    if not replaced:
        normalized_messages.insert(
            0,
            {
                "role": "user",
                "content": prompt_text,
            },
        )
    body_for_ui["messages"] = normalized_messages
    return body_for_ui


def build_sample_query(hf_model: str):
    prompt = "write a quick sort algorithm."
    body = build_text2text_chat_body_for_ui(
        {
            "model": hf_model,
            "max_tokens": "1000",
            "temperature": "0",
            "stream": "true",
        },
        prompt,
    )
    return {
        "apiType": "text2text",
        "prompt": prompt,
        "prompts": list(DEFAULT_SAMPLE_QUERY_PROMPTS),
        "path": "v1/chat/completions",
        "body": body,
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
        if should_sync_tensor_parallel_size_for_advanced_commands(partial_spec["image"], full_commands):
            full_commands = sync_tensor_parallel_size(full_commands, gpu_count)
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


def redact_inline_media_in_value(value):
    if isinstance(value, list):
        return [redact_inline_media_in_value(item) for item in value]
    if isinstance(value, dict):
        return {key: redact_inline_media_in_value(item) for key, item in value.items()}
    if isinstance(value, str) and value.startswith("data:"):
        return f"[redacted data URL; {len(value)} chars]"
    return value


def summarize_headers_for_log(headers):
    summarized = {}
    for key, value in (headers or {}).items():
        normalized_key = str(key or "")
        if normalized_key.lower() in {"authorization", "cookie", "proxy-authorization", "x-api-key"}:
            summarized[normalized_key] = "[redacted]"
        else:
            summarized[normalized_key] = str(value)
    return summarized


def summarize_cookies_for_log(cookies):
    try:
        keys = sorted([str(key) for key in cookies.keys()])
    except Exception:
        keys = []
    return {
        "count": len(keys),
        "keys": keys,
        "values": "[redacted]",
    }


def summarize_request_body_for_log(body_bytes):
    if not body_bytes:
        return {"kind": "empty", "bytes": 0}

    body_size = len(body_bytes)
    try:
        parsed = json.loads(body_bytes.decode("utf-8"))
        return {
            "kind": "json",
            "bytes": body_size,
            "body": redact_inline_media_in_value(parsed),
        }
    except Exception:
        pass

    try:
        text = body_bytes.decode("utf-8")
    except UnicodeDecodeError:
        return {
            "kind": "binary",
            "bytes": body_size,
            "body": "[non-utf8 body omitted]",
        }

    return {
        "kind": "text",
        "bytes": body_size,
        "body": text if len(text) <= 2048 else f"{text[:2048]}... [truncated]",
    }


def maybe_log_proxy_gateway_request(path, upstream_url, method, headers, cookies, body_bytes, timeout_sec):
    if not DEBUG_PROXY_GATEWAY_REQUESTS:
        return

    app.logger.warning(
        "dashboard proxy gateway request path=%s method=%s upstream=%s timeout_sec=%s headers=%s cookies=%s body=%s",
        path,
        method,
        upstream_url,
        timeout_sec,
        summarize_headers_for_log(headers),
        summarize_cookies_for_log(cookies),
        summarize_request_body_for_log(body_bytes),
    )


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
    normalized_base = normalize_json_valued_command_args(
        [str(item) for item in base_commands] if isinstance(base_commands, list) else [],
        label="catalog base commands",
    )
    normalized_submitted = normalize_json_valued_command_args(
        [str(item) for item in submitted_commands] if isinstance(submitted_commands, list) else [],
        label="submitted catalog commands",
    )
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


def query_catalog_entry_by_source(catalog_source):
    normalized_source = normalize_catalog_source(catalog_source)
    if normalized_source is None:
        return None

    try:
        return query_catalog_entry_by_id(normalized_source["catalog_id"], active_only=False)
    except Exception:
        return None


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
    if normalized.lower() == "knowledgebase":
        return "knowledgebase"
    return normalized



def knowledgebase_stage_file_path(tenant: str, namespace: str, name: str) -> Path:
    return KNOWLEDGEBASE_DIR / f"{tenant}.{namespace}.{name}" / "kb.data"


def knowledgebase_root_dir_path(tenant: str, namespace: str, name: str) -> Path:
    return KNOWLEDGEBASE_DIR / f"{tenant}.{namespace}.{name}"


def knowledgebase_version_file_path(tenant: str, namespace: str, name: str, version: str) -> Path:
    normalized_version = str(version or "").strip()
    return knowledgebase_root_dir_path(tenant, namespace, name) / normalized_version / "kb.data"


def resolve_knowledgebase_file_path(tenant: str, namespace: str, name: str, *, live_func=None) -> Path | None:
    stage_file = knowledgebase_stage_file_path(tenant, namespace, name)
    if stage_file.exists():
        return stage_file

    version = str(((((live_func or {}).get("func") or {}).get("object") or {}).get("spec") or {}).get("version") or "").strip()
    if version != "":
        version_file = knowledgebase_version_file_path(tenant, namespace, name, version)
        if version_file.exists():
            return version_file

    return None


def validate_function_name_or_raise(name: str):
    normalized_name = str(name or "").strip()
    if normalized_name == "":
        raise ValueError("Function name is required.")
    if not all(ch.isascii() and (ch.isalnum() or ch in "-_.") for ch in normalized_name):
        raise ValueError("Function name may contain only letters, numbers, hyphens, underscores, and periods.")
    return normalized_name


def ensure_functions_db_available():
    if psycopg2 is None or RealDictCursor is None:
        raise RuntimeError("functions support requires `psycopg2-binary` in the dashboard environment")
    if SECRDB_ADDR == "":
        raise RuntimeError("SECRDB_ADDR is empty")


def user_can_manage_models(roles) -> bool:
    if not isinstance(roles, list):
        return False
    return any(
        str(role.get('role', '')).lower() == 'admin'
        and str(role.get('objType', '')).lower() in ('tenant', 'namespace')
        for role in roles
    )


def normalize_function_row(row):
    normalized = dict(row or {})
    normalized["tenant"] = str(normalized.get("tenant", "") or "").strip()
    normalized["namespace"] = str(normalized.get("namespace", "") or "").strip()
    normalized["name"] = str(normalized.get("name", "") or "").strip()
    normalized["preset"] = str(normalized.get("preset", "") or "").strip()
    normalized["catalog"] = str(normalized.get("catalog", "") or "").strip()
    try:
        normalized["token_length"] = int(normalized.get("token_length"))
    except Exception:
        normalized["token_length"] = 0
    kb_token_count = normalized.get("kb_token_count")
    if kb_token_count is None:
        normalized["kb_token_count"] = None
    else:
        try:
            normalized["kb_token_count"] = int(kb_token_count)
        except Exception:
            normalized["kb_token_count"] = None
    normalized["created_at_display"] = format_catalog_datetime(normalized.get("created_at"))
    normalized["created_at"] = normalize_catalog_datetime_value(normalized.get("created_at"))
    return normalized


def resolve_function_model_name(function_row) -> str:
    row = normalize_function_row(function_row)
    catalog_slug = str(row.get("catalog", "") or "").strip()
    if catalog_slug == "":
        return ""
    try:
        entry = query_function_mapping_catalog_entry(catalog_slug)
        return str(entry.get("display_name", "") or catalog_slug).strip()
    except Exception:
        return catalog_slug


def resolve_function_total_token_length(function_row) -> int:
    row = normalize_function_row(function_row)
    return int(row.get("token_length") or 0)


def list_function_rows(*, tenant="", namespace=""):
    ensure_functions_db_available()

    normalized_tenant = str(tenant or "").strip()
    normalized_namespace = str(namespace or "").strip()
    where_clauses = []
    params = []
    if normalized_tenant != "":
        where_clauses.append("tenant = %s")
        params.append(normalized_tenant)
    if normalized_namespace != "":
        where_clauses.append("namespace = %s")
        params.append(normalized_namespace)

    query = """
        SELECT id, tenant, namespace, name, preset, catalog, token_length, kb_token_count, created_at
        FROM functions
    """
    if where_clauses:
        query += " WHERE " + " AND ".join(where_clauses)
    query += " ORDER BY created_at DESC, id DESC"

    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, tuple(params))
                rows = cursor.fetchall()
    except Exception as e:
        raise RuntimeError(f"failed to query functions: {e}")

    return [normalize_function_row(dict(row)) for row in rows]


def query_function_row(tenant: str, namespace: str, name: str):
    ensure_functions_db_available()
    query = """
        SELECT id, tenant, namespace, name, preset, catalog, token_length, kb_token_count, created_at
        FROM functions
        WHERE tenant = %s AND namespace = %s AND name = %s
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (tenant, namespace, name))
                row = cursor.fetchone()
    except Exception as e:
        raise RuntimeError(f"failed to query function `{tenant}/{namespace}/{name}`: {e}")

    if row is None:
        raise LookupError(f"function `{tenant}/{namespace}/{name}` not found")
    return normalize_function_row(dict(row))


def find_function_row(tenant: str, namespace: str, name: str):
    try:
        return query_function_row(tenant, namespace, name)
    except LookupError:
        return None


def insert_function_row(*, tenant: str, namespace: str, name: str, preset: str, catalog: str, token_length: int, kb_token_count: int | None):
    ensure_functions_db_available()
    query = """
        INSERT INTO functions (tenant, namespace, name, preset, catalog, token_length, kb_token_count)
        VALUES (%s, %s, %s, %s, %s, %s, %s)
        RETURNING id, tenant, namespace, name, preset, catalog, token_length, kb_token_count, created_at
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (tenant, namespace, name, preset, catalog, token_length, kb_token_count))
                row = cursor.fetchone()
            conn.commit()
    except psycopg2.Error as e:
        raise e
    except Exception as e:
        raise RuntimeError(f"failed to create function row `{tenant}/{namespace}/{name}`: {e}")
    return normalize_function_row(dict(row))


def delete_function_row(tenant: str, namespace: str, name: str):
    ensure_functions_db_available()
    query = """
        DELETE FROM functions
        WHERE tenant = %s AND namespace = %s AND name = %s
        RETURNING id, tenant, namespace, name, preset, catalog, token_length, kb_token_count, created_at
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (tenant, namespace, name))
                row = cursor.fetchone()
            conn.commit()
    except Exception as e:
        raise RuntimeError(f"failed to delete function row `{tenant}/{namespace}/{name}`: {e}")
    if row is None:
        raise LookupError(f"function `{tenant}/{namespace}/{name}` not found")
    return normalize_function_row(dict(row))


def remove_function_kb_directory(tenant: str, namespace: str, name: str):
    kb_dir = knowledgebase_root_dir_path(tenant, namespace, name)
    if not kb_dir.exists():
        return
    try:
        import shutil
        shutil.rmtree(kb_dir)
    except Exception as e:
        raise RuntimeError(f"failed to delete KB directory `{kb_dir}`: {e}")


def get_function_mapping_entries():
    return [dict(entry) for entry in FUNCTION_MAPPINGS]


def query_function_mapping_catalog_entry(catalog_slug: str):
    normalized_slug = str(catalog_slug or "").strip()
    if normalized_slug == "":
        raise ValueError("catalog slug is required")
    return query_catalog_entry_by_slug(normalized_slug, active_only=False)


def resolve_function_preset_options():
    options = []
    for idx, mapping in enumerate(get_function_mapping_entries()):
        catalog_slug = str(mapping.get("catalog", "") or "").strip()
        if catalog_slug == "":
            continue
        try:
            entry = query_function_mapping_catalog_entry(catalog_slug)
            spec = validate_catalog_template_spec(entry.get("default_func_spec"))
            model_id = resolve_effective_model_target_from_spec(
                spec,
                commands_label="function mapping catalog default_func_spec.commands",
                sample_query_label="function mapping catalog default_func_spec.sample_query.body.model",
            )
        except Exception as e:
            app.logger.warning(
                "skipping function preset `%s` from %s[%d] catalog=%s: %s",
                str(mapping.get("preset", "") or "").strip(),
                FUNCTION_MAPPINGS_ENV,
                idx,
                catalog_slug,
                e,
            )
            continue

        options.append({
            "preset": str(mapping["preset"]),
            "catalog": catalog_slug,
            "token_length": int(mapping["token_length"]),
            "model_name": str(entry.get("display_name", "") or catalog_slug).strip(),
            "model_id": str(model_id or "").strip(),
            "gpu_count": extract_gpu_count_from_spec(spec) or 0,
            "catalog_slug": str(entry.get("slug", "") or catalog_slug).strip(),
            "catalog_id": int(entry.get("id")),
        })

    return options


def find_function_preset_option(preset: str):
    normalized_preset = str(preset or "").strip()
    for option in resolve_function_preset_options():
        if str(option.get("preset", "")).strip() == normalized_preset:
            return option
    return None


def diagnose_function_preset_unavailability(preset: str) -> str:
    normalized_preset = str(preset or "").strip()
    if normalized_preset == "":
        return "Preset is required."

    mappings = get_function_mapping_entries()
    matching_mapping = None
    for idx, mapping in enumerate(mappings):
        if str(mapping.get("preset", "") or "").strip() == normalized_preset:
            matching_mapping = (idx, mapping)
            break

    if matching_mapping is None:
        return f"Preset `{normalized_preset}` is not configured."

    idx, mapping = matching_mapping
    catalog_slug = str(mapping.get("catalog", "") or "").strip()
    if catalog_slug == "":
        return (
            f"Preset `{normalized_preset}` is misconfigured: "
            f"{FUNCTION_MAPPINGS_ENV}[{idx}].catalog is empty."
        )

    try:
        entry = query_function_mapping_catalog_entry(catalog_slug)
    except Exception as e:
        app.logger.warning(
            "function preset `%s` unavailable while loading catalog `%s`: %s",
            normalized_preset,
            catalog_slug,
            e,
        )
        return f"Preset `{normalized_preset}` is unavailable: failed to load catalog `{catalog_slug}`."

    try:
        spec = validate_catalog_template_spec(entry.get("default_func_spec"))
        model_id = resolve_effective_model_target_from_spec(
            spec,
            commands_label="function mapping catalog default_func_spec.commands",
            sample_query_label="function mapping catalog default_func_spec.sample_query.body.model",
        ).strip()
    except Exception as e:
        app.logger.warning(
            "function preset `%s` unavailable while resolving model from catalog `%s`: %s",
            normalized_preset,
            catalog_slug,
            e,
        )
        return (
            f"Preset `{normalized_preset}` is unavailable: catalog `{catalog_slug}` "
            f"does not resolve to a valid effective model target."
        )

    if model_id == "":
        app.logger.warning(
            "function preset `%s` unavailable: catalog `%s` resolved an empty model id",
            normalized_preset,
            catalog_slug,
        )
        return (
            f"Preset `{normalized_preset}` is unavailable: catalog `{catalog_slug}` "
            f"resolved an empty effective model target."
        )

    return f"Preset `{normalized_preset}` is unavailable."


def build_function_detail_view(function_row, *, live_func=None, live_unavailable=False):
    row = normalize_function_row(function_row)
    row["model_name"] = resolve_function_model_name(row)
    row["total_token_length"] = resolve_function_total_token_length(row)
    live_spec = ((((live_func or {}).get("func") or {}).get("object") or {}).get("spec")) or {}
    live_status = ((((live_func or {}).get("func") or {}).get("object") or {}).get("status")) or {}
    is_live_available = isinstance(live_func, dict) and not live_unavailable
    kb_file = resolve_knowledgebase_file_path(
        row["tenant"],
        row["namespace"],
        row["name"],
        live_func=live_func,
    )
    created_at_summary = row.get("created_at_display")
    created_at_value = row.get("created_at")
    if isinstance(created_at_value, str) and created_at_value.strip() != "":
        try:
            created_dt = datetime.fromisoformat(created_at_value.replace("Z", "+00:00")).astimezone(DASHBOARD_LOCAL_TZ)
            created_at_summary = created_dt.strftime("%Y-%m-%dT%H:%M:%S")
        except ValueError:
            pass
    return {
        "row": row,
        "is_live_available": is_live_available,
        "state": str(live_status.get("state", "") or "").strip() if is_live_available else "unavailable",
        "gpu_count": (
            ((live_spec.get("resources") or {}).get("GPU") or {}).get("Count")
            if is_live_available
            else None
        ),
        "stage_file_exists": kb_file is not None,
        "stage_file_name": kb_file.name if kb_file is not None else "kb.data",
        "stage_file_download_href": dashboard_href(
            "prefix.DownloadKnowledgebaseFile",
            tenant=row["tenant"],
            namespace=row["namespace"],
            name=row["name"],
        ),
        "created_at_summary": created_at_summary,
        "cleanup_href": dashboard_href(
            "prefix.FunctionDetail",
            tenant=row["tenant"],
            namespace=row["namespace"],
            name=row["name"],
        ),
    }


def deploy_catalog_entry_to_function_target(entry, *, tenant: str, namespace: str, name: str):
    catalog_source = {
        "catalog_id": int(entry["id"]),
        "catalog_version": int(entry["catalog_version"]),
    }
    full_spec = validate_catalog_template_spec(entry.get("default_func_spec"))
    full_spec["catalog_source"] = dict(catalog_source)

    gateway_req = {
        "type": "function",
        "tenant": tenant,
        "namespace": namespace,
        "name": name,
        "object": {"spec": full_spec},
    }

    return requests.request(
        "PUT",
        f"{apihostaddr}/object/",
        headers=gateway_headers(include_json=True),
        json=gateway_req,
        timeout=60,
    )


class KbTokenCountGatewayError(RuntimeError):
    def __init__(self, message: str, status_code: int):
        super().__init__(message)
        self.status_code = int(status_code)


def count_kb_tokens_via_gateway(*, tenant: str, namespace: str, name: str, model: str):
    normalized_model = str(model or "").strip()
    if normalized_model == "":
        raise ValueError("model is required")

    encoded_tenant = quote(tenant, safe="")
    encoded_namespace = quote(namespace, safe="")
    encoded_name = quote(name, safe="")
    url = f"{get_gateway_url()}/kb/token-count/{encoded_tenant}/{encoded_namespace}/{encoded_name}"

    try:
        resp = requests.post(
            url,
            headers=gateway_headers(include_json=True),
            json={"model": normalized_model},
            timeout=60,
        )
    except requests.exceptions.RequestException as e:
        raise RuntimeError(f"failed to reach gateway: {e}")

    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload) or f"gateway returned HTTP {resp.status_code}"
        raise KbTokenCountGatewayError(detail, resp.status_code)

    if not isinstance(payload, dict):
        raise RuntimeError("gateway returned an invalid KB token count response")

    try:
        return int(payload.get("kb_token_count"))
    except Exception:
        raise RuntimeError("gateway response is missing `kb_token_count`")


def delete_function_dashboard_artifacts(tenant: str, namespace: str, name: str):
    remove_function_kb_directory(tenant, namespace, name)
    delete_function_row(tenant, namespace, name)


def delete_function_via_dashboard(tenant: str, namespace: str, name: str):
    live_exists = False
    try:
        func_resp, func_payload = getfunc_response(tenant, namespace, name)
        if func_resp.ok and isinstance(func_payload, dict):
            live_exists = True
        elif not is_upstream_resource_unavailable(func_resp, func_payload):
            detail = extract_upstream_error_message(func_resp, func_payload) or "failed to load function"
            status_code = func_resp.status_code if func_resp is not None else 502
            return {
                "ok": False,
                "status": status_code,
                "message": detail,
                "live_exists": False,
            }
    except Exception as e:
        return {
            "ok": False,
            "status": 502,
            "message": f"failed to load function: {e}",
            "live_exists": False,
        }

    if live_exists:
        try:
            delete_resp = requests.delete(
                f"{apihostaddr}/object/function/{tenant}/{namespace}/{name}/",
                headers=gateway_headers(),
                timeout=60,
            )
        except requests.exceptions.RequestException as e:
            return {
                "ok": False,
                "status": 502,
                "message": f"Error connecting to backend server: {e}",
                "live_exists": True,
            }

        delete_payload = response_json_or_none(delete_resp)
        if not delete_resp.ok:
            return {
                "ok": False,
                "status": delete_resp.status_code,
                "message": extract_upstream_error_message(delete_resp, delete_payload) or "Delete failed",
                "live_exists": True,
            }

    try:
        delete_function_dashboard_artifacts(tenant, namespace, name)
    except LookupError as e:
        return {
            "ok": False,
            "status": 404,
            "message": str(e),
            "live_exists": live_exists,
        }
    except Exception as e:
        return {
            "ok": False,
            "status": 500,
            "message": str(e),
            "live_exists": live_exists,
        }

    return {
        "ok": True,
        "status": 200,
        "message": "",
        "live_exists": live_exists,
    }


def infer_catalog_modality_from_sample_query(sample_query):
    sample_query_obj = sample_query if isinstance(sample_query, dict) else {}
    api_type = normalize_catalog_api_type(sample_query_obj.get("apiType"))
    path = str(sample_query_obj.get("path") or "").strip().lower().lstrip("/")

    modality_map = {
        "text2text": "text",
        "image2text": "multimodal",
        "audio2text": "multimodal",
        "transcriptions": "multimodal",
        "text2img": "image",
        "text2audio": "audio",
    }

    if path == "v1/audio/transcriptions":
        return "multimodal"
    return modality_map.get(api_type, "text")


def build_catalog_default_spec_from_func(full_spec):
    spec = clone_json_value(full_spec if isinstance(full_spec, dict) else {})
    spec.pop("catalog_source", None)
    spec.pop("version", None)
    spec.pop("status", None)
    return spec


def build_catalog_prefill_from_func(hf_model: str, full_spec, func_name: str):
    catalog_spec = build_catalog_default_spec_from_func(full_spec)
    normalized_hf_model = str(hf_model or "").strip()
    provider = normalized_hf_model.split("/", 1)[0] if "/" in normalized_hf_model else ""
    display_name = normalized_hf_model.split("/")[-1] if normalized_hf_model != "" else ""
    return {
        "source_model_id": normalized_hf_model,
        "source_kind": "huggingface",
        "display_name": display_name,
        "provider": provider,
        "modality": infer_catalog_modality_from_sample_query(
            (full_spec.get("sample_query") or {}) if isinstance(full_spec, dict) else {}
        ),
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


def extract_gpu_count_from_spec(spec):
    if not isinstance(spec, dict):
        return None
    resources = spec.get("resources")
    if not isinstance(resources, dict):
        return None
    gpu = resources.get("GPU")
    if not isinstance(gpu, dict):
        return None
    gpu_count = gpu.get("Count")
    if not isinstance(gpu_count, int) or isinstance(gpu_count, bool) or gpu_count <= 0:
        return None
    return gpu_count


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
    source_kind = str(entry.get("source_kind") or "").strip().lower()
    source_model_id = str(entry.get("source_model_id") or "").strip()
    entry["source_external_url"] = ""
    if source_kind == "huggingface" and source_model_id != "":
        entry["source_external_url"] = f"https://huggingface.co/{quote(source_model_id, safe='/')}"
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


def user_can_access_dashboard_resource(roles, tenant_name: str, namespace_name: str = ""):
    normalized_tenant = str(tenant_name or "").strip()
    normalized_namespace = str(namespace_name or "").strip()
    if normalized_tenant == "":
        return False

    if has_inferx_admin_role(roles):
        return True

    for role in roles if isinstance(roles, list) else []:
        if not isinstance(role, dict):
            continue
        if not role_grants_dashboard_resource_access(role):
            continue

        role_tenant = str(role.get("tenant", "") or "").strip()
        role_namespace = str(role.get("namespace", "") or "").strip()
        obj_type = str(role.get("objType", "") or "").strip().lower()

        if role_tenant != normalized_tenant:
            continue
        if obj_type == "tenant":
            return True
        if obj_type == "namespace" and normalized_namespace != "" and role_namespace == normalized_namespace:
            return True

    return False


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


ENDPOINT_ENTRY_SELECT_COLUMNS = """
    SELECT
        slug,
        func_revision,
        brief_intro,
        detailed_intro,
        cs_ttft,
        recommended_use_cases,
        tags,
        provider,
        parameter_count_b,
        context_length,
        concurrency,
        last_published_at,
        last_published_by
    FROM Endpoints
"""


def ensure_endpoint_db_available():
    ensure_catalog_db_available()


def normalize_endpoint_row(row):
    if not isinstance(row, dict):
        raise ValueError("Invalid endpoint row")

    entry = dict(row)
    for key in ("tags", "recommended_use_cases"):
        entry[key] = normalize_catalog_json_field(entry.get(key))

    if not isinstance(entry.get("tags"), list):
        entry["tags"] = []
    if not isinstance(entry.get("recommended_use_cases"), list):
        entry["recommended_use_cases"] = []

    parameter_count = entry.get("parameter_count_b")
    if parameter_count is not None:
        try:
            entry["parameter_count_b"] = float(parameter_count)
        except Exception:
            entry["parameter_count_b"] = None

    concurrency = entry.get("concurrency")
    if concurrency is not None:
        try:
            entry["concurrency"] = float(concurrency)
        except Exception:
            entry["concurrency"] = None

    entry["last_published_at_display"] = format_catalog_datetime(entry.get("last_published_at"))
    entry["last_published_at"] = normalize_catalog_datetime_value(entry.get("last_published_at"))
    return entry


def list_endpoint_rows():
    ensure_endpoint_db_available()

    query = f"""
        {ENDPOINT_ENTRY_SELECT_COLUMNS}
        ORDER BY slug ASC
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query)
                rows = cursor.fetchall()
    except Exception as e:
        raise RuntimeError(f"failed to query endpoints: {e}")

    return [normalize_endpoint_row(dict(row)) for row in rows]


def query_endpoint_row_by_slug(slug: str):
    ensure_endpoint_db_available()
    normalized_slug = str(slug or "").strip()
    if normalized_slug == "":
        raise ValueError("endpoint slug is required")

    query = f"""
        {ENDPOINT_ENTRY_SELECT_COLUMNS}
        WHERE slug = %s
    """
    try:
        with psycopg2.connect(SECRDB_ADDR, connect_timeout=5) as conn:
            with conn.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute(query, (normalized_slug,))
                row = cursor.fetchone()
    except Exception as e:
        raise RuntimeError(f"failed to query endpoint `{normalized_slug}`: {e}")

    if row is None:
        raise LookupError(f"endpoint `{normalized_slug}` not found")

    return normalize_endpoint_row(dict(row))


def gateway_request_headers(*, json_body=False, active_tenant: str | None = None):
    access_token = session.get('access_token', '')
    headers = {}
    if access_token != "":
        headers['Authorization'] = f'Bearer {access_token}'
    if json_body:
        headers["Content-Type"] = "application/json"
    if active_tenant:
        headers["X-Tenant"] = str(active_tenant).strip()
    return headers


def list_gateway_objects(obj_type: str, tenant: str, namespace: str):
    url = f"{apihostaddr}/objects/{obj_type}/{tenant}/{namespace}/"
    resp = requests.get(url, headers=gateway_request_headers())
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(
            f"failed to list {obj_type} objects in {tenant}/{namespace}: "
            f"HTTP {resp.status_code}{': ' + detail if detail else ''}"
        )
    return payload if isinstance(payload, list) else []


def get_gateway_object(obj_type: str, tenant: str, namespace: str, name: str):
    url = f"{apihostaddr}/object/{obj_type}/{tenant}/{namespace}/{name}/"
    resp = requests.get(url, headers=gateway_request_headers())
    payload = response_json_or_none(resp)
    if is_upstream_resource_unavailable(resp, payload):
        raise LookupError(f"{obj_type} `{tenant}/{namespace}/{name}` not found")
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(
            f"failed to load {obj_type} `{tenant}/{namespace}/{name}`: "
            f"HTTP {resp.status_code}{': ' + detail if detail else ''}"
        )
    if not isinstance(payload, dict):
        raise RuntimeError(f"invalid {obj_type} response for `{tenant}/{namespace}/{name}`")
    return payload


def proxy_gateway_endpoint_admin_action(method: str, slug: str, *, metadata=None):
    normalized_slug = str(slug or "").strip()
    if normalized_slug == "":
        raise ValueError("endpoint slug is required")

    if method.upper() == "POST" and metadata is None:
        url = f"{apihostaddr}/admin/endpoints/{normalized_slug}/unpublish"
        resp = requests.post(url, headers=gateway_request_headers())
    else:
        suffix = "metadata" if method.upper() == "PUT" else "publish"
        url = f"{apihostaddr}/admin/endpoints/{normalized_slug}/{suffix}"
        resp = requests.request(
            method.upper(),
            url,
            headers=gateway_request_headers(json_body=True),
            json=metadata,
        )

    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body if body != "" else f"HTTP {resp.status_code}")

    try:
        return int(body)
    except Exception:
        return body


def list_admin_skill_templates():
    url = f"{apihostaddr}/admin/skilltemplates"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, list):
        raise RuntimeError("invalid skill template list response")
    return payload


def create_admin_skill_template(payload):
    url = f"{apihostaddr}/admin/skilltemplates"
    resp = requests.post(
        url,
        headers=gateway_request_headers(json_body=True),
        json=payload,
        timeout=60,
    )
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")

    parsed = response_json_or_none(resp)
    if not isinstance(parsed, dict):
        raise RuntimeError("invalid create skill template response")
    return parsed


def deactivate_admin_skill_template(template_id: int):
    url = f"{apihostaddr}/admin/skilltemplates/{int(template_id)}/deactivate"
    resp = requests.post(url, headers=gateway_request_headers(), timeout=60)
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return body


def activate_admin_skill_template(template_id: int):
    url = f"{apihostaddr}/admin/skilltemplates/{int(template_id)}/activate"
    resp = requests.post(url, headers=gateway_request_headers(), timeout=60)
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return body


def update_admin_skill_template(template_id: int, payload):
    url = f"{apihostaddr}/admin/skilltemplates/{int(template_id)}"
    resp = requests.put(
        url,
        headers=gateway_request_headers(json_body=True),
        json=payload,
        timeout=60,
    )
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    parsed = response_json_or_none(resp)
    if not isinstance(parsed, dict):
        raise RuntimeError("invalid update skill template response")
    return parsed


def delete_admin_skill_template(template_id: int):
    url = f"{apihostaddr}/admin/skilltemplates/{int(template_id)}"
    resp = requests.delete(url, headers=gateway_request_headers(), timeout=60)
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return body


def normalize_admin_skill_template_form(form):
    def required(field_name: str, label: str) -> str:
        value = str(form.get(field_name, "") or "").strip()
        if value == "":
            raise ValueError(f"{label} is required")
        return value

    return {
        "tenant": required("tenant", "Tenant"),
        "namespace": required("namespace", "Namespace"),
        "display_name": required("display_name", "Display name"),
        "description": str(form.get("description", "") or "").strip(),
        "func_tenant": required("func_tenant", "Function tenant"),
        "func_namespace": required("func_namespace", "Function namespace"),
        "normal_funcname": required("normal_funcname", "Function name"),
        "is_active": str(form.get("is_active", "true") or "true").strip().lower() not in ("0", "false", "off", "no"),
    }


def list_active_skill_templates():
    url = f"{apihostaddr}/skilltemplates"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, list):
        raise RuntimeError("invalid skill template response")
    return payload


def list_published_skills():
    url = f"{apihostaddr}/skills"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, list):
        raise RuntimeError("invalid skills list response")
    return payload


def list_marketplace_skills(*, page: int = 1, page_size: int = 24, keyword: str | None = None, active_tenant: str | None = None, include_unpublished: bool = False):
    url = f"{apihostaddr}/api/v1/skills/marketplace"
    params: dict = {
        "page": max(1, int(page)),
        "page_size": max(1, int(page_size)),
    }
    if keyword is not None and str(keyword).strip() != "":
        params["keyword"] = str(keyword).strip()
    if include_unpublished:
        params["include_unpublished"] = "true"

    resp = requests.get(url, headers=gateway_request_headers(active_tenant=active_tenant), params=params, timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise SkillSubscriptionGatewayError(detail or f"HTTP {resp.status_code}", resp.status_code)

    if isinstance(payload, list):
        return {
            "items": payload,
            "page": params["page"],
            "page_size": params["page_size"],
            "has_next": False,
            "keyword": params.get("keyword", ""),
        }

    if not isinstance(payload, dict):
        raise RuntimeError("invalid marketplace skills response")

    items = payload.get("items") or []
    if not isinstance(items, list):
        raise RuntimeError("invalid marketplace skills response")

    return {
        "items": items,
        "page": int(payload.get("page") or params["page"]),
        "page_size": int(payload.get("page_size") or params["page_size"]),
        "has_next": bool(payload.get("has_next")),
        "keyword": str(payload.get("keyword") or params.get("keyword", "") or "").strip(),
    }


def list_tenant_skills(owner_tenant: str):
    url = f"{apihostaddr}/skills/{owner_tenant}"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, list):
        raise RuntimeError("invalid tenant skills response")
    return payload


def list_namespace_skills(owner_tenant: str, namespace: str):
    url = f"{apihostaddr}/skills/{owner_tenant}/{namespace}"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, list):
        raise RuntimeError("invalid namespace skills response")
    return payload


class SkillSubscriptionGatewayError(RuntimeError):
    def __init__(self, message: str, status_code: int = 502):
        super().__init__(message)
        self.status_code = int(status_code)


def list_skill_subscriptions(*, active_tenant: str | None = None):
    url = f"{apihostaddr}/api/v1/skills/subscriptions"
    resp = requests.get(url, headers=gateway_request_headers(active_tenant=active_tenant), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise SkillSubscriptionGatewayError(detail or f"HTTP {resp.status_code}", resp.status_code)
    if not isinstance(payload, list):
        raise RuntimeError("invalid skill subscriptions response")

    rows = []
    for row in payload:
        if not isinstance(row, dict):
            continue
        item = dict(row)
        item["subscribed_at_display"] = format_catalog_datetime(item.get("subscribed_at"))
        rows.append(item)
    return rows


def create_skill_subscription(owner_tenant: str, namespace: str, skillname: str, tool_alias: str | None = None, *, active_tenant: str | None = None):
    payload = {
        "owner_tenant": owner_tenant,
        "owner_namespace": namespace,
        "skillname": skillname,
    }
    if tool_alias is not None and str(tool_alias).strip() != "":
        payload["tool_alias"] = str(tool_alias).strip()

    url = f"{apihostaddr}/api/v1/skills/subscriptions"
    resp = requests.post(
        url,
        headers=gateway_request_headers(json_body=True, active_tenant=active_tenant),
        json=payload,
        timeout=60,
    )
    response_payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, response_payload)
        raise SkillSubscriptionGatewayError(detail or f"HTTP {resp.status_code}", resp.status_code)
    if not isinstance(response_payload, dict):
        raise RuntimeError("invalid create skill subscription response")
    return response_payload


def update_skill_subscription_alias(owner_tenant: str, namespace: str, skillname: str, tool_alias: str, *, active_tenant: str | None = None):
    encoded_owner = quote(owner_tenant, safe="")
    encoded_namespace = quote(namespace, safe="")
    encoded_skill = quote(skillname, safe="")
    url = f"{apihostaddr}/api/v1/skills/subscriptions/{encoded_owner}/{encoded_namespace}/{encoded_skill}"
    resp = requests.patch(
        url,
        headers=gateway_request_headers(json_body=True, active_tenant=active_tenant),
        json={"tool_alias": str(tool_alias or "").strip()},
        timeout=60,
    )
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise SkillSubscriptionGatewayError(detail or f"HTTP {resp.status_code}", resp.status_code)
    if not isinstance(payload, dict):
        raise RuntimeError("invalid update skill subscription response")
    return payload


def delete_skill_subscription(owner_tenant: str, namespace: str, skillname: str, *, active_tenant: str | None = None):
    encoded_owner = quote(owner_tenant, safe="")
    encoded_namespace = quote(namespace, safe="")
    encoded_skill = quote(skillname, safe="")
    url = f"{apihostaddr}/api/v1/skills/subscriptions/{encoded_owner}/{encoded_namespace}/{encoded_skill}"
    resp = requests.delete(url, headers=gateway_request_headers(active_tenant=active_tenant), timeout=60)
    payload = response_json_or_none(resp)
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise SkillSubscriptionGatewayError(detail or f"HTTP {resp.status_code}", resp.status_code)
    return True


def get_admin_namespace_names_for_tenant(roles, tenant_name: str):
    normalized_tenant = str(tenant_name or "").strip().lower()
    if normalized_tenant == "":
        return []

    namespaces = []
    seen = set()
    for role in roles if isinstance(roles, list) else []:
        if not isinstance(role, dict):
            continue
        if str(role.get("objType", "") or "").strip().lower() != "namespace":
            continue
        if str(role.get("role", "") or "").strip().lower() != "admin":
            continue
        if str(role.get("tenant", "") or "").strip().lower() != normalized_tenant:
            continue
        namespace_name = str(role.get("namespace", "") or "").strip()
        namespace_key = namespace_name.lower()
        if namespace_name == "" or namespace_key in seen:
            continue
        seen.add(namespace_key)
        namespaces.append(namespace_name)

    return sorted(namespaces, key=lambda item: item.lower())


def load_skill_dashboard_context(*, skills_view: str = "my_skills"):
    def parse_positive_int_arg(name: str, default: int, *, minimum: int = 1, maximum: int | None = None) -> int:
        raw = str(request.args.get(name, default) or "").strip()
        try:
            value = int(raw)
        except Exception:
            value = default
        if value < minimum:
            value = minimum
        if maximum is not None and value > maximum:
            value = maximum
        return value

    roles = listroles()
    active_tenant = resolve_active_tenant_name(roles)
    if active_tenant == "":
        raise RuntimeError("No active tenant selected")

    is_inferx_admin = is_inferx_admin_user()
    can_manage_subscriptions = has_admin_role_for_model(roles, active_tenant) or is_inferx_admin

    my_skills = list_tenant_skills(active_tenant)
    if not (is_inferx_admin or has_admin_role_for_model(roles, active_tenant)):
        seen_keys = {
            (
                str(row.get("owner_tenant", "") or "").strip().lower(),
                str(row.get("owner_namespace", "") or "").strip().lower(),
                str(row.get("skillname", "") or "").strip().lower(),
            )
            for row in my_skills
            if isinstance(row, dict)
        }
        for namespace in get_admin_namespace_names_for_tenant(roles, active_tenant):
            for row in list_namespace_skills(active_tenant, namespace):
                key = (
                    str(row.get("owner_tenant", "") or "").strip().lower(),
                    str(row.get("owner_namespace", "") or "").strip().lower(),
                    str(row.get("skillname", "") or "").strip().lower(),
                )
                if any(part == "" for part in key) or key in seen_keys:
                    continue
                seen_keys.add(key)
                my_skills.append(row)

    subscriptions = list_skill_subscriptions(active_tenant=active_tenant)
    subscriptions_by_skill = {}
    for row in subscriptions:
        key = (
            str(row.get("owner_tenant", "") or "").strip().lower(),
            str(row.get("owner_namespace", "") or "").strip().lower(),
            str(row.get("skillname", "") or "").strip().lower(),
        )
        if any(part == "" for part in key):
            continue
        subscriptions_by_skill[key] = dict(row)

    for item in my_skills:
        key = (
            str(item.get("owner_tenant", "") or "").strip().lower(),
            str(item.get("owner_namespace", "") or "").strip().lower(),
            str(item.get("skillname", "") or "").strip().lower(),
        )
        subscription = subscriptions_by_skill.get(key)
        item["is_subscribed"] = bool(subscription)
        item["tool_alias"] = subscription.get("tool_alias") if subscription else None

    marketplace_page = 1
    marketplace_page_size = 24
    marketplace_keyword = ""
    marketplace_has_next = False
    marketplace_include_unpublished = False
    marketplace_skills = []
    onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(active_tenant)
    mcp_server_url = f"{normalize_public_api_base_url()}/mcp"
    normalized_onboarding_apikey = str(onboarding_apikey or "").strip()
    mcp_api_key_copyable = normalized_onboarding_apikey != ""
    if normalized_onboarding_apikey != "":
        mcp_api_key_value = normalized_onboarding_apikey
        mcp_api_key_display = mask_apikey_for_ui(normalized_onboarding_apikey)
    else:
        mcp_api_key_value = build_inference_apikey_placeholder()
        mcp_api_key_display = mcp_api_key_value

    if skills_view == "marketplace":
        marketplace_page = parse_positive_int_arg("page", 1)
        marketplace_page_size = parse_positive_int_arg("page_size", 24, maximum=200)
        marketplace_keyword = str(request.args.get("keyword", "") or "").strip()
        marketplace_include_unpublished = is_inferx_admin and str(request.args.get("include_unpublished", "") or "").strip().lower() in ("1", "true", "yes", "on")

        marketplace_payload = list_marketplace_skills(
            page=marketplace_page,
            page_size=marketplace_page_size,
            keyword=marketplace_keyword or None,
            active_tenant=active_tenant,
            include_unpublished=marketplace_include_unpublished,
        )
        marketplace_page = int(marketplace_payload.get("page") or marketplace_page)
        marketplace_page_size = int(marketplace_payload.get("page_size") or marketplace_page_size)
        marketplace_has_next = bool(marketplace_payload.get("has_next"))

        my_skill_ids = {int(r["skill_id"]) for r in my_skills if r.get("skill_id") is not None}
        for row in marketplace_payload.get("items", []):
            skill_id = row.get("skill_id")
            if skill_id is not None and int(skill_id) in my_skill_ids:
                continue
            item = dict(row)
            key = (
                str(item.get("owner_tenant", "") or "").strip().lower(),
                str(item.get("owner_namespace", "") or "").strip().lower(),
                str(item.get("skillname", "") or "").strip().lower(),
            )
            if "is_subscribed" not in item:
                subscription = subscriptions_by_skill.get(key)
                item["is_subscribed"] = bool(subscription)
                item["tool_alias"] = subscription.get("tool_alias") if subscription else None
            marketplace_skills.append(item)

    return {
        "active_tenant": active_tenant,
        "can_manage_subscriptions": can_manage_subscriptions,
        "is_inferx_admin": is_inferx_admin,
        "my_skills": my_skills,
        "marketplace_skills": marketplace_skills,
        "marketplace_page": marketplace_page,
        "marketplace_page_size": marketplace_page_size,
        "marketplace_keyword": marketplace_keyword,
        "marketplace_has_next": marketplace_has_next,
        "marketplace_include_unpublished": marketplace_include_unpublished,
        "subscriptions": subscriptions,
        "mcp_server_url": mcp_server_url,
        "mcp_api_key_value": mcp_api_key_value,
        "mcp_api_key_display": mcp_api_key_display,
        "mcp_api_key_copyable": mcp_api_key_copyable,
        "mcp_api_key_name": str(onboarding_apikey_name or "").strip(),
    }


def create_skill(owner_tenant: str, namespace: str, skillname: str, payload):
    url = f"{apihostaddr}/skills/{owner_tenant}/{namespace}/{skillname}"
    resp = requests.post(
        url,
        headers=gateway_request_headers(json_body=True),
        json=payload,
        timeout=60,
    )
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return response_json_or_none(resp)


def post_skill_state_action(owner_tenant: str, namespace: str, skillname: str, action: str):
    url = f"{apihostaddr}/skills/{owner_tenant}/{namespace}/{skillname}/{action}"
    resp = requests.post(url, headers=gateway_request_headers(), timeout=60)
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return body


def delete_skill(owner_tenant: str, namespace: str, skillname: str):
    url = f"{apihostaddr}/skills/{owner_tenant}/{namespace}/{skillname}"
    resp = requests.delete(url, headers=gateway_request_headers(), timeout=60)
    body = (resp.text or "").strip()
    if not resp.ok:
        raise RuntimeError(body or f"HTTP {resp.status_code}")
    return body


def build_skill_form_context(*, roles=None):
    if roles is None:
        roles = listroles()
    deploy_target = build_catalog_deploy_target_selector_context(roles=roles)
    deploy_target = dict(deploy_target)
    deploy_target["resolved_summary_label"] = "Skill path:"
    deploy_target["unresolved_summary_label"] = "Skill path not selected:"
    deploy_target["action_label"] = "Create Skill"
    deploy_target["preferred_namespace_value"] = "default"
    templates = list_active_skill_templates()
    return {
        "deploy_target": deploy_target,
        "skill_templates": templates,
    }


def endpoint_metadata_payload_from_prefill(data):
    entry = data if isinstance(data, dict) else {}
    context_length_raw = str(entry.get("context_length", "") or "").strip()
    context_length = None if context_length_raw == "" else int(context_length_raw)
    return {
        "brief_intro": str(entry.get("brief_intro", "") or "").strip(),
        "detailed_intro": str(entry.get("detailed_intro", "") or "").strip(),
        "cs_ttft": normalize_catalog_string_field(
            entry.get("cs_ttft"),
            "cs_ttft",
            allow_empty=True,
        ),
        "recommended_use_cases": normalize_catalog_string_list(
            entry.get("recommended_use_cases"),
            "recommended_use_cases",
        ),
        "tags": normalize_catalog_string_list(entry.get("tags"), "tags"),
        "provider": normalize_catalog_string_field(
            entry.get("provider"),
            "provider",
            allow_empty=True,
        ),
        "parameter_count_b": normalize_catalog_parameter_count(entry.get("parameter_count_b")),
        "context_length": context_length,
        "concurrency": normalize_catalog_parameter_count(entry.get("concurrency")),
    }


def resolve_endpoint_model_name(sample_query, spec=None, fallback_slug: str = "") -> str:
    model_name = extract_sample_query_model_target(sample_query)
    if model_name == "":
        try:
            model_name = resolve_effective_model_target_from_spec(
                spec,
                commands_label="endpoint function commands",
                sample_query_label="endpoint function sample_query.body.model",
            )
        except Exception:
            model_name = ""
    if model_name == "":
        model_name = str(fallback_slug or "").strip()
    return model_name


def build_endpoint_client_setup_preview(tenant: str, slug: str, model_name: str, provider: str = ""):
    base_url = normalize_public_api_base_url()
    normalized_model_name = str(model_name or "").strip()
    if normalized_model_name == "":
        normalized_slug = str(slug or "").strip()
        normalized_provider = str(provider or "").strip().strip("/")
        normalized_model_name = f"{normalized_provider}/{normalized_slug}" if normalized_provider and normalized_slug else normalized_slug
    return {
        "api_base_url": f"{base_url}/funccall/{tenant}/endpoints/{slug}/v1",
        "api_base_url_display": f"{base_url}/funccall/.../v1",
        "auth_required": True,
        "model_name": normalized_model_name,
        "api_key": "<INFERENCE_API_KEY>",
        "api_key_display": "<INFERENCE_API_KEY>",
        "api_key_copyable": False,
        "api_key_name": "",
    }


def build_endpoint_runtime_context(spec, sample_query, *, fallback_slug: str = ""):
    spec_obj = spec if isinstance(spec, dict) else {}
    sample_query_obj = sample_query if isinstance(sample_query, dict) else {}
    body = sample_query_obj.get("body")
    if not isinstance(body, dict):
        body = {}

    return {
        "spec": spec_obj,
        "sample_query": sample_query_obj,
        "api_type": str(sample_query_obj.get("apiType", "") or "").strip(),
        "path": str(sample_query_obj.get("path", "") or "").strip(),
        "prompt": str(sample_query_obj.get("prompt", "") or "").strip(),
        "map": clone_json_value(body),
        "model_name": resolve_endpoint_model_name(sample_query_obj, spec_obj, fallback_slug=fallback_slug),
        "enabled": isinstance(sample_query_obj, dict) and bool(sample_query_obj) and isinstance(body, dict) and bool(body),
    }


def resolve_endpoint_catalog_entry(slug: str, platform_func=None):
    platform_spec = (((platform_func or {}).get("func") or {}).get("object") or {}).get("spec")
    catalog_entry = query_catalog_entry_by_source(
        platform_spec.get("catalog_source") if isinstance(platform_spec, dict) else None
    )
    if isinstance(catalog_entry, dict):
        return catalog_entry

    normalized_slug = str(slug or "").strip()
    if normalized_slug == "":
        return None

    try:
        return query_catalog_entry_by_slug(normalized_slug, active_only=False)
    except Exception:
        return None


def fill_missing_endpoint_metadata_from_source(payload, source):
    merged = clone_json_value(payload) if isinstance(payload, dict) else {}
    if not isinstance(source, dict):
        return merged

    for key in (
        "brief_intro",
        "detailed_intro",
        "cs_ttft",
        "recommended_use_cases",
        "tags",
        "provider",
        "parameter_count_b",
        "context_length",
        "concurrency",
    ):
        current_value = merged.get(key)
        source_value = source.get(key)

        if key in ("recommended_use_cases", "tags"):
            if isinstance(current_value, list) and current_value:
                continue
            if isinstance(source_value, list):
                next_value = [str(item).strip() for item in source_value if str(item).strip() != ""]
                if next_value:
                    merged[key] = next_value
            continue

        if current_value not in ("", None):
            continue
        if source_value is None:
            continue
        if isinstance(source_value, str) and source_value.strip() == "":
            continue
        merged[key] = source_value

    return merged


def build_endpoint_editor_prefill(slug: str, existing_entry, platform_func):
    prefill = {
        "slug": slug,
        "brief_intro": "",
        "detailed_intro": "",
        "cs_ttft": "",
        "recommended_use_cases": [],
        "tags": [],
        "provider": "",
        "parameter_count_b": "",
        "context_length": "",
        "concurrency": "",
    }

    catalog_entry = resolve_endpoint_catalog_entry(slug, platform_func)

    platform_spec = (((platform_func or {}).get("func") or {}).get("object") or {}).get("spec")
    platform_commands = platform_spec.get("commands") if isinstance(platform_spec, dict) else []

    for source in (existing_entry, catalog_entry):
        if not isinstance(source, dict):
            continue
        for key in (
            "brief_intro",
            "detailed_intro",
            "cs_ttft",
            "recommended_use_cases",
            "tags",
            "provider",
            "parameter_count_b",
            "context_length",
            "concurrency",
        ):
            value = source.get(key)
            if key in ("recommended_use_cases", "tags"):
                if prefill[key]:
                    continue
                if isinstance(value, list) and value:
                    prefill[key] = [str(item).strip() for item in value if str(item).strip() != ""]
                continue
            if prefill[key] not in ("", None, []):
                continue
            if value is None:
                continue
            if isinstance(value, str) and value.strip() == "":
                continue
            prefill[key] = value

    if prefill["context_length"] in ("", None):
        max_model_len = parse_named_command_arg(platform_commands, "--max-model-len")
        if max_model_len != "":
            try:
                prefill["context_length"] = int(max_model_len)
            except Exception:
                pass

    if prefill["parameter_count_b"] is None:
        prefill["parameter_count_b"] = ""
    if prefill["concurrency"] is None:
        prefill["concurrency"] = ""

    return prefill, catalog_entry


def endpoint_state_from_sources(*, published: bool, metadata_entry, platform_detail):
    platform_func = (platform_detail or {}).get("func") if isinstance(platform_detail, dict) else {}
    func_state = ((((platform_func or {}).get("object") or {}).get("status") or {}).get("state") or "")
    model_status = infer_model_status((platform_detail or {}).get("pods"), func_state, [])
    has_metadata = isinstance(metadata_entry, dict)
    has_publish_history = bool((metadata_entry or {}).get("last_published_at"))

    if published:
        return {"code": "published", "label": "Published"}
    if model_status["code"] in ("ready", "standby"):
        return {"code": "ready", "label": "Ready"}
    if has_metadata or has_publish_history:
        return {"code": "unpublished", "label": "Unpublished"}
    return {"code": "deploying", "label": "Deploying"}


def accessible_endpoint_tenant_names(roles):
    if has_inferx_admin_role(roles):
        tenant_names = []
        for tenant_obj in listtenants():
            if not isinstance(tenant_obj, dict):
                continue
            tenant_name = str(tenant_obj.get("name", "") or "").strip()
            if tenant_name == "" or tenant_name.lower() == "system":
                continue
            tenant_names.append(tenant_name)
        return sorted(set(tenant_names), key=lambda item: item.lower())

    tenant_names = get_accessible_tenant_names_from_roles_for_create(roles)
    if not tenant_names:
        tenant_names = [
            str(item).strip()
            for item in session.get("tenant_names", [])
            if str(item).strip() != ""
        ]
    active_tenant = str(session.get("active_tenant_name", "") or "").strip()
    if active_tenant != "" and active_tenant not in tenant_names:
        tenant_names.append(active_tenant)
    return sorted(set(tenant_names), key=lambda item: item.lower())


def resolve_active_tenant_name(roles=None) -> str:
    active_tenant = str(session.get("active_tenant_name", session.get("tenant_name", "")) or "").strip()
    if active_tenant != "":
        return active_tenant

    if roles is None:
        roles = listroles()

    tenant_names = accessible_endpoint_tenant_names(roles)
    if not tenant_names:
        return ""

    active_tenant = tenant_names[0]
    session["active_tenant_name"] = active_tenant
    return active_tenant


def enrich_endpoint_catalog_fallback(entry):
    endpoint = clone_json_value(entry) if isinstance(entry, dict) else {}
    slug = str(endpoint.get("slug", "") or "").strip()
    try:
        catalog_entry = query_catalog_entry_by_slug(slug, active_only=False)
    except Exception:
        catalog_entry = None

    endpoint["catalog_entry"] = catalog_entry
    endpoint["spec"] = (
        (catalog_entry or {}).get("default_func_spec")
        if isinstance(catalog_entry, dict)
        else None
    )
    endpoint["sample_query"] = (
        ((endpoint.get("spec") or {}).get("sample_query"))
        if isinstance(endpoint.get("spec"), dict)
        else None
    )
    endpoint["model_name"] = resolve_endpoint_model_name(
        endpoint.get("sample_query"),
        endpoint.get("spec"),
        fallback_slug=slug,
    )
    return endpoint


def load_live_endpoint_detail(
    slug: str,
    *,
    allow_missing_live_spec: bool = False,
    allow_missing_status: bool = False,
):
    normalized_slug = str(slug or "").strip()
    if normalized_slug == "":
        raise ValueError("endpoint slug is required")

    metadata_entry = None
    try:
        metadata_entry = query_endpoint_row_by_slug(normalized_slug)
    except LookupError:
        metadata_entry = None

    entry = enrich_endpoint_catalog_fallback(metadata_entry if metadata_entry is not None else {"slug": normalized_slug})
    published = False
    status_obj = None
    try:
        status_obj = get_gateway_object("funcstatus", "inferx", "endpoint", normalized_slug)
        published = bool((((status_obj or {}).get("object") or {}).get("published")))
    except LookupError:
        raise
    except Exception as e:
        if not allow_missing_status or metadata_entry is None:
            raise
        if "No permission" not in str(e) and "permission" not in str(e).lower():
            raise
        # Tenant endpoint pages may not be allowed to read shared funcstatus directly.
        # Fall back to metadata-backed visibility so the page can still render.
        published = True

    entry["published"] = published

    platform_detail = None
    resp, platform_detail_payload = getfunc_response("inferx", "endpoint", normalized_slug)
    if is_upstream_resource_unavailable(resp, platform_detail_payload):
        raise LookupError(f"endpoint `{normalized_slug}` not found")
    if resp.ok:
        if not isinstance(platform_detail_payload, dict):
            raise RuntimeError(f"invalid function response for endpoint `{normalized_slug}`")
        platform_detail = platform_detail_payload
    elif not (allow_missing_live_spec and is_upstream_permission_denied(resp, platform_detail_payload)):
        detail = extract_upstream_error_message(resp, platform_detail_payload)
        raise RuntimeError(
            f"failed to load endpoint `{normalized_slug}`: "
            f"HTTP {resp.status_code}{': ' + detail if detail else ''}"
        )

    entry["platform_detail"] = platform_detail

    platform_spec = (((platform_detail or {}).get("func") or {}).get("object") or {}).get("spec")
    if isinstance(platform_spec, dict):
        entry["spec"] = clone_json_value(platform_spec)
        platform_sample_query = platform_spec.get("sample_query")
        if isinstance(platform_sample_query, dict):
            entry["sample_query"] = clone_json_value(platform_sample_query)
    entry["gpu_count"] = extract_gpu_count_from_spec(entry.get("spec"))

    entry["model_name"] = resolve_endpoint_model_name(
        entry.get("sample_query"),
        entry.get("spec"),
        fallback_slug=normalized_slug,
    )
    endpoint_state = endpoint_state_from_sources(
        published=published,
        metadata_entry=metadata_entry,
        platform_detail=platform_detail,
    )

    return {
        "entry": entry,
        "metadata_entry": metadata_entry,
        "platform_detail": platform_detail,
        "published": published,
        "endpoint_state": endpoint_state,
    }


def load_tenant_endpoint_detail(slug: str, tenant: str):
    normalized_slug = str(slug or "").strip()
    normalized_tenant = str(tenant or "").strip()
    if normalized_slug == "":
        raise ValueError("endpoint slug is required")
    if normalized_tenant == "":
        raise ValueError("tenant is required")

    metadata_entry = None
    try:
        metadata_entry = query_endpoint_row_by_slug(normalized_slug)
    except LookupError:
        metadata_entry = None

    entry = enrich_endpoint_catalog_fallback(metadata_entry if metadata_entry is not None else {"slug": normalized_slug})

    resp, platform_detail_payload = get_published_endpoint_response(normalized_tenant, normalized_slug)
    if is_upstream_resource_unavailable(resp, platform_detail_payload):
        raise LookupError(f"endpoint `{normalized_slug}` not found")
    if is_upstream_permission_denied(resp, platform_detail_payload):
        raise PermissionError(f"endpoint `{normalized_slug}` unavailable")
    if not resp.ok:
        detail = extract_upstream_error_message(resp, platform_detail_payload)
        raise RuntimeError(
            f"failed to load endpoint `{normalized_slug}`: "
            f"HTTP {resp.status_code}{': ' + detail if detail else ''}"
        )
    if not isinstance(platform_detail_payload, dict):
        raise RuntimeError(f"invalid published endpoint response for `{normalized_slug}`")

    platform_detail = platform_detail_payload
    entry["platform_detail"] = platform_detail
    entry["published"] = True

    platform_spec = (((platform_detail or {}).get("func") or {}).get("object") or {}).get("spec")
    if isinstance(platform_spec, dict):
        entry["spec"] = clone_json_value(platform_spec)
        platform_sample_query = platform_spec.get("sample_query")
        if isinstance(platform_sample_query, dict):
            entry["sample_query"] = clone_json_value(platform_sample_query)
    entry["gpu_count"] = extract_gpu_count_from_spec(entry.get("spec"))

    entry["model_name"] = resolve_endpoint_model_name(
        entry.get("sample_query"),
        entry.get("spec"),
        fallback_slug=normalized_slug,
    )

    return {
        "entry": entry,
        "metadata_entry": metadata_entry,
        "platform_detail": platform_detail,
        "published": True,
        "endpoint_state": {"code": "published", "label": "Published"},
    }


def build_endpoint_list_entries(*, include_unpublished: bool, tenant: str = ""):
    metadata_rows = {row["slug"]: row for row in list_endpoint_rows()}
    status_rows = list_gateway_objects("funcstatus", "inferx", "endpoint")
    platform_status = {}
    for status in status_rows:
        name = str(status.get("name", "") or "").strip()
        if name != "":
            platform_status[name] = status

    slugs = sorted(set(metadata_rows.keys()) | set(platform_status.keys()))
    entries = []
    for slug in slugs:
        published = bool((((platform_status.get(slug) or {}).get("object") or {}).get("published")))
        if not include_unpublished and not published:
            continue
        entry = metadata_rows.get(slug)
        if entry is None:
            entry = {"slug": slug}
        entry = enrich_endpoint_catalog_fallback(entry)
        entry["published"] = published
        platform_detail = None
        normalized_tenant = str(tenant or "").strip()
        if normalized_tenant != "":
            try:
                resp, platform_detail_payload = get_published_endpoint_response(normalized_tenant, slug)
                if resp.ok and isinstance(platform_detail_payload, dict):
                    platform_detail = platform_detail_payload
            except Exception:
                platform_detail = None

        if platform_detail is None:
            try:
                platform_detail = getfunc("inferx", "endpoint", slug)
            except Exception:
                platform_detail = None

        if isinstance(platform_detail, dict):
            entry["platform_detail"] = platform_detail
            platform_spec = (((platform_detail or {}).get("func") or {}).get("object") or {}).get("spec")
            if isinstance(platform_spec, dict):
                entry["spec"] = clone_json_value(platform_spec)
                platform_sample_query = platform_spec.get("sample_query")
                if isinstance(platform_sample_query, dict):
                    entry["sample_query"] = clone_json_value(platform_sample_query)
        entry["gpu_count"] = extract_gpu_count_from_spec(entry.get("spec"))

        entry["model_name"] = resolve_endpoint_model_name(
            entry.get("sample_query"),
            entry.get("spec"),
            fallback_slug=slug,
        )
        entries.append(entry)

    return entries


def build_admin_endpoint_list_entries():
    metadata_rows = {row["slug"]: row for row in list_endpoint_rows()}
    platform_funcs = listfuncs("inferx", "endpoint")
    status_rows = list_gateway_objects("funcstatus", "inferx", "endpoint")
    platform_status = {}
    for status in status_rows:
        name = str(status.get("name", "") or "").strip()
        if name != "":
            platform_status[name] = status

    entries = []
    for func_brief in platform_funcs:
        slug = str(((func_brief or {}).get("func") or {}).get("name") or "").strip()
        if slug == "":
            continue
        platform_detail = getfunc("inferx", "endpoint", slug)
        metadata_entry = metadata_rows.get(slug)
        published = bool((((platform_status.get(slug) or {}).get("object") or {}).get("published")))
        entry = enrich_endpoint_catalog_fallback(metadata_entry if metadata_entry is not None else {"slug": slug})
        entry["platform_detail"] = platform_detail
        entry["published"] = published
        platform_spec = (((platform_detail or {}).get("func") or {}).get("object") or {}).get("spec")
        if isinstance(platform_spec, dict):
            entry["spec"] = clone_json_value(platform_spec)
        entry["gpu_count"] = extract_gpu_count_from_spec(entry.get("spec"))
        entry["state"] = endpoint_state_from_sources(
            published=published,
            metadata_entry=metadata_entry,
            platform_detail=platform_detail,
        )
        entries.append(entry)

    return entries


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
    commands = normalize_json_valued_command_args(
        [str(item) for item in commands],
        label="catalog `default_func_spec.commands`",
    )
    validate_catalog_command_tokens(commands)
    hf_model = resolve_effective_model_target_from_spec(
        normalized_spec,
        commands_label="catalog `default_func_spec.commands`",
        sample_query_label="catalog `default_func_spec.sample_query.body.model`",
    ).strip()
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
    if api_type == "":
        raise ValueError("catalog `default_func_spec.sample_query.apiType` must be a non-empty string")
    if not isinstance(sample_query.get("path"), str) or str(sample_query.get("path")).strip() == "":
        raise ValueError("catalog `default_func_spec.sample_query.path` must be a non-empty string")
    if not isinstance(sample_query.get("prompt"), str) or str(sample_query.get("prompt")).strip() == "":
        raise ValueError("catalog `default_func_spec.sample_query.prompt` must be a non-empty string")
    body = sample_query.get("body")
    if not isinstance(body, dict) or len(body) == 0:
        raise ValueError("catalog `default_func_spec.sample_query.body` must be a non-empty object")
    if "model" in body:
        sample_query_model = body.get("model")
        if not isinstance(sample_query_model, str) or normalize_effective_model_target(sample_query_model) != hf_model:
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


def build_inference_apikey_placeholder() -> str:
    return "<INFERENCE_API_KEY(Find or create one on Admin|Apikeys page)>"


def mask_apikey_for_ui(raw_key: str) -> str:
    normalized = str(raw_key or "")
    if normalized == "":
        return ""
    if len(normalized) <= 8:
        return "********"
    return f"{normalized[:3]}{'*' * max(0, len(normalized) - 7)}{normalized[-4:]}"


def build_onboard_inference_apikey_name_for_ui(tenant_name: str, sub: str) -> str:
    normalized_tenant = str(tenant_name or "").strip().lower()
    normalized_sub = str(sub or "").strip().lower()
    if normalized_tenant == "" or normalized_sub == "":
        return ""

    tenant_slug_parts = []
    previous_was_dash = False
    for char in normalized_tenant:
        if char.isalnum():
            tenant_slug_parts.append(char)
            previous_was_dash = False
            continue
        if not previous_was_dash:
            tenant_slug_parts.append("-")
            previous_was_dash = True

    tenant_slug = "".join(tenant_slug_parts).strip("-")
    if tenant_slug == "":
        tenant_slug = "tenant"

    sub_slug = "".join(char for char in normalized_sub if char.isalnum())
    if sub_slug == "":
        sub_suffix = "user"
    else:
        sub_suffix = sub_slug[:8]

    return f"quickstart-inference-{tenant_slug}-{sub_suffix}"


def resolve_onboarding_inference_apikey_for_ui(tenant_name: str):
    normalized_tenant = str(tenant_name or "").strip()
    if normalized_tenant.lower() == "public":
        return "", ""

    onboarding_apikey = str(session.get("onboarding_inference_apikey", "") or "").strip()
    cached_apikey_name = str(session.get("onboarding_inference_apikey_name", "") or "").strip()
    onboarding_apikey_name = build_onboard_inference_apikey_name_for_ui(
        normalized_tenant,
        session.get("sub", ""),
    ) or cached_apikey_name

    if onboarding_apikey != "" and onboarding_apikey_name != "" and cached_apikey_name == onboarding_apikey_name:
        return onboarding_apikey, onboarding_apikey_name

    if onboarding_apikey_name == "":
        return "", ""

    try:
        apikeys = getapikeys()
    except Exception:
        return "", onboarding_apikey_name

    if not isinstance(apikeys, list):
        return "", onboarding_apikey_name

    for apikey in apikeys:
        if not isinstance(apikey, dict):
            continue
        if str(apikey.get("keyname", "") or "").strip() != onboarding_apikey_name:
            continue

        restrict_tenant = str(apikey.get("restrict_tenant", "") or "").strip()
        if restrict_tenant != "" and restrict_tenant != normalized_tenant:
            continue

        raw_apikey = str(apikey.get("apikey", "") or "").strip()
        if raw_apikey == "":
            continue

        session["onboarding_inference_apikey"] = raw_apikey
        session["onboarding_inference_apikey_name"] = onboarding_apikey_name
        return raw_apikey, onboarding_apikey_name

    return "", onboarding_apikey_name


def build_client_setup_for_ui(tenant: str, namespace: str, funcname: str, sample_query, apikey: str, apikey_name: str, spec=None):
    if not isinstance(sample_query, dict):
        return {}

    path = str(sample_query.get("path", "v1/completions") or "v1/completions").strip()
    path = path.lstrip("/")
    body = sample_query.get("body", {})
    if not isinstance(body, dict):
        body = {}

    base_url = normalize_public_api_base_url()
    api_base_url = ""
    if path == "v1" or path.startswith("v1/"):
        api_base_url = f"{base_url}/funccall/{tenant}/{namespace}/{funcname}/v1"

    model_name = extract_sample_query_model_target(sample_query)
    if model_name == "":
        try:
            model_name = resolve_effective_model_target_from_spec(
                spec,
                commands_label="deployed function commands",
                sample_query_label="deployed function sample_query.body.model",
            )
        except ValueError:
            model_name = ""

    tenant_name = str(tenant or "").strip().lower()
    auth_required = tenant_name != "public"
    normalized_apikey = str(apikey or "").strip()
    normalized_apikey_name = str(apikey_name or "").strip()
    api_key_copyable = False

    if not auth_required:
        api_key_value = "(not required for public models)"
        api_key_display = api_key_value
        normalized_apikey_name = ""
    elif normalized_apikey != "":
        api_key_value = normalized_apikey
        api_key_display = mask_apikey_for_ui(normalized_apikey)
        api_key_copyable = True
    else:
        api_key_value = build_inference_apikey_placeholder()
        api_key_display = api_key_value

    return {
        "api_base_url": api_base_url,
        "auth_required": auth_required,
        "model_name": model_name,
        "api_key": api_key_value,
        "api_key_display": api_key_display,
        "api_key_copyable": api_key_copyable,
        "api_key_name": normalized_apikey_name,
        "path": path,
    }


def load_opencode_config_template_text():
    try:
        return OPEN_CODE_CONFIG_TEMPLATE_PATH.read_text(encoding="utf-8")
    except OSError:
        return ""


def parse_positive_int_or_none(value):
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value if value > 0 else None
    raw = str(value or "").strip()
    if raw == "":
        return None
    try:
        parsed = int(raw)
    except (TypeError, ValueError):
        return None
    return parsed if parsed > 0 else None


def resolve_opencode_context_length(spec=None, entry=None, fallback: int = 8192):
    entry_obj = entry if isinstance(entry, dict) else {}
    spec_obj = spec if isinstance(spec, dict) else {}
    context_length = parse_positive_int_or_none(entry_obj.get("context_length"))
    if context_length is None:
        commands = spec_obj.get("commands")
        if not isinstance(commands, list):
            commands = []
        context_length = parse_positive_int_or_none(parse_named_command_arg(commands, "--max-model-len"))
    return context_length if context_length is not None else fallback


def resolve_opencode_limit_split(spec=None, entry=None, *, context_fallback: int = 8192, output_fallback: int = 16384):
    max_model_len = resolve_opencode_context_length(spec=spec, entry=entry, fallback=context_fallback)
    max_model_len = parse_positive_int_or_none(max_model_len)
    if max_model_len is None:
        return {
            "context": int(context_fallback),
            "output": int(output_fallback),
        }

    return {
        "context": int(max_model_len),
        "output": int(output_fallback),
    }


def resolve_opencode_output_length(sample_query=None, fallback: int = 4096):
    sample_query_obj = sample_query if isinstance(sample_query, dict) else {}
    body = sample_query_obj.get("body")
    if not isinstance(body, dict):
        body = {}
    for field_name in ("max_completion_tokens", "max_output_tokens", "max_tokens"):
        output_length = parse_positive_int_or_none(body.get(field_name))
        if output_length is not None:
            return output_length
    return fallback


def sanitize_download_stem(value: str, fallback: str = "config") -> str:
    normalized = re.sub(r"[^A-Za-z0-9._-]+", "-", str(value or "").strip()).strip("._-")
    return normalized or fallback


def build_opencode_config_payload(*, api_base_url: str, api_key: str, model_name: str, context_length: int, output_length: int):
    normalized_api_base_url = str(api_base_url or "").strip()
    normalized_api_key = str(api_key or "").strip()
    normalized_model_name = str(model_name or "").strip()
    normalized_context_length = int(context_length)
    normalized_output_length = int(output_length)

    payload = {
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            "inferx": {
                "npm": "@ai-sdk/openai-compatible",
                "name": "inferx",
                "options": {
                    "baseURL": normalized_api_base_url,
                    "apiKey": normalized_api_key,
                },
                "models": {
                    normalized_model_name: {
                        "name": normalized_model_name,
                        "limit": {
                            "context": normalized_context_length,
                            "output": normalized_output_length,
                        },
                    }
                },
            }
        },
    }

    template_text = load_opencode_config_template_text()
    if template_text != "":
        try:
            template_obj = json.loads(
                template_text
                .replace('"myprovider"', json.dumps("inferx"))
                .replace('"{apiKey}"', json.dumps(normalized_api_key))
                .replace('"{ModelName}"', json.dumps(normalized_model_name))
                .replace('"{contextLength}"', json.dumps(normalized_context_length))
                .replace('"{outputLength}"', json.dumps(normalized_output_length))
                .replace('{contextLength}', json.dumps(normalized_context_length))
                .replace('{outputLength}', json.dumps(normalized_output_length))
                .replace(
                    '"https://model.inferx.net/funccall/{tenant}/{namespace}/{funcname}/v1"',
                    json.dumps(normalized_api_base_url),
                )
            )
            if isinstance(template_obj, dict):
                payload = template_obj
        except Exception:
            pass

    return payload


def build_opencode_download_response(*, filename_stem: str, api_base_url: str, api_key: str, model_name: str, context_length: int, output_length: int):
    payload = build_opencode_config_payload(
        api_base_url=api_base_url,
        api_key=api_key,
        model_name=model_name,
        context_length=context_length,
        output_length=output_length,
    )
    response = Response(
        json.dumps(payload, indent=2) + "\n",
        status=200,
        mimetype="application/json",
    )
    response.headers["Content-Disposition"] = 'attachment; filename="opencode.json"'
    return response


def build_sample_rest_call_for_ui(tenant: str, namespace: str, funcname: str, sample_query, apikey: str):
    if not isinstance(sample_query, dict):
        return ""

    path = str(sample_query.get("path", "v1/completions") or "v1/completions").strip().lstrip("/")
    body = sample_query.get("body", {})
    if not isinstance(body, dict):
        body = {}
    body_for_ui = clone_json_value(body)

    prompt = sample_query.get("prompt")
    api_type = str(sample_query.get("apiType", "") or "").strip().lower()
    if api_type in ("text2text", "knowledgebase"):
        path = "v1/chat/completions"
        body_for_ui = build_text2text_chat_body_for_ui(body_for_ui, prompt)
        body_for_ui.setdefault("stream", True)

    if api_type == "image2text":
        messages = body_for_ui.get("messages")
        if not isinstance(messages, list) or len(messages) == 0:
            body_for_ui["messages"] = [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": str(prompt or "What is in this image?")},
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": "<data-url>",
                            },
                        },
                    ],
                },
            ]
        else:
            def replace_image_urls_with_placeholder(value):
                if isinstance(value, list):
                    return [replace_image_urls_with_placeholder(item) for item in value]
                if isinstance(value, dict):
                    replaced = {}
                    for key, item in value.items():
                        if key == "image_url":
                            image_value = clone_json_value(item) if isinstance(item, dict) else {}
                            image_value["url"] = "<data-url>"
                            replaced[key] = image_value
                        else:
                            replaced[key] = replace_image_urls_with_placeholder(item)
                    return replaced
                if isinstance(value, str) and value.startswith("data:"):
                    return "<data-url>"
                return value

            body_for_ui["messages"] = replace_image_urls_with_placeholder(messages)
        body_for_ui.setdefault("stream", True)

    token = str(apikey or "").strip()
    if token == "":
        token = build_inference_apikey_placeholder()

    base_url = normalize_public_api_base_url()
    url = f"{base_url}/funccall/{tenant}/{namespace}/{funcname}/{path}"
    tenant_name = str(tenant or "").strip().lower()
    include_auth_header = tenant_name != "public"
    auth_header_line = f"  -H 'Authorization: Bearer {token}'" if include_auth_header else ""

    def shell_single_quote(value) -> str:
        return str(value).replace("'", "'\"'\"'")

    is_transcription_path = path.lower() == "v1/audio/transcriptions"
    if api_type == "transcriptions" or is_transcription_path:
        curl_lines = [
            f"curl -X POST {url}",
            "  -H 'Content-Type: multipart/form-data'",
        ]
        if auth_header_line != "":
            curl_lines.append(auth_header_line)
        curl_lines.append("  -F 'file=@/path/to/audio.wav'")
        for key, value in body_for_ui.items():
            if str(key or "").strip() == "" or key == "model" or value is None:
                continue
            if isinstance(value, (dict, list)):
                serialized_value = json.dumps(value, ensure_ascii=False)
            elif isinstance(value, bool):
                serialized_value = "true" if value else "false"
            else:
                serialized_value = str(value)
            if serialized_value.strip() == "":
                continue
            curl_lines.append(
                f"  -F '{shell_single_quote(key)}={shell_single_quote(serialized_value)}'"
            )
        return " \\\n".join(curl_lines)

    body_json = json.dumps(body_for_ui, ensure_ascii=False)
    body_json = body_json.replace("'", "'\"'\"'")
    curl_lines = [
        f"curl -X POST {url}",
        "  -H 'Content-Type: application/json'",
    ]
    if auth_header_line != "":
        curl_lines.append(auth_header_line)
    curl_lines.append(f"  -d '{body_json}'")
    return " \\\n".join(curl_lines)


def mask_sample_rest_call_for_ui(sample_rest_call: str, apikey: str) -> str:
    rendered_call = str(sample_rest_call or "")
    token = str(apikey or "").strip()
    if rendered_call == "" or token == "":
        return rendered_call
    return rendered_call.replace(token, mask_apikey_for_ui(token))


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
    return Response(resp.iter_content(1024), resp.status_code, headers)


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


@prefix_bp.route("/endpoints", methods=["GET"])
@not_require_login
def EndpointList():
    is_authenticated = session.get('access_token', '') != ''
    view = str(request.args.get("view", "") or "").strip().lower()
    roles = listroles() if is_authenticated else []
    is_inferx_admin = has_inferx_admin_role(roles) if is_authenticated else False

    if view == "" and is_inferx_admin:
        view = "admin"

    if view == "admin" and is_inferx_admin:
        try:
            entries = build_admin_endpoint_list_entries()
        except Exception as e:
            return json_error(f"failed to load admin endpoints: {e}", 500)
        return render_template(
            "endpoints_list.html",
            endpoint_entries=entries,
            is_admin_view=True,
            selected_tenant="",
            tenant_options=[],
            public_api_base_url=normalize_public_api_base_url(),
            endpoint_admin_href=dashboard_href("prefix.EndpointList", view="admin"),
            endpoint_tenant_href=dashboard_href("prefix.EndpointList", view="tenant"),
        )

    if is_authenticated:
        tenant_options = accessible_endpoint_tenant_names(roles)
        selected_tenant = str(request.args.get("tenant", "") or "").strip()
        if selected_tenant != "" and selected_tenant not in tenant_options:
            selected_tenant = ""
        if selected_tenant == "" and tenant_options:
            active_tenant = str(session.get("active_tenant_name", session.get("tenant_name", "")) or "").strip()
            selected_tenant = active_tenant if active_tenant in tenant_options else tenant_options[0]

        try:
            entries = build_endpoint_list_entries(include_unpublished=False, tenant=selected_tenant)
            onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(selected_tenant)
        except Exception as e:
            return json_error(f"failed to load endpoints: {e}", 500)

        preview_tenant = selected_tenant
        api_key_display = mask_apikey_for_ui(onboarding_apikey) if onboarding_apikey else build_inference_apikey_placeholder()
        api_key_copy_value = onboarding_apikey if onboarding_apikey else build_inference_apikey_placeholder()
        api_key_copyable = bool(onboarding_apikey)
        api_key_name = onboarding_apikey_name
    else:
        tenant_options = []
        selected_tenant = "public"
        try:
            entries = build_endpoint_list_entries(include_unpublished=False, tenant=PUBLIC_TENANT_NAME)
        except Exception as e:
            return json_error(f"failed to load public endpoints: {e}", 500)

        preview_tenant = "<tenant>"
        api_key_display = "Log in to get API key"
        api_key_copy_value = ""
        api_key_copyable = False
        api_key_name = ""

    for entry in entries:
        entry["client_setup_preview"] = build_endpoint_client_setup_preview(
            preview_tenant,
            entry.get("slug", ""),
            entry.get("model_name", ""),
            entry.get("provider", ""),
        )

    return render_template(
        "endpoints_list.html",
        endpoint_entries=entries,
        is_admin_view=False,
        is_authenticated=is_authenticated,
        selected_tenant=selected_tenant,
        tenant_options=tenant_options,
        api_key_display=api_key_display,
        api_key_copy_value=api_key_copy_value,
        api_key_copyable=api_key_copyable,
        api_key_name=api_key_name,
        public_api_base_url=normalize_public_api_base_url(),
        endpoint_admin_href=dashboard_href("prefix.EndpointList", view="admin"),
        endpoint_tenant_href=dashboard_href("prefix.EndpointList", tenant=selected_tenant, view="tenant"),
        login_href=url_for('prefix.login', redirectpath=request.url),
        page_title="InferX Endpoints | Serverless Inference For OpenCode, Dify, OpenWebUI",
        page_description="Browse published InferX endpoints for serverless GPU inference, OpenAI-compatible APIs, OpenCode, KiloCode, Dify, OpenWebUI, and agent-native workloads optimized for subsecond cold start integration.",
        page_keywords="InferX endpoints, OpenCode, KiloCode, Dify, OpenWebUI, serverless inference, subsecond cold start, OpenAI-compatible API, agent-native workloads",
        page_robots="index, follow",
    )


@prefix_bp.route("/endpoints/<slug>", methods=["GET"])
@not_require_login
def EndpointDetail(slug):
    is_authenticated = session.get('access_token', '') != ''
    selected_tenant = str(request.args.get("tenant", session.get("active_tenant_name", session.get("tenant_name", ""))) or "").strip()

    if is_authenticated:
        roles = listroles()
        active_tenant = resolve_active_tenant_name(roles)
        if selected_tenant == "":
            tenant_options = accessible_endpoint_tenant_names(roles)
            selected_tenant = tenant_options[0] if tenant_options else ""

        try:
            detail = load_tenant_endpoint_detail(slug, selected_tenant)
            entry = detail["entry"]
            endpoint_state = detail["endpoint_state"]
            if not detail["published"]:
                raise LookupError(f"endpoint `{slug}` not found")
        except LookupError:
            return render_resource_unavailable_page(
                resource_kind="Endpoint",
                resource_name=slug,
                tenant=selected_tenant,
                namespace="endpoints",
                message="This endpoint is no longer published.",
                suggestion="It may have been unpublished or the URL may be outdated.",
                primary_href=dashboard_href("prefix.EndpointList"),
                primary_label="Back to Endpoints",
                secondary_href=dashboard_href("prefix.CatalogList"),
                secondary_label="Catalog",
                status=404,
            )
        except PermissionError:
            return render_resource_unavailable_page(
                resource_kind="Endpoint",
                resource_name=slug,
                tenant=selected_tenant,
                namespace="endpoints",
                message="This endpoint is unavailable.",
                suggestion="The URL may be outdated or this endpoint may not be available in the current tenant context.",
                primary_href=dashboard_href("prefix.EndpointList"),
                primary_label="Back to Endpoints",
                secondary_href=dashboard_href("prefix.CatalogList"),
                secondary_label="Catalog",
                status=404,
            )
        except Exception as e:
            return json_error(f"failed to load endpoint `{slug}`: {e}", 500)

        onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(active_tenant or selected_tenant)
        client_setup = build_client_setup_for_ui(
            tenant=selected_tenant,
            namespace="endpoints",
            funcname=slug,
            sample_query=entry.get("sample_query"),
            apikey=onboarding_apikey,
            apikey_name=onboarding_apikey_name,
            spec=entry.get("spec"),
        )
        sample_rest_call = build_sample_rest_call_for_ui(
            tenant=selected_tenant,
            namespace="endpoints",
            funcname=slug,
            sample_query=entry.get("sample_query"),
            apikey=onboarding_apikey,
        )
        sample_rest_call_display = mask_sample_rest_call_for_ui(sample_rest_call, onboarding_apikey)
    else:
        selected_tenant = PUBLIC_TENANT_NAME
        try:
            detail = load_tenant_endpoint_detail(slug, PUBLIC_TENANT_NAME)
            entry = detail["entry"]
            endpoint_state = detail["endpoint_state"]
        except LookupError:
            return render_resource_unavailable_page(
                resource_kind="Endpoint",
                resource_name=slug,
                tenant=PUBLIC_TENANT_NAME,
                namespace="endpoints",
                message="This endpoint is no longer published.",
                suggestion="It may have been unpublished or the URL may be outdated.",
                primary_href=dashboard_href("prefix.EndpointList"),
                primary_label="Back to Endpoints",
                secondary_href=dashboard_href("prefix.CatalogList"),
                secondary_label="Catalog",
                status=404,
            )
        except PermissionError:
            return render_resource_unavailable_page(
                resource_kind="Endpoint",
                resource_name=slug,
                tenant=PUBLIC_TENANT_NAME,
                namespace="endpoints",
                message="This endpoint is unavailable.",
                suggestion="The URL may be outdated or the endpoint is not currently public.",
                primary_href=dashboard_href("prefix.EndpointList"),
                primary_label="Back to Endpoints",
                secondary_href=dashboard_href("prefix.CatalogList"),
                secondary_label="Catalog",
                status=404,
            )
        except Exception as e:
            return json_error(f"failed to load public endpoint `{slug}`: {e}", 500)

        client_setup = build_endpoint_client_setup_preview(
            "<tenant>",
            slug,
            entry.get("model_name", ""),
            entry.get("provider", ""),
        )
        sample_rest_call = build_sample_rest_call_for_ui(
            "<tenant>",
            "endpoints",
            slug,
            entry.get("sample_query"),
            "<INFERENCE_API_KEY>",
        )
        sample_rest_call_display = sample_rest_call

    runtime_context = build_endpoint_runtime_context(
        entry.get("spec"),
        entry.get("sample_query"),
        fallback_slug=slug,
    )
    endpoint_funcspec = ""
    try:
        endpoint_funcspec = json.dumps(project_func_for_edit(entry.get("platform_detail"))["spec"], indent=4)
    except Exception:
        endpoint_funcspec = ""

    return render_template(
        "endpoint_detail.html",
        endpoint_entry=entry,
        is_admin_view=False,
        is_authenticated=is_authenticated,
        selected_tenant=selected_tenant,
        client_setup=client_setup,
        opencode_download_href=(
            url_for("prefix.DownloadEndpointOpenCodeConfig", slug=slug, tenant=selected_tenant)
            if is_authenticated and selected_tenant
            else ""
        ),
        sample_rest_call=sample_rest_call,
        sample_rest_call_display=sample_rest_call_display,
        interactive_enabled=runtime_context["enabled"] if is_authenticated else False,
        interactive_api_type=runtime_context["api_type"],
        interactive_path=runtime_context["path"],
        interactive_prompt=runtime_context["prompt"],
        interactive_map=runtime_context["map"],
        endpoint_list_href=dashboard_href("prefix.EndpointList", tenant=selected_tenant) if is_authenticated else dashboard_href("prefix.EndpointList"),
        endpoint_admin_href=dashboard_href("prefix.EndpointAdminDetail", slug=slug),
        endpoint_tenant_href=dashboard_href("prefix.EndpointDetail", slug=slug, tenant=selected_tenant) if is_authenticated else dashboard_href("prefix.EndpointDetail", slug=slug),
        endpoint_state=endpoint_state,
        endpoint_funcspec=endpoint_funcspec,
        login_href=url_for('prefix.login', redirectpath=request.url),
        page_title=f"InferX Endpoint | {slug}",
        page_description=f"Explore the InferX endpoint `{slug}` with OpenAI-compatible integration details for OpenCode, KiloCode, Dify, OpenWebUI, and serverless GPU inference workflows.",
        page_keywords=f"{slug}, InferX endpoint, OpenCode, KiloCode, Dify, OpenWebUI, serverless inference, OpenAI-compatible API",
        page_robots="index, follow",
    )


@prefix_bp.route("/integration/opencode/endpoints/<slug>.json", methods=["GET"])
@require_login
def DownloadEndpointOpenCodeConfig(slug):
    try:
        roles = listroles()
        active_tenant = resolve_active_tenant_name(roles)
        tenant_options = accessible_endpoint_tenant_names(roles)
        selected_tenant = str(
            request.args.get("tenant", session.get("active_tenant_name", session.get("tenant_name", ""))) or ""
        ).strip()
        if selected_tenant != "" and selected_tenant not in tenant_options:
            selected_tenant = ""
        if selected_tenant == "":
            selected_tenant = tenant_options[0] if tenant_options else ""
        if selected_tenant == "":
            return json_error("No accessible tenant is available for endpoint integration.", 403)

        detail = load_tenant_endpoint_detail(slug, selected_tenant)
        entry = detail["entry"]
        if not detail["published"]:
            raise LookupError(f"endpoint `{slug}` not found")

        onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(active_tenant or selected_tenant)
        client_setup = build_client_setup_for_ui(
            tenant=selected_tenant,
            namespace="endpoints",
            funcname=slug,
            sample_query=entry.get("sample_query"),
            apikey=onboarding_apikey,
            apikey_name=onboarding_apikey_name,
            spec=entry.get("spec"),
        )
        api_base_url = str(client_setup.get("api_base_url", "") or "").strip()
        model_name = str(client_setup.get("model_name", "") or "").strip()
        api_key = str(client_setup.get("api_key", "") or "").strip()
        if api_base_url == "" or model_name == "" or api_key == "":
            return json_error("OpenCode config is unavailable for this endpoint.", 400)

        limits = resolve_opencode_limit_split(spec=entry.get("spec"), entry=entry)
        return build_opencode_download_response(
            filename_stem=f"inferx-opencode-endpoint-{slug}",
            api_base_url=api_base_url,
            api_key=api_key,
            model_name=model_name,
            context_length=limits["context"],
            output_length=limits["output"],
        )
    except LookupError:
        return json_error(f"endpoint `{slug}` is not available for tenant `{selected_tenant}`", 404)
    except PermissionError:
        return json_error(f"tenant `{selected_tenant}` cannot access endpoint `{slug}`", 403)
    except Exception as e:
        return json_error(f"failed to build OpenCode config for endpoint `{slug}`: {e}", 500)


@prefix_bp.route("/admin/endpoints/<slug>", methods=["GET"])
@require_login
@require_admin
def EndpointAdminDetail(slug):
    try:
        detail = load_live_endpoint_detail(slug)
        entry = detail["entry"]
        endpoint_state = detail["endpoint_state"]
    except LookupError:
        return render_resource_unavailable_page(
            resource_kind="Endpoint",
            resource_name=slug,
            tenant="inferx",
            namespace="endpoint",
            message="This platform endpoint is no longer available.",
            suggestion="It may have been removed or renamed.",
            primary_href=dashboard_href("prefix.EndpointList", view="admin"),
            primary_label="Back to Endpoints",
            secondary_href=dashboard_href("prefix.CatalogList"),
            secondary_label="Catalog",
            status=404,
        )
    except Exception as e:
        return json_error(f"failed to load admin endpoint `{slug}`: {e}", 500)

    roles = listroles()
    active_tenant = resolve_active_tenant_name(roles)
    onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(active_tenant)
    client_setup = build_client_setup_for_ui(
        tenant=active_tenant,
        namespace="endpoints",
        funcname=slug,
        sample_query=entry.get("sample_query"),
        apikey=onboarding_apikey,
        apikey_name=onboarding_apikey_name,
        spec=entry.get("spec"),
    )
    sample_rest_call = build_sample_rest_call_for_ui(
        tenant=active_tenant,
        namespace="endpoints",
        funcname=slug,
        sample_query=entry.get("sample_query"),
        apikey=onboarding_apikey,
    )
    sample_rest_call_display = mask_sample_rest_call_for_ui(sample_rest_call, onboarding_apikey)
    runtime_context = build_endpoint_runtime_context(
        entry.get("spec"),
        entry.get("sample_query"),
        fallback_slug=slug,
    )
    endpoint_funcspec = ""
    try:
        endpoint_funcspec = json.dumps(project_func_for_edit(entry.get("platform_detail"))["spec"], indent=4)
    except Exception:
        endpoint_funcspec = ""
    return render_template(
        "endpoint_detail.html",
        endpoint_entry=entry,
        is_admin_view=True,
        is_authenticated=True,
        selected_tenant=active_tenant,
        client_setup=client_setup,
        opencode_download_href=(
            url_for("prefix.DownloadEndpointOpenCodeConfig", slug=slug, tenant=active_tenant)
            if active_tenant
            else ""
        ),
        sample_rest_call=sample_rest_call,
        sample_rest_call_display=sample_rest_call_display,
        interactive_enabled=runtime_context["enabled"],
        interactive_api_type=runtime_context["api_type"],
        interactive_path=runtime_context["path"],
        interactive_prompt=runtime_context["prompt"],
        interactive_map=runtime_context["map"],
        endpoint_list_href=dashboard_href("prefix.EndpointList", view="admin"),
        endpoint_admin_href=dashboard_href("prefix.EndpointAdminDetail", slug=slug),
        endpoint_tenant_href=dashboard_href("prefix.EndpointDetail", slug=slug, view="tenant"),
        endpoint_state=endpoint_state,
        endpoint_funcspec=endpoint_funcspec,
    )


@prefix_bp.route("/admin/endpoints/<slug>/edit", methods=["GET"])
@require_login
@require_admin
def EndpointAdminEdit(slug):
    try:
        detail = load_live_endpoint_detail(slug, allow_missing_live_spec=False, allow_missing_status=False)
        existing_entry = detail["metadata_entry"]
        platform_detail = detail["platform_detail"]
        endpoint_state = detail["endpoint_state"]
        prefill, catalog_entry = build_endpoint_editor_prefill(slug, existing_entry, platform_detail)
    except LookupError:
        return render_resource_unavailable_page(
            resource_kind="Endpoint",
            resource_name=slug,
            tenant="inferx",
            namespace="endpoint",
            message="This platform endpoint is no longer available.",
            suggestion="It may have been removed or renamed.",
            primary_href=dashboard_href("prefix.EndpointList", view="admin"),
            primary_label="Back to Endpoints",
            status=404,
        )
    except Exception as e:
        return json_error(f"failed to load endpoint editor for `{slug}`: {e}", 500)

    return render_template(
        "endpoint_admin_edit.html",
        endpoint_slug=slug,
        endpoint_prefill=prefill,
        endpoint_catalog_entry=catalog_entry,
        endpoint_detail_href=dashboard_href("prefix.EndpointAdminDetail", slug=slug),
        endpoint_state=endpoint_state,
        catalog_tag_groups=CATALOG_TAG_GROUPS,
        catalog_use_case_groups=CATALOG_RECOMMENDED_USE_CASE_GROUPS,
    )


@prefix_bp.route("/admin/endpoints/<slug>/metadata", methods=["PUT"])
@require_login
@require_admin
def EndpointAdminSaveMetadata(slug):
    req = request.get_json(silent=True) or {}
    try:
        payload = endpoint_metadata_payload_from_prefill(req)
        version = proxy_gateway_endpoint_admin_action("PUT", slug, metadata=payload)
    except ValueError as e:
        return json_error(str(e), 400)
    except Exception as e:
        return json_error(f"failed to save endpoint metadata: {e}", 502)
    return jsonify({"slug": slug, "version": version, "saved": True})


@prefix_bp.route("/admin/endpoints/<slug>/publish", methods=["POST"])
@require_login
@require_admin
def EndpointAdminPublish(slug):
    req = request.get_json(silent=True) or {}
    try:
        payload = endpoint_metadata_payload_from_prefill(req)
        existing_entry = None
        try:
            existing_entry = query_endpoint_row_by_slug(slug)
        except LookupError:
            existing_entry = None

        if existing_entry is None:
            platform_detail = getfunc("inferx", "endpoint", slug)
            catalog_entry = resolve_endpoint_catalog_entry(slug, platform_detail)
            payload = fill_missing_endpoint_metadata_from_source(payload, catalog_entry)

        version = proxy_gateway_endpoint_admin_action("POST", slug, metadata=payload)
    except ValueError as e:
        return json_error(str(e), 400)
    except Exception as e:
        return json_error(f"failed to publish endpoint: {e}", 502)
    return jsonify({"slug": slug, "version": version, "published": True})


@prefix_bp.route("/admin/endpoints/<slug>/unpublish", methods=["POST"])
@require_login
@require_admin
def EndpointAdminUnpublish(slug):
    try:
        version = proxy_gateway_endpoint_admin_action("POST", slug, metadata=None)
    except Exception as e:
        return json_error(f"failed to unpublish endpoint: {e}", 502)
    return jsonify({"slug": slug, "version": version, "published": False})


@prefix_bp.route("/admin/skilltemplates", methods=["GET"])
@require_login
@require_admin
def AdminSkillTemplates():
    try:
        templates = list_admin_skill_templates()
    except Exception as e:
        return json_error(f"failed to load skill templates: {e}", 500)

    return render_template(
        "admin_skill_templates.html",
        skill_templates=templates,
    )


@prefix_bp.route("/admin/skilltemplates/json", methods=["GET"])
@require_login
@require_admin
def AdminSkillTemplatesJson():
    try:
        templates = list_admin_skill_templates()
    except Exception as e:
        return json_error(f"failed to load skill templates: {e}", 500)
    return jsonify(templates)


@prefix_bp.route("/admin/skilltemplate_create", methods=["GET"])
@require_login
@require_admin
def AdminSkillTemplateCreate():
    prefill = {
        "tenant": "inferx",
        "namespace": "skills",
        "display_name": "",
        "description": "",
        "func_tenant": "inferx",
        "func_namespace": "skills",
        "normal_funcname": "",
        "is_active": True,
    }

    return render_template(
        "admin_skill_template_create.html",
        form_data=prefill,
        error_message="",
    )


@prefix_bp.route("/admin/skilltemplate_save", methods=["POST"])
@require_login
@require_admin
def AdminSkillTemplateSave():
    try:
        payload = normalize_admin_skill_template_form(request.form)
        create_admin_skill_template(payload)
    except ValueError as e:
        return render_template(
            "admin_skill_template_create.html",
            form_data={
                "tenant": str(request.form.get("tenant", "") or "").strip(),
                "namespace": str(request.form.get("namespace", "") or "").strip(),
                "display_name": str(request.form.get("display_name", "") or "").strip(),
                "description": str(request.form.get("description", "") or "").strip(),
                "func_tenant": str(request.form.get("func_tenant", "") or "").strip(),
                "func_namespace": str(request.form.get("func_namespace", "") or "").strip(),
                "normal_funcname": str(request.form.get("normal_funcname", "") or "").strip(),
                "is_active": str(request.form.get("is_active", "true") or "true").strip().lower() not in ("0", "false", "off", "no"),
            },
            error_message=str(e),
        )
    except Exception as e:
        message = str(e)
        if "skill template name is taken" in message:
            message = "Template name is already taken"
        return render_template(
            "admin_skill_template_create.html",
            form_data={
                "tenant": str(request.form.get("tenant", "") or "").strip(),
                "namespace": str(request.form.get("namespace", "") or "").strip(),
                "display_name": str(request.form.get("display_name", "") or "").strip(),
                "description": str(request.form.get("description", "") or "").strip(),
                "func_tenant": str(request.form.get("func_tenant", "") or "").strip(),
                "func_namespace": str(request.form.get("func_namespace", "") or "").strip(),
                "normal_funcname": str(request.form.get("normal_funcname", "") or "").strip(),
                "is_active": str(request.form.get("is_active", "true") or "true").strip().lower() not in ("0", "false", "off", "no"),
            },
            error_message=message,
        )

    return redirect(url_for("prefix.apikeys"))


@prefix_bp.route("/admin/skilltemplate/<int:template_id>/deactivate", methods=["POST"])
@require_login
@require_admin
def AdminSkillTemplateDeactivate(template_id: int):
    try:
        deactivate_admin_skill_template(template_id)
    except Exception as e:
        return json_error(f"failed to deactivate skill template: {e}", 502)
    return jsonify({"ok": True})


@prefix_bp.route("/admin/skilltemplate/<int:template_id>/activate", methods=["POST"])
@require_login
@require_admin
def AdminSkillTemplateActivate(template_id: int):
    try:
        activate_admin_skill_template(template_id)
    except Exception as e:
        return json_error(f"failed to activate skill template: {e}", 502)
    return jsonify({"ok": True})


@prefix_bp.route("/admin/skilltemplate/<int:template_id>", methods=["GET"])
@require_login
@require_admin
def AdminSkillTemplateDetail(template_id: int):
    try:
        templates = list_admin_skill_templates()
    except Exception as e:
        return json_error(f"failed to load skill template: {e}", 500)

    template = next((t for t in templates if t.get("template_id") == template_id), None)
    if template is None:
        return json_error("skill template not found", 404)

    return render_template(
        "admin_skill_template_detail.html",
        template=template,
    )


@prefix_bp.route("/admin/skilltemplate/<int:template_id>/edit", methods=["GET"])
@require_login
@require_admin
def AdminSkillTemplateEdit(template_id: int):
    try:
        templates = list_admin_skill_templates()
    except Exception as e:
        return json_error(f"failed to load skill template: {e}", 500)

    template = next((t for t in templates if t.get("template_id") == template_id), None)
    if template is None:
        return json_error("skill template not found", 404)

    return render_template(
        "admin_skill_template_edit.html",
        template_id=template_id,
        form_data=template,
        error_message="",
    )


@prefix_bp.route("/admin/skilltemplate/<int:template_id>/update", methods=["POST"])
@require_login
@require_admin
def AdminSkillTemplateUpdate(template_id: int):
    def render_edit(error_message):
        return render_template(
            "admin_skill_template_edit.html",
            template_id=template_id,
            form_data={
                "tenant": str(request.form.get("tenant", "") or "").strip(),
                "namespace": str(request.form.get("namespace", "") or "").strip(),
                "display_name": str(request.form.get("display_name", "") or "").strip(),
                "description": str(request.form.get("description", "") or "").strip(),
                "func_tenant": str(request.form.get("func_tenant", "") or "").strip(),
                "func_namespace": str(request.form.get("func_namespace", "") or "").strip(),
                "normal_funcname": str(request.form.get("normal_funcname", "") or "").strip(),
                "is_active": str(request.form.get("is_active", "true") or "true").strip().lower() not in ("0", "false", "off", "no"),
            },
            error_message=error_message,
        )

    try:
        payload = normalize_admin_skill_template_form(request.form)
        update_admin_skill_template(template_id, payload)
    except ValueError as e:
        return render_edit(str(e))
    except Exception as e:
        message = str(e)
        if "skill template name is taken" in message:
            message = "Template name is already taken"
        return render_edit(message)

    return redirect(url_for("prefix.apikeys"))


@prefix_bp.route("/admin/skilltemplate/<int:template_id>/delete", methods=["POST"])
@require_login
@require_admin
def AdminSkillTemplateDelete(template_id: int):
    try:
        delete_admin_skill_template(template_id)
    except Exception as e:
        message = str(e)
        if "in use by existing skills" in message:
            return json_error("Cannot delete: template is in use by existing skills.", 409)
        return json_error(f"failed to delete skill template: {e}", 502)
    return jsonify({"ok": True})


def render_skill_create_page(*, form_data=None, error_message="", success_message=""):
    try:
        context = build_skill_form_context()
    except Exception as e:
        return json_error(f"failed to load skill create page: {e}", 500)

    deploy_target = context["deploy_target"]
    skill_templates = context["skill_templates"]
    merged_form_data = {
        "skillname": "",
        "tenant": deploy_target.get("selected_tenant", ""),
        "namespace": deploy_target.get("selected_namespace", ""),
        "template_id": "",
        "prefix": "",
        "gpu_billing_target": "caller",
    }
    if isinstance(form_data, dict):
        merged_form_data.update(form_data)
    if merged_form_data.get("template_id", "") == "" and len(skill_templates) == 1:
        merged_form_data["template_id"] = str(skill_templates[0].get("template_id") or "")

    return render_template(
        "skill_create.html",
        form_data=merged_form_data,
        error_message=error_message,
        success_message=success_message,
        deploy_target=deploy_target,
        skill_templates=skill_templates,
        metadata_gen_model_configured=bool(METADATA_GEN_MODEL and "/" in METADATA_GEN_MODEL),
    )


@prefix_bp.route("/listskills", methods=["GET"])
@require_login
def ListSkillsLegacy():
    return redirect(url_for("prefix.ListSkills"))


@prefix_bp.route("/listskills/myskills", methods=["GET"])
@require_login
def ListSkills():
    try:
        context = load_skill_dashboard_context(skills_view="my_skills")
    except Exception as e:
        return json_error(f"failed to load skills: {e}", 500)

    return render_template(
        "skill_list.html",
        skills_view="my_skills",
        **context,
    )


@prefix_bp.route("/listskills/public", methods=["GET"])
@require_login
def SkillMarketplace():
    try:
        context = load_skill_dashboard_context(skills_view="marketplace")
    except Exception as e:
        return json_error(f"failed to load marketplace: {e}", 500)

    return render_template(
        "skill_list.html",
        skills_view="marketplace",
        **context,
    )


@prefix_bp.route("/skill_create", methods=["GET"])
@require_login
def SkillCreate():
    return render_skill_create_page()


@prefix_bp.route("/skill_save", methods=["POST"])
@require_login
def SkillSave():
    raw_allowed = str(request.form.get("allowed_child_skilleps", "") or "").strip()
    allowed_child_skilleps = [s.strip() for s in raw_allowed.split(",") if s.strip()]
    form_data = {
        "skillname": str(request.form.get("skillname", "") or "").strip(),
        "tenant": str(request.form.get("tenant", "") or "").strip(),
        "namespace": str(request.form.get("namespace", "") or "").strip(),
        "template_id": str(request.form.get("template_id", "") or "").strip(),
        "description": str(request.form.get("description", "") or "").strip(),
        "gpu_billing_target": str(request.form.get("gpu_billing_target", "caller") or "caller").strip().lower(),
        "prefix": str(request.form.get("prefix", "") or ""),
    }

    try:
        if form_data["tenant"] == "":
            raise ValueError("Tenant is required")
        if not re.fullmatch(r"^[a-zA-Z0-9][a-zA-Z0-9-]*$", form_data["skillname"]):
            raise ValueError("Skill name must match ^[a-zA-Z0-9][a-zA-Z0-9-]*$")
        if form_data["namespace"] == "":
            raise ValueError("Namespace is required")
        template_id = int(form_data["template_id"])
        if not form_data["description"]:
            raise ValueError("Description is required")
        if form_data["gpu_billing_target"] not in ("caller", "owner"):
            raise ValueError("GPU billed to must be Caller or Owner")
        if not form_data["prefix"].strip():
            raise ValueError("System prompt is required")

        payload = {
            "template_id": template_id,
            "description": form_data["description"] or None,
            "prefix": form_data["prefix"],
            "serving_mode": "dedicated",
            "has_cache": False,
            "gpu_billing_target": form_data["gpu_billing_target"],
            "allowed_child_skilleps": allowed_child_skilleps,
        }

        create_skill(
            form_data["tenant"],
            form_data["namespace"],
            form_data["skillname"],
            payload,
        )
    except ValueError as e:
        return render_skill_create_page(form_data=form_data, error_message=str(e))
    except Exception as e:
        return render_skill_create_page(form_data=form_data, error_message=str(e))

    flash("Skill created — publish it when ready", "success")
    return redirect(url_for("prefix.ListSkills"))


@prefix_bp.route("/skill/publish/<owner>/<ns>/<name>", methods=["POST"])
@require_login
def SkillPublish(owner, ns, name):
    try:
        post_skill_state_action(owner, ns, name, "publish")
    except Exception as e:
        return json_error(f"failed to publish skill: {e}", 502)
    return redirect(url_for("prefix.ListSkills"))


@prefix_bp.route("/skill/unpublish/<owner>/<ns>/<name>", methods=["POST"])
@require_login
def SkillUnpublish(owner, ns, name):
    try:
        post_skill_state_action(owner, ns, name, "unpublish")
    except Exception as e:
        return json_error(f"failed to unpublish skill: {e}", 502)
    return redirect(url_for("prefix.ListSkills"))


@prefix_bp.route("/skill/delete/<owner>/<ns>/<name>", methods=["POST"])
@require_login
def SkillDelete(owner, ns, name):
    try:
        delete_skill(owner, ns, name)
    except Exception as e:
        return json_error(f"failed to delete skill: {e}", 502)
    return redirect(url_for("prefix.ListSkills"))


@prefix_bp.route("/skill_subscription/subscribe", methods=["POST"])
@require_login
def SkillSubscriptionSubscribe():
    roles = listroles()
    active_tenant = resolve_active_tenant_name(roles)
    if active_tenant == "":
        return json_error("No active tenant selected", 400)

    try:
        if not has_admin_role_for_model(roles, active_tenant) and not is_inferx_admin_user():
            return json_error("No permission", 403)

        req = request.get_json(silent=True) or {}
        owner = str(req.get("owner_tenant", "") or "").strip()
        namespace = str(req.get("owner_namespace", "") or "").strip()
        skillname = str(req.get("skillname", "") or "").strip()
        alias = str(req.get("tool_alias", "") or "").strip()
        if owner == "" or namespace == "" or skillname == "":
            return json_error("owner_tenant, owner_namespace, and skillname are required", 400)
        if alias != "" and not all(ch.isascii() and (ch.isalnum() or ch in "-_.") for ch in alias):
            return json_error("tool_alias may contain only letters, numbers, hyphens, underscores, and periods", 400)
        created = create_skill_subscription(owner, namespace, skillname, alias or None, active_tenant=active_tenant)
    except SkillSubscriptionGatewayError as e:
        return json_error(str(e), e.status_code)
    except Exception as e:
        return json_error(f"failed to subscribe to function: {e}", 502)

    return jsonify(created), 201


@prefix_bp.route("/skill_subscription/unsubscribe", methods=["POST"])
@require_login
def SkillSubscriptionUnsubscribe():
    roles = listroles()
    active_tenant = resolve_active_tenant_name(roles)
    if active_tenant == "":
        return json_error("No active tenant selected", 400)

    try:
        if not has_admin_role_for_model(roles, active_tenant) and not is_inferx_admin_user():
            return json_error("No permission", 403)

        req = request.get_json(silent=True) or {}
        owner = str(req.get("owner_tenant", "") or "").strip()
        namespace = str(req.get("owner_namespace", "") or "").strip()
        skillname = str(req.get("skillname", "") or "").strip()
        if owner == "" or namespace == "" or skillname == "":
            return json_error("owner_tenant, owner_namespace, and skillname are required", 400)
        delete_skill_subscription(owner, namespace, skillname, active_tenant=active_tenant)
    except SkillSubscriptionGatewayError as e:
        return json_error(str(e), e.status_code)
    except Exception as e:
        return json_error(f"failed to unsubscribe from skill: {e}", 502)

    return jsonify({"ok": True})


@prefix_bp.route("/skill_subscription/update_alias", methods=["POST"])
@require_login
def SkillSubscriptionUpdateAlias():
    roles = listroles()
    active_tenant = resolve_active_tenant_name(roles)
    if active_tenant == "":
        return json_error("No active tenant selected", 400)

    try:
        if not has_admin_role_for_model(roles, active_tenant) and not is_inferx_admin_user():
            return json_error("No permission", 403)

        req = request.get_json(silent=True) or {}
        owner = str(req.get("owner_tenant", "") or "").strip()
        namespace = str(req.get("owner_namespace", "") or "").strip()
        skillname = str(req.get("skillname", "") or "").strip()
        alias = str(req.get("tool_alias", "") or "").strip()
        if owner == "" or namespace == "" or skillname == "":
            return json_error("owner_tenant, owner_namespace, and skillname are required", 400)
        if not alias or not all(ch.isascii() and (ch.isalnum() or ch in "-_.") for ch in alias):
            return json_error("tool_alias may contain only letters, numbers, hyphens, underscores, and periods", 400)
        updated = update_skill_subscription_alias(owner, namespace, skillname, alias, active_tenant=active_tenant)
    except SkillSubscriptionGatewayError as e:
        return json_error(str(e), e.status_code)
    except Exception as e:
        return json_error(f"failed to update subscription alias: {e}", 502)

    return jsonify(updated)


@prefix_bp.route("/generate_skills", methods=["GET"])
@require_login
def GenerateSkills():
    tenant = str(request.args.get("tenant", "") or "").strip()
    namespace = str(request.args.get("namespace", "") or "").strip()
    keyword = str(request.args.get("keyword", "") or "").strip()
    if not tenant or not namespace:
        return json_error("tenant and namespace are required", 400)
    try:
        own_skills = list_namespace_skills(tenant, namespace)
    except Exception:
        own_skills = []
    try:
        marketplace_payload = list_marketplace_skills(keyword=keyword or None, page_size=50, active_tenant=tenant)
        marketplace_items = marketplace_payload.get("items", []) if isinstance(marketplace_payload, dict) else []
    except Exception:
        marketplace_items = []

    seen = set()
    results = []

    def add_item(row, is_own):
        ot = str(row.get("owner_tenant", "") or "").strip()
        ns = str(row.get("owner_namespace", "") or "").strip()
        sn = str(row.get("skillname", "") or "").strip()
        if not ot or not ns or not sn:
            return
        key = f"{ot}/{ns}/{sn}"
        if key in seen:
            return
        if is_own and keyword:
            kw = keyword.lower()
            haystack = " ".join([sn, str(row.get("description", "") or ""), str(row.get("template_display_name", "") or "")]).lower()
            if kw not in haystack:
                return
        seen.add(key)
        results.append({
            "skillep_id": key,
            "name": sn,
            "template_display_name": str(row.get("template_display_name", "") or ""),
            "description": str(row.get("description", "") or ""),
        })

    for row in own_skills:
        add_item(row, is_own=True)
    for row in marketplace_items:
        add_item(row, is_own=False)

    return jsonify(results)


@prefix_bp.route("/generate_skill_metadata", methods=["POST"])
@require_login
def GenerateSkillMetadata():
    if not METADATA_GEN_MODEL or "/" not in METADATA_GEN_MODEL:
        return json_error("METADATA_GEN_MODEL is not configured", 503)
    namespace, modelname = METADATA_GEN_MODEL.split("/", 1)
    tenant = session.get("active_tenant_name", session.get("tenant_name", ""))
    if not tenant:
        return json_error("no active tenant", 400)
    req_data = request.get_json(silent=True) or {}
    subskills = req_data.get("subskills", [])
    if not isinstance(subskills, list) or not subskills:
        return json_error("subskills list is required", 400)

    skills_summary = "\n".join(
        f"- {s.get('name', '')}: {s.get('description', '') or '(no description)'}"
        for s in subskills
    )
    prompt = (
        f"Given these sub-skills:\n{skills_summary}\n\n"
        "Respond with valid JSON only, no explanation, no markdown fences. Use this exact schema:\n"
        "{\"description\": \"<1-2 sentence description of a parent skill that routes to these sub-skills>\"}"
    )

    url = f"{get_gateway_url()}/funccall/{tenant}/{namespace}/{modelname}/v1/chat/completions"
    try:
        resp = requests.post(
            url,
            headers=gateway_request_headers(json_body=True),
            json={"messages": [{"role": "user", "content": prompt}], "stream": False, "chat_template_kwargs": {"enable_thinking": False}},
            timeout=30,
        )
    except Exception as e:
        return json_error(f"metadata generation request failed: {e}", 502)

    if not resp.ok:
        return json_error(f"metadata generation failed: HTTP {resp.status_code}", 502)

    payload = response_json_or_none(resp)
    try:
        llm_text = (
            payload["choices"][0]["message"]["content"]
            if payload and payload.get("choices")
            else ""
        )
    except (KeyError, IndexError, TypeError):
        llm_text = ""

    how_to_use = "\n## How to use\n" + "\n".join(
        f"- **{s.get('name', '')}**: call {s.get('skillep_id', '')}"
        for s in subskills
    )

    import json as _json
    try:
        parsed = _json.loads(llm_text.strip())
    except Exception:
        parsed = {}
    description = str(parsed.get("description", "") or "").strip()
    subskill_lines = [
        f"- **{s.get('name', '')}** (`{s.get('skillep_id', '')}`): {s.get('description', '') or '(no description)'}"
        for s in subskills
    ]
    prefix_parts = [
        "You are a routing assistant. Analyze the user's request and call the most relevant sub-skill.",
        "## Available Sub-skills\n" + "\n".join(subskill_lines),
    ]
    prefix_parts.append(how_to_use)
    prefix = "\n\n".join(prefix_parts)

    return jsonify({"description": description, "prefix": prefix})


def get_skill_detail(owner_tenant: str, namespace: str, skillname: str):
    url = f"{apihostaddr}/skills/{owner_tenant}/{namespace}/{skillname}"
    resp = requests.get(url, headers=gateway_request_headers(), timeout=60)
    payload = response_json_or_none(resp)
    if is_upstream_resource_unavailable(resp, payload):
        raise LookupError(f"skill `{owner_tenant}/{namespace}/{skillname}` not found")
    if not resp.ok:
        detail = extract_upstream_error_message(resp, payload)
        raise RuntimeError(detail or f"HTTP {resp.status_code}")
    if not isinstance(payload, dict):
        raise RuntimeError("invalid skill detail response")
    return payload


def load_skill_prefix_text(tenant: str, namespace: str, name: str, version: int) -> str:
    path = SKILLS_DIR / f"{tenant}.{namespace}.{name}" / str(version) / "skill.data"
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""
    except Exception:
        return ""


def build_skill_sample_rest_call(owner_tenant: str, namespace: str, skillname: str, normal_funcname: str, apikey: str) -> str:
    base_url = normalize_public_api_base_url()
    url = f"{base_url}/skills/{owner_tenant}/{namespace}/{skillname}/v1/chat/completions"
    token = str(apikey or "").strip() or build_inference_apikey_placeholder()
    body = json.dumps({
        "model": normal_funcname,
        "messages": [{"role": "user", "content": "What's in the context?"}],
        "stream": True,
    }, indent=2)
    lines = [
        f"curl -X POST '{url}'",
        f"  -H 'Authorization: Bearer {token}'",
        "  -H 'Content-Type: application/json'",
        f"  -d '{body}'",
    ]
    return " \\\n".join(lines)


@prefix_bp.route("/skill", methods=["GET"])
@require_login
def SkillDetailLegacy():
    return redirect(url_for("prefix.SkillDetail", **request.args))


@prefix_bp.route("/function", methods=["GET"])
@require_login
def SkillDetail():
    owner = str(request.args.get("owner", "") or "").strip()
    ns = str(request.args.get("ns", "") or "").strip()
    name = str(request.args.get("name", "") or "").strip()

    if not owner or not ns or not name:
        return json_error("owner, ns, and name are required", 400)

    try:
        skill = get_skill_detail(owner, ns, name)
    except LookupError as e:
        return json_error(str(e), 404)
    except Exception as e:
        return json_error(f"failed to load skill: {e}", 502)

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    can_manage = has_admin_role_for_model(roles, owner, ns) or is_inferx_admin_user()
    if not can_manage and not skill.get("is_published"):
        return Response("No permission", status=403)

    active_tenant = resolve_active_tenant_name(roles)
    onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(active_tenant or owner)
    normal_funcname = str(skill.get("normal_funcname", "") or "").strip()

    func_tenant = str(skill.get("func_tenant", "") or "").strip()
    func_namespace = str(skill.get("func_namespace", "") or "").strip()
    model_name = normal_funcname
    if func_tenant and func_namespace and normal_funcname:
        try:
            _, backing_func = getfunc_response(func_tenant, func_namespace, normal_funcname)
            if backing_func is not None:
                backing_spec = (((backing_func.get("func") or {}).get("object") or {}).get("spec") or {})
                resolved = resolve_effective_model_target_from_spec(backing_spec)
                if resolved:
                    model_name = resolved
        except Exception:
            pass

    sample_rest_call = build_skill_sample_rest_call(owner, ns, name, model_name, onboarding_apikey)
    sample_rest_call_display = mask_sample_rest_call_for_ui(sample_rest_call, onboarding_apikey) if onboarding_apikey else sample_rest_call

    base_url = normalize_public_api_base_url()
    skill_api_base_url = f"{base_url}/skills/{owner}/{ns}/{name}/v1"
    mcp_server_url = f"{base_url}/mcp"
    function_id = f"{owner}/{ns}/{name}"
    auth_required = str(owner or "").strip().lower() != "public"
    normalized_apikey = str(onboarding_apikey or "").strip()
    api_key_copyable = False
    if normalized_apikey != "":
        api_key_value = normalized_apikey
        api_key_display = mask_apikey_for_ui(normalized_apikey)
        api_key_copyable = True
    else:
        api_key_value = build_inference_apikey_placeholder()
        api_key_display = api_key_value
    client_setup = {
        "integration_kind": "skill_mcp",
        "api_base_url": skill_api_base_url,
        "mcp_server_url": mcp_server_url,
        "function_id": function_id,
        "auth_required": auth_required,
        "model_name": model_name,
        "api_key": api_key_value,
        "api_key_display": api_key_display,
        "api_key_copyable": api_key_copyable,
        "api_key_name": str(onboarding_apikey_name or "").strip(),
    }
    opencode_download_href = ""
    if session.get("access_token", "") != "" and skill_api_base_url:
        opencode_download_href = url_for(
            "prefix.DownloadSkillOpenCodeConfig",
            owner=owner,
            ns=ns,
            name=name,
        )
    skill_prefix_text = ""
    if can_manage:
        version = skill.get("version") or 0
        skill_prefix_text = load_skill_prefix_text(owner, ns, name, int(version or 0))

    return render_template(
        "skill_detail.html",
        skill=skill,
        owner=owner,
        ns=ns,
        name=name,
        normal_funcname=normal_funcname,
        model_name=model_name,
        sample_rest_call=sample_rest_call,
        sample_rest_call_display=sample_rest_call_display,
        client_setup=client_setup,
        opencode_download_href=opencode_download_href,
        can_manage=can_manage,
        skill_prefix_text=skill_prefix_text,
    )


@prefix_bp.route("/catalog", methods=["GET"])
@not_require_login
def CatalogList():
    is_authenticated = session.get('access_token', '') != ''
    try:
        is_inferx_admin = is_inferx_admin_user() if is_authenticated else False
        entries = list_catalog_entries(active_only=not is_inferx_admin)
        filter_options = collect_catalog_filter_options(entries)
        if is_authenticated:
            deploy_target = build_catalog_deploy_target_selector_context()
        else:
            deploy_target = build_catalog_deploy_target_unavailable_context(
                "Log in to deploy or customize a catalog model."
            )
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
        is_authenticated=is_authenticated,
        login_href=url_for('prefix.login', redirectpath=request.url),
        published_entry_count=published_entry_count,
        unpublished_entry_count=unpublished_entry_count,
        page_title="InferX Catalog | Serverless Model Catalog For Agent-Native Workloads",
        page_description="Browse the InferX catalog of deployable serverless models for OpenAI-compatible inference, OpenCode, Dify, OpenWebUI, and agent-native workloads.",
        page_keywords="InferX catalog, serverless model catalog, deployable AI models, OpenCode, Dify, OpenWebUI, OpenAI-compatible API, agent-native workloads",
        page_robots="index, follow",
    )


@prefix_bp.route("/catalog/<slug>", methods=["GET"])
@not_require_login
def CatalogDetail(slug):
    is_authenticated = session.get('access_token', '') != ''
    try:
        is_inferx_admin = is_inferx_admin_user() if is_authenticated else False
        if is_authenticated:
            entry = fetch_catalog_entry_by_slug_for_current_user(slug)
            deploy_target = build_catalog_deploy_target_selector_context()
        else:
            entry = fetch_active_catalog_entry_by_slug(slug)
            deploy_target = build_catalog_deploy_target_unavailable_context(
                "Log in to deploy this catalog model."
            )
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
        is_authenticated=is_authenticated,
        login_href=url_for('prefix.login', redirectpath=request.url),
        page_title=f"InferX Catalog | {entry_for_view.get('display_name', slug)}",
        page_description=f"Explore the InferX catalog model `{entry_for_view.get('display_name', slug)}` for serverless deployment, OpenAI-compatible inference, and agent-native application integration.",
        page_keywords=f"{entry_for_view.get('display_name', slug)}, InferX catalog, serverless model deployment, OpenAI-compatible inference, OpenCode, Dify, OpenWebUI",
        page_robots="index, follow",
    )


@prefix_bp.route("/help", methods=["GET"])
@not_require_login
def HelpPage():
    is_authenticated = session.get('access_token', '') != ''
    endpoint_href = dashboard_href("prefix.EndpointList")
    model_create_href = dashboard_href("prefix.FuncCreate") if is_authenticated else url_for('prefix.login', redirectpath=dashboard_href("prefix.FuncCreate"))
    return render_template(
        "help.html",
        endpoint_list_href=endpoint_href,
        catalog_list_href=dashboard_href("prefix.CatalogList"),
        model_create_href=model_create_href,
        models_list_href=dashboard_href("prefix.ListFunc"),
        is_authenticated=is_authenticated,
        page_title="InferX Help | Endpoints, Catalog, And Serverless Model Deployment",
        page_description="Learn when to use InferX Endpoints, Catalog, or deploy from scratch for OpenCode, Dify, OpenWebUI, OpenAI-compatible APIs, serverless inference, and agent-native workloads.",
        page_keywords="InferX help, OpenCode, Dify, OpenWebUI, serverless inference, OpenAI-compatible API, subsecond cold start, endpoints, catalog, model deployment",
        page_robots="index, follow",
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

        tenant, namespace = resolve_catalog_deploy_target_selection(
            deploy_target,
            requested_tenant=req.get("tenant", ""),
            requested_namespace=req.get("namespace", ""),
            allow_implicit=True,
        )
        name = build_catalog_default_func_name(entry)

        try:
            resp = deploy_catalog_entry_to_function_target(
                entry,
                tenant=tenant,
                namespace=namespace,
                name=name,
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


class RemoteImageFetchError(Exception):
    def __init__(self, status_code: int, message: str):
        super().__init__(message)
        self.status_code = status_code
        self.message = message


class RemoteAudioFetchError(Exception):
    def __init__(self, status_code: int, message: str):
        super().__init__(message)
        self.status_code = status_code
        self.message = message


def infer_remote_image_content_type(content_type: str, remote_url: str) -> str:
    normalized_type = str(content_type or "").split(";", 1)[0].strip().lower()
    if normalized_type in REMOTE_IMAGE_ALLOWED_MIME_TYPES:
        return normalized_type

    guessed_type, _ = mimetypes.guess_type(urlparse(remote_url).path)
    guessed_type = str(guessed_type or "").strip().lower()
    if guessed_type in REMOTE_IMAGE_ALLOWED_MIME_TYPES:
        return guessed_type
    return ""


def infer_remote_audio_content_type(content_type: str, remote_url: str) -> str:
    normalized_type = str(content_type or "").split(";", 1)[0].strip().lower()
    if normalized_type in REMOTE_AUDIO_ALLOWED_MIME_TYPES:
        return normalized_type

    guessed_type, _ = mimetypes.guess_type(urlparse(remote_url).path)
    guessed_type = str(guessed_type or "").split(";", 1)[0].strip().lower()
    if guessed_type in REMOTE_AUDIO_ALLOWED_MIME_TYPES:
        return guessed_type
    return ""


def is_allowed_remote_fetch_host(hostname: str) -> bool:
    normalized_host = str(hostname or "").strip().lower()
    if normalized_host == "" or normalized_host == "localhost" or normalized_host.endswith(".local"):
        return False

    try:
        address_info = socket.getaddrinfo(normalized_host, None, type=socket.SOCK_STREAM)
    except socket.gaierror:
        return False

    if len(address_info) == 0:
        return False

    for entry in address_info:
        ip_text = str(entry[4][0] or "").split("%", 1)[0].strip()
        if ip_text == "":
            return False
        try:
            ip_addr = ipaddress.ip_address(ip_text)
        except ValueError:
            return False
        if (
            ip_addr.is_private
            or ip_addr.is_loopback
            or ip_addr.is_link_local
            or ip_addr.is_reserved
            or ip_addr.is_multicast
            or ip_addr.is_unspecified
        ):
            return False

    return True


def validate_remote_fetch_url(raw_url: str, error_cls) -> str:
    normalized_url = str(raw_url or "").strip()
    if normalized_url == "":
        raise error_cls(400, "Remote URL is required.")

    parsed = urlparse(normalized_url)
    if parsed.scheme not in ("http", "https"):
        raise error_cls(400, "Remote URL must use http or https.")
    if parsed.hostname is None or parsed.netloc == "":
        raise error_cls(400, "Remote URL must include a valid host.")
    if parsed.username or parsed.password:
        raise error_cls(400, "Remote URL must not include embedded credentials.")
    if not is_allowed_remote_fetch_host(parsed.hostname):
        raise error_cls(403, "Remote URL host is not allowed.")

    return parsed._replace(fragment="").geturl()


def fetch_remote_image_bytes(remote_url: str) -> tuple[bytes, str]:
    current_url = validate_remote_fetch_url(remote_url, RemoteImageFetchError)
    session = requests.Session()

    try:
        for _redirect_index in range(REMOTE_IMAGE_FETCH_REDIRECT_LIMIT + 1):
            try:
                resp = session.get(
                    current_url,
                    headers=REMOTE_IMAGE_FETCH_HEADERS,
                    timeout=(REMOTE_IMAGE_FETCH_CONNECT_TIMEOUT_SEC, REMOTE_IMAGE_FETCH_READ_TIMEOUT_SEC),
                    stream=True,
                    allow_redirects=False,
                )
            except requests.exceptions.Timeout:
                raise RemoteImageFetchError(504, "Remote image download timed out.")
            except requests.exceptions.RequestException as e:
                raise RemoteImageFetchError(502, f"Remote image download failed: {e}")

            if resp.status_code in (301, 302, 303, 307, 308):
                redirect_target = str(resp.headers.get("Location") or "").strip()
                resp.close()
                if redirect_target == "":
                    raise RemoteImageFetchError(502, "Remote image redirect was missing a Location header.")
                current_url = validate_remote_fetch_url(urljoin(current_url, redirect_target), RemoteImageFetchError)
                continue

            if resp.status_code != 200:
                resp.close()
                raise RemoteImageFetchError(
                    resp.status_code,
                    f"Remote image fetch failed with HTTP {resp.status_code}.",
                )

            content_type = infer_remote_image_content_type(resp.headers.get("Content-Type"), current_url)
            if content_type == "":
                resp.close()
                raise RemoteImageFetchError(
                    415,
                    "Remote URL did not return a supported image type. Use JPEG, PNG, or WebP.",
                )

            image_bytes = bytearray()
            try:
                for chunk in resp.iter_content(chunk_size=64 * 1024):
                    if not chunk:
                        continue
                    image_bytes.extend(chunk)
                    if len(image_bytes) > REMOTE_IMAGE_FETCH_MAX_BYTES:
                        raise RemoteImageFetchError(
                            413,
                            "Remote image is too large. The source file must be 10 MiB or smaller.",
                        )
            except requests.exceptions.Timeout:
                raise RemoteImageFetchError(504, "Remote image download timed out while reading the response body.")
            except requests.exceptions.RequestException as e:
                raise RemoteImageFetchError(
                    502,
                    f"Remote image download failed while reading the response body: {e}",
                )
            finally:
                resp.close()

            return bytes(image_bytes), content_type

        raise RemoteImageFetchError(400, "Remote image fetch hit too many redirects.")
    finally:
        session.close()


def fetch_remote_audio_bytes(remote_url: str) -> tuple[bytes, str]:
    current_url = validate_remote_fetch_url(remote_url, RemoteAudioFetchError)
    session = requests.Session()

    try:
        for _redirect_index in range(REMOTE_IMAGE_FETCH_REDIRECT_LIMIT + 1):
            try:
                resp = session.get(
                    current_url,
                    headers=REMOTE_AUDIO_FETCH_HEADERS,
                    timeout=(REMOTE_IMAGE_FETCH_CONNECT_TIMEOUT_SEC, REMOTE_IMAGE_FETCH_READ_TIMEOUT_SEC),
                    stream=True,
                    allow_redirects=False,
                )
            except requests.exceptions.Timeout:
                raise RemoteAudioFetchError(504, "Remote audio download timed out.")
            except requests.exceptions.RequestException as e:
                raise RemoteAudioFetchError(502, f"Remote audio download failed: {e}")

            if resp.status_code in (301, 302, 303, 307, 308):
                redirect_target = str(resp.headers.get("Location") or "").strip()
                resp.close()
                if redirect_target == "":
                    raise RemoteAudioFetchError(502, "Remote audio redirect was missing a Location header.")
                current_url = validate_remote_fetch_url(urljoin(current_url, redirect_target), RemoteAudioFetchError)
                continue

            if resp.status_code != 200:
                resp.close()
                raise RemoteAudioFetchError(
                    resp.status_code,
                    f"Remote audio fetch failed with HTTP {resp.status_code}.",
                )

            content_type = infer_remote_audio_content_type(resp.headers.get("Content-Type"), current_url)
            if content_type == "":
                resp.close()
                raise RemoteAudioFetchError(
                    415,
                    "Remote URL did not return a supported audio type. Use WAV, MP3, MP4, M4A, OGG, WebM, or FLAC.",
                )

            audio_bytes = bytearray()
            try:
                for chunk in resp.iter_content(chunk_size=64 * 1024):
                    if not chunk:
                        continue
                    audio_bytes.extend(chunk)
                    if len(audio_bytes) > REMOTE_AUDIO_FETCH_MAX_BYTES:
                        raise RemoteAudioFetchError(
                            413,
                            "Remote audio is too large. The source file must be 25 MiB or smaller.",
                        )
            except requests.exceptions.Timeout:
                raise RemoteAudioFetchError(504, "Remote audio download timed out while reading the response body.")
            except requests.exceptions.RequestException as e:
                raise RemoteAudioFetchError(
                    502,
                    f"Remote audio download failed while reading the response body: {e}",
                )
            finally:
                resp.close()

            return bytes(audio_bytes), content_type

        raise RemoteAudioFetchError(400, "Remote audio fetch hit too many redirects.")
    finally:
        session.close()


@prefix_bp.route('/image2text/fetch-remote', methods=['POST'])
@not_require_login
def fetch_remote_image_for_image2text():
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict):
        return Response("Request body must be a JSON object.", status=400, mimetype='text/plain')

    remote_url = payload.get("url")
    try:
        image_bytes, content_type = fetch_remote_image_bytes(remote_url)
    except RemoteImageFetchError as e:
        return Response(e.message, status=e.status_code, mimetype='text/plain')

    response = Response(image_bytes, status=200, mimetype=content_type)
    response.headers["Cache-Control"] = "no-store"
    response.headers["X-Content-Type-Options"] = "nosniff"
    response.headers["Content-Length"] = str(len(image_bytes))
    return response


@prefix_bp.route('/audio/fetch-remote', methods=['POST'])
@not_require_login
def fetch_remote_audio_for_transcriptions():
    payload = request.get_json(silent=True)
    if not isinstance(payload, dict):
        return Response("Request body must be a JSON object.", status=400, mimetype='text/plain')

    remote_url = payload.get("url")
    try:
        audio_bytes, content_type = fetch_remote_audio_bytes(remote_url)
    except RemoteAudioFetchError as e:
        return Response(e.message, status=e.status_code, mimetype='text/plain')

    response = Response(audio_bytes, status=200, mimetype=content_type)
    response.headers["Cache-Control"] = "no-store"
    response.headers["X-Content-Type-Options"] = "nosniff"
    response.headers["Content-Length"] = str(len(audio_bytes))
    return response


@prefix_bp.route('/proxy/object/function/<tenant>/<namespace>/<name>/', methods=['DELETE'])
@not_require_login
def proxy_delete_function(tenant, namespace, name):
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    has_client_auth = any(key.lower() == 'authorization' for key in headers)
    if access_token != "" and not has_client_auth:
        headers["Authorization"] = f'Bearer {access_token}'

    function_row = find_function_row(tenant, namespace, name)
    if function_row is not None:
        try:
            roles = listroles()
        except Exception:
            roles = []
        if not has_admin_role_for_model(roles, tenant, namespace) and not can_view_public_tenant(roles):
            return Response("No permission", status=403)
        result = delete_function_via_dashboard(tenant, namespace, name)
        return Response(result["message"], status=result["status"], mimetype='text/plain')

    url = f"{apihostaddr}/object/function/{tenant}/{namespace}/{name}/"
    try:
        resp = requests.delete(url, headers=headers, timeout=60)
    except requests.exceptions.RequestException as e:
        return Response(f"Error connecting to backend server: {e}", status=502)

    excluded_headers = ['content-encoding', 'transfer-encoding', 'connection']
    response_headers = [
        (header_name, header_value)
        for header_name, header_value in resp.headers.items()
        if header_name.lower() not in excluded_headers
    ]
    return Response(resp.content, resp.status_code, response_headers)

@prefix_bp.route('/proxy/<path:path>', methods=['GET', 'POST', 'PUT', 'DELETE', 'PATCH', 'OPTIONS'])
@not_require_login
def proxy(path):
    # TODO(auth-hardening): migrate dashboard JS calls to send explicit Authorization
    # headers (for example via a dedicated fetch wrapper) and stop implicit session
    # token injection in this generic proxy endpoint.
    access_token = session.get('access_token', '')
    headers = {key: value for key, value in request.headers if key.lower() != 'host'}
    normalized_path = path.lstrip('/')
    active_tenant = str(session.get("active_tenant_name", session.get("tenant_name", "")) or "").strip()
    # Keep public funccall requests anonymous. Some gateway configs reject
    # dashboard session tokens on /funccall while allowing unauthenticated
    # access for public tenant models.
    is_public_funccall = normalized_path.startswith("funccall/public/")
    has_client_auth = any(key.lower() == 'authorization' for key in headers)
    has_client_tenant = any(key.lower() == 'x-tenant' for key in headers)
    if access_token != "" and not has_client_auth and not is_public_funccall:
        headers["Authorization"] = f'Bearer {access_token}'
    if (
        active_tenant != ""
        and not has_client_tenant
        and normalized_path.startswith("skills/")
    ):
        headers["X-Tenant"] = active_tenant
    
    # Construct the full URL for the backend request
    url = f"{apihostaddr}/{path}"
    request_body = request.get_data()
    # Keep proxy read-timeout slightly above client-declared inference timeout so
    # backend timeout responses are returned directly when possible.
    requested_timeout_sec = parse_inferx_timeout_seconds(request.headers.get("X-Inferx-Timeout"), default_value=60.0)
    proxy_read_timeout_sec = min(requested_timeout_sec + 5.0, 600.0)
    connect_timeout_sec = 10.0
    maybe_log_proxy_gateway_request(
        path=normalized_path,
        upstream_url=url,
        method=request.method,
        headers=headers,
        cookies=request.cookies,
        body_bytes=request_body,
        timeout_sec=proxy_read_timeout_sec,
    )

    try:
        resp = requests.request(
            method=request.method,
            url=url,
            headers=headers,
            data=request_body,
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


@prefix_bp.route("/admin/usage/endpoints", methods=["GET"])
@require_admin
def admin_endpoint_usage():
    """Admin cross-tenant endpoint usage API"""
    try:
        hours = request.args.get("hours", type=int, default=24)
        start = request.args.get("start")
        end = request.args.get("end")
        limit = request.args.get("limit", type=int, default=100)
        offset = request.args.get("offset", type=int, default=0)
        group_by = request.args.get("group_by")
        tenant = request.args.get("tenant")
        
        # Validate hours range (1-720)
        if hours is not None and (hours < 1 or hours > 720):
            return json_error("hours must be between 1 and 720 (30 days)", 400)
        
        # Build query params
        params = {
            "hours": hours,
            "start": start,
            "end": end,
            "limit": limit,
            "offset": offset,
            "group_by": group_by,
            "tenant": tenant,
        }
        
        # Proxy to gateway
        gateway_url = get_gateway_url()
        url = f"{gateway_url}/admin/usage/endpoints?{urlencode({k: v for k, v in params.items() if v is not None}, doseq=True)}"
        
        response = proxy_request_to_gateway(url, method="GET")
        return response
        
    except Exception as e:
        return json_error(f"failed to get admin endpoint usage: {e}", 500)


@prefix_bp.route("/admin/usage/endpoints/<tenant>/<endpoint_slug>", methods=["GET"])
@require_admin
def admin_endpoint_usage_by_period(tenant, endpoint_slug):
    """Admin endpoint drill-down API"""
    try:
        hours = request.args.get("hours", type=int, default=24)
        start = request.args.get("start")
        end = request.args.get("end")
        granularity = request.args.get("granularity", default="hourly")
        
        # Validate hours range (1-720)
        if hours is not None and (hours < 1 or hours > 720):
            return json_error("hours must be between 1 and 720 (30 days)", 400)
        
        # Validate granularity
        if granularity not in ("hourly", "daily", "weekly"):
            return json_error("granularity must be 'hourly', 'daily', or 'weekly'", 400)
        
        # Build query params
        params = {"hours": hours, "start": start, "end": end, "granularity": granularity}
        
        # Proxy to gateway
        gateway_url = get_gateway_url()
        encoded_tenant = quote(str(tenant), safe="")
        encoded_endpoint_slug = quote(str(endpoint_slug), safe="")
        url = f"{gateway_url}/admin/usage/endpoints/{encoded_tenant}/{encoded_endpoint_slug}?{urlencode({k: v for k, v in params.items() if v is not None}, doseq=True)}"
        
        response = proxy_request_to_gateway(url, method="GET")
        return response
        
    except Exception as e:
        return json_error(f"failed to get endpoint usage by period: {e}", 500)


def get_gateway_url():
    """Use the same API gateway base address as the rest of the dashboard."""
    return str(apihostaddr or "").rstrip("/")


def proxy_request_to_gateway(url, method="GET", json_data=None):
    """Proxy request to the gateway"""
    headers = {}
    
    # Prefer an explicit Authorization header, but fall back to the dashboard session token.
    if "Authorization" in request.headers:
        headers["Authorization"] = request.headers["Authorization"]
    else:
        access_token = str(session.get("access_token", "") or "")
        if access_token != "":
            headers["Authorization"] = f"Bearer {access_token}"
    
    try:
        if method == "GET":
            resp = requests.get(url, headers=headers, timeout=60)
        elif method == "POST":
            resp = requests.post(url, headers=headers, json=json_data, timeout=60)
        elif method == "PUT":
            resp = requests.put(url, headers=headers, json=json_data, timeout=60)
        elif method == "DELETE":
            resp = requests.delete(url, headers=headers, timeout=60)
        else:
            return json_error(f"Unsupported method: {method}", 405)
    except requests.exceptions.RequestException as e:
        return json_error(f"failed to reach gateway: {e}", 502)
    
    excluded_headers = {
        "connection",
        "content-encoding",
        "content-length",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    }
    response_headers = [
        (name, value)
        for name, value in resp.headers.items()
        if name.lower() not in excluded_headers
    ]

    return Response(resp.content, status=resp.status_code, headers=response_headers)


def json_error(message, status=400):
    """Return a JSON error response"""
    return jsonify({"error": message}), status


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
                if editor_mode == "basic" and hf_model == "":
                    raise ValueError("existing function spec is missing an effective model target required for basic mode")
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
            partial_spec = validate_partial_spec(
                spec,
                max_node_vram,
                max_node_gpu_count=max_node_gpu_count,
                editor_mode=editor_mode,
            )
            if hf_model == "":
                hf_model = resolve_effective_model_target_from_spec(
                    partial_spec,
                    commands_label="submitted function commands",
                    sample_query_label="submitted function sample_query.body.model",
                ).strip()
            if editor_mode == "basic" and hf_model == "":
                raise ValueError("`hf_model` is required in basic mode when the submitted spec does not encode a model target")
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


@prefix_bp.route("/api/funcs/<tenant>/<namespace>/<name>", methods=["GET"])
@require_login
def api_get_func(tenant, namespace, name):
    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    resp, payload = getfunc_response(tenant, namespace, name)
    if is_upstream_resource_unavailable(resp, payload):
        return jsonify({"exists": False}), 404
    if not resp.ok:
        return json_error(
            extract_upstream_error_message(resp, payload) or "failed to load function",
            resp.status_code,
        )
    return jsonify({"exists": True}), 200



@prefix_bp.route("/skill/download-prefix/<owner>/<ns>/<name>", methods=["GET"])
@require_login
def download_skill_prefix(owner, ns, name):
    deny_resp = deny_public_tenant_request(owner)
    if deny_resp is not None:
        return deny_resp

    if not re.fullmatch(r"^[a-zA-Z0-9][a-zA-Z0-9-]*$", name):
        return json_error("invalid skill name", 400)

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not has_admin_role_for_model(roles, owner, ns) and not is_inferx_admin_user():
        return Response("No permission", status=403)

    try:
        skill = get_skill_detail(owner, ns, name)
    except LookupError as e:
        return json_error(str(e), 404)
    except Exception as e:
        return json_error(f"failed to load skill: {e}", 502)

    version = skill.get("version") or 0
    skill_file = SKILLS_DIR / f"{owner}.{ns}.{name}" / str(version) / "skill.data"
    if not skill_file.exists():
        return json_error("system prompt file not found", 404)

    return send_from_directory(
        str(skill_file.parent),
        skill_file.name,
        as_attachment=True,
        download_name=f"{name}-skill.data",
    )


@prefix_bp.route("/skill/download-opencode/<owner>/<ns>/<name>", methods=["GET"])
@require_login
def DownloadSkillOpenCodeConfig(owner, ns, name):
    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not has_admin_role_for_model(roles, owner, ns) and not is_inferx_admin_user():
        return Response("No permission", status=403)

    try:
        skill = get_skill_detail(owner, ns, name)
    except LookupError as e:
        return json_error(str(e), 404)
    except Exception as e:
        return json_error(f"failed to load skill: {e}", 502)

    active_tenant = resolve_active_tenant_name(roles)
    onboarding_apikey, _ = resolve_onboarding_inference_apikey_for_ui(active_tenant or owner)
    normalized_apikey = str(onboarding_apikey or "").strip()
    if not normalized_apikey:
        normalized_apikey = build_inference_apikey_placeholder()

    base_url = normalize_public_api_base_url()
    api_base_url = f"{base_url}/skills/{owner}/{ns}/{name}/v1"
    normal_funcname = str(skill.get("normal_funcname", "") or "").strip()
    func_tenant = str(skill.get("func_tenant", "") or "").strip()
    func_namespace = str(skill.get("func_namespace", "") or "").strip()
    model_name = normal_funcname
    if func_tenant and func_namespace and normal_funcname:
        try:
            _, backing_func = getfunc_response(func_tenant, func_namespace, normal_funcname)
            if backing_func is not None:
                backing_spec = (((backing_func.get("func") or {}).get("object") or {}).get("spec") or {})
                resolved = resolve_effective_model_target_from_spec(backing_spec)
                if resolved:
                    model_name = resolved
        except Exception:
            pass
    if not model_name:
        return json_error("OpenCode config is unavailable: missing model name.", 400)

    return build_opencode_download_response(
        filename_stem=f"inferx-opencode-skill-{owner}-{ns}-{name}",
        api_base_url=api_base_url,
        api_key=normalized_apikey,
        model_name=model_name,
        context_length=8192,
        output_length=16384,
    )


@prefix_bp.route("/kb/upload/<tenant>/<namespace>/<name>", methods=["POST"])
@require_login
def upload_knowledgebase_file(tenant, namespace, name):
    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    try:
        validate_function_name_or_raise(name)
    except ValueError as e:
        return json_error(str(e), 400)

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not has_admin_role_for_model(roles, tenant, namespace):
        return Response("No permission", status=403)

    try:
        if find_function_row(tenant, namespace, name) is not None:
            return json_error(
                f"function `{tenant}/{namespace}/{name}` already exists; KB upload is only allowed for new functions",
                409,
            )
    except Exception as e:
        return json_error(f"failed to check existing function: {e}", 500)

    uploaded = request.files.get("file")
    if uploaded is None:
        return json_error("missing `file` upload", 400)

    filename = str(getattr(uploaded, "filename", "") or "").strip()
    if filename == "":
        return json_error("no file selected", 400)
    model = str(request.form.get("model", "") or "").strip()
    if model == "":
        return json_error("missing `model`", 400)

    try:
        stage_file = knowledgebase_stage_file_path(tenant, namespace, name)
        stage_file.parent.mkdir(parents=True, exist_ok=True)
        uploaded.save(stage_file)
    except Exception as e:
        return json_error(f"failed to store KB file: {e}", 500)

    try:
        kb_token_count = count_kb_tokens_via_gateway(
            tenant=tenant,
            namespace=namespace,
            name=name,
            model=model,
        )
    except ValueError as e:
        try:
            stage_file.unlink(missing_ok=True)
        except Exception:
            pass
        return json_error(str(e), 400)
    except KbTokenCountGatewayError as e:
        try:
            stage_file.unlink(missing_ok=True)
        except Exception:
            pass
        return json_error(str(e), e.status_code)
    except RuntimeError as e:
        try:
            stage_file.unlink(missing_ok=True)
        except Exception:
            pass
        return json_error(str(e), 502)
    except Exception as e:
        try:
            stage_file.unlink(missing_ok=True)
        except Exception:
            pass
        return json_error(f"failed to count KB tokens: {e}", 500)

    return jsonify({"ok": True, "path": str(stage_file), "kb_token_count": kb_token_count, "model": model})


@prefix_bp.route("/kb/delete/<tenant>/<namespace>/<name>", methods=["POST"])
@require_login
def delete_knowledgebase_file(tenant, namespace, name):
    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    try:
        validate_function_name_or_raise(name)
    except ValueError as e:
        return json_error(str(e), 400)

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not has_admin_role_for_model(roles, tenant, namespace):
        return Response("No permission", status=403)

    stage_file = knowledgebase_stage_file_path(tenant, namespace, name)
    stage_dir = knowledgebase_root_dir_path(tenant, namespace, name)
    try:
        if stage_file.exists():
            stage_file.unlink()
        if stage_dir.exists():
            stage_dir.rmdir()
    except Exception as e:
        return json_error(f"failed to delete KB file: {e}", 500)

    return jsonify({"ok": True})


@prefix_bp.route("/kb/download/<tenant>/<namespace>/<name>", methods=["GET"])
@require_login
def DownloadKnowledgebaseFile(tenant, namespace, name):
    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    try:
        validate_function_name_or_raise(name)
    except ValueError as e:
        return json_error(str(e), 400)

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not user_can_access_dashboard_resource(roles, tenant, namespace):
        return Response("No permission", status=403)

    live_func = None
    try:
        func_resp, func_payload = getfunc_response(tenant, namespace, name)
        if func_resp.ok and isinstance(func_payload, dict):
            live_func = func_payload
    except Exception:
        live_func = None

    kb_file = resolve_knowledgebase_file_path(tenant, namespace, name, live_func=live_func)
    if kb_file is None:
        return json_error("knowledge base file not found", 404)

    return send_from_directory(
        str(kb_file.parent),
        kb_file.name,
        as_attachment=True,
        download_name=kb_file.name,
    )


@prefix_bp.route("/listfunctions", methods=["GET"])
@require_login
def ListFunctions():
    tenant = str(request.args.get("tenant", "") or "").strip()
    namespace = str(request.args.get("namespace", "") or "").strip()

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    is_inferx_admin = can_view_public_tenant(roles)
    can_manage_functions = user_can_manage_models(roles)

    if tenant != "" and not user_can_access_dashboard_resource(roles, tenant, namespace):
        return Response("No permission", status=403)

    try:
        rows = list_function_rows(tenant=tenant, namespace=namespace)
    except Exception as e:
        return json_error(str(e), 500)

    visible_rows = []
    for row in rows:
        if deny_public_tenant_request(row["tenant"]) is not None:
            continue
        if not user_can_access_dashboard_resource(roles, row["tenant"], row["namespace"]):
            continue

        live_func = None
        live_unavailable = True
        try:
            func_resp, func_payload = getfunc_response(row["tenant"], row["namespace"], row["name"])
            if func_resp.ok and isinstance(func_payload, dict):
                live_func = func_payload
                live_unavailable = False
            else:
                live_unavailable = True
        except Exception:
            live_unavailable = True

        detail = build_function_detail_view(row, live_func=live_func, live_unavailable=live_unavailable)
        state_label = detail["state"]
        if not live_unavailable and isinstance(live_func, dict):
            func_health_state = str((((live_func.get("func") or {}).get("object") or {}).get("status") or {}).get("state") or "").strip()
            state_label = infer_model_status(live_func.get("pods", []), func_health_state).get("label", state_label)
        visible_rows.append({
            "tenant": row["tenant"],
            "namespace": row["namespace"],
            "name": row["name"],
            "model_name": detail["row"]["model_name"],
            "token_length": detail["row"]["token_length"],
            "kb_token_count": detail["row"]["kb_token_count"],
            "total_token_length": detail["row"]["total_token_length"],
            "state": state_label,
            "gpu_count": detail["gpu_count"],
            "is_live_available": detail["is_live_available"],
            "can_manage": has_admin_role_for_model(roles, row["tenant"], row["namespace"]),
            "detail_href": dashboard_href(
                "prefix.FunctionDetail",
                tenant=row["tenant"],
                namespace=row["namespace"],
                name=row["name"],
            ),
        })

    return render_template(
        "function_list.html",
        functions=visible_rows,
        can_manage_functions=can_manage_functions,
    )


def render_function_create_page(*, error_message="", conflict_href="", preset_value="", target_tenant="", target_namespace="", function_name=""):
    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not user_can_manage_models(roles):
        return Response("No permission", status=403)

    try:
        deploy_target = build_catalog_deploy_target_selector_context(roles=roles, inferx_admin=is_inferx_admin_user())
        preset_options = resolve_function_preset_options()
    except RuntimeError as e:
        return json_error(str(e), 502)
    except Exception as e:
        return json_error(f"failed to load function presets: {e}", 500)

    return render_template(
        "function_create.html",
        preset_options=preset_options,
        deploy_target=deploy_target,
        error_message=str(error_message or "").strip(),
        conflict_href=str(conflict_href or "").strip(),
        preset_value=str(preset_value or "").strip(),
        function_name=str(function_name or "").strip(),
        target_tenant=str(target_tenant or "").strip(),
        target_namespace=str(target_namespace or "").strip(),
    )


@prefix_bp.route("/function_create", methods=["GET"])
@require_login
def FunctionCreate():
    return render_function_create_page()


@prefix_bp.route("/function_create", methods=["POST"])
@require_login
def FunctionCreatePost():
    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not user_can_manage_models(roles):
        return Response("No permission", status=403)

    preset = str(request.form.get("preset", "") or "").strip()
    function_name = str(request.form.get("name", "") or "").strip()
    requested_tenant = str(request.form.get("tenant", "") or "").strip()
    requested_namespace = str(request.form.get("namespace", "") or "").strip()

    try:
        deploy_target = build_catalog_deploy_target_selector_context(roles=roles, inferx_admin=is_inferx_admin_user())
        tenant, namespace = resolve_catalog_deploy_target_selection(
            deploy_target,
            requested_tenant=requested_tenant,
            requested_namespace=requested_namespace,
            allow_implicit=True,
        )
        validate_function_name_or_raise(function_name)
    except ValueError as e:
        return render_function_create_page(
            error_message=str(e),
            preset_value=preset,
            target_tenant=requested_tenant,
            target_namespace=requested_namespace,
            function_name=function_name,
        ), 400
    except Exception as e:
        return render_function_create_page(
            error_message=str(e),
            preset_value=preset,
            target_tenant=requested_tenant,
            target_namespace=requested_namespace,
            function_name=function_name,
        ), 500

    if not has_admin_role_for_model(roles, tenant, namespace):
        return Response("No permission", status=403)

    preset_option = find_function_preset_option(preset)
    if preset_option is None:
        error_message = diagnose_function_preset_unavailability(preset)
        return render_function_create_page(
            error_message=error_message,
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 400

    if find_function_row(tenant, namespace, function_name) is not None:
        conflict_href = dashboard_href("prefix.FunctionDetail", tenant=tenant, namespace=namespace, name=function_name)
        return render_function_create_page(
            error_message="That function name is already in use.",
            conflict_href=conflict_href,
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 409

    stage_file = knowledgebase_stage_file_path(tenant, namespace, function_name)
    if not stage_file.exists():
        return render_function_create_page(
            error_message="Upload a knowledge base file before creating the function.",
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 400

    try:
        kb_token_count = count_kb_tokens_via_gateway(
            tenant=tenant,
            namespace=namespace,
            name=function_name,
            model=preset_option["model_id"],
        )
    except ValueError as e:
        return render_function_create_page(
            error_message=str(e),
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 400
    except KbTokenCountGatewayError as e:
        return render_function_create_page(
            error_message=f"Failed to count KB tokens: {e}",
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), e.status_code
    except RuntimeError as e:
        return render_function_create_page(
            error_message=f"Failed to count KB tokens: {e}",
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 502
    except Exception as e:
        return render_function_create_page(
            error_message=f"Failed to count KB tokens: {e}",
            preset_value=preset,
            target_tenant=tenant,
            target_namespace=namespace,
            function_name=function_name,
        ), 500

    try:
        insert_function_row(
            tenant=tenant,
            namespace=namespace,
            name=function_name,
            preset=preset_option["preset"],
            catalog=preset_option["catalog"],
            token_length=preset_option["token_length"],
            kb_token_count=kb_token_count,
        )
    except psycopg2.Error as e:
        if getattr(e, "pgcode", "") == "23505":
            conflict_href = dashboard_href("prefix.FunctionDetail", tenant=tenant, namespace=namespace, name=function_name)
            return render_function_create_page(
                error_message="That function name is already in use.",
                conflict_href=conflict_href,
                preset_value=preset,
                target_tenant=tenant,
                target_namespace=namespace,
                function_name=function_name,
            ), 409
        return json_error(f"failed to create function row: {e}", 500)
    except Exception as e:
        return json_error(str(e), 500)

    try:
        entry = query_function_mapping_catalog_entry(preset_option["catalog_slug"])
        deploy_catalog_entry_to_function_target(
            entry,
            tenant=tenant,
            namespace=namespace,
            name=function_name,
        )
    except Exception as e:
        app.logger.warning(
            "function create deploy request failed tenant=%s namespace=%s name=%s: %s",
            tenant,
            namespace,
            function_name,
            e,
        )

    return redirect(dashboard_href("prefix.FunctionDetail", tenant=tenant, namespace=namespace, name=function_name))


@prefix_bp.route("/function", methods=["GET"])
@require_login
def FunctionDetail():
    tenant = str(request.args.get("tenant", "") or "").strip()
    namespace = str(request.args.get("namespace", "") or "").strip()
    name = str(request.args.get("name", "") or "").strip()

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not user_can_access_dashboard_resource(roles, tenant, namespace):
        return Response("No permission", status=403)

    is_inferx_admin = can_view_public_tenant(roles)
    can_manage = has_admin_role_for_model(roles, tenant, namespace) or is_inferx_admin

    try:
        function_row = query_function_row(tenant, namespace, name)
    except LookupError:
        return render_resource_unavailable_page(
            resource_kind="Function",
            resource_name=name,
            tenant=tenant,
            namespace=namespace,
            message="This function is no longer available in the dashboard.",
            suggestion="It may already have been cleaned up.",
            primary_href=dashboard_href("prefix.ListFunctions"),
            primary_label="Back to Functions",
            status=404,
        )
    except Exception as e:
        return json_error(str(e), 500)

    live_func = None
    live_unavailable = True
    try:
        func_resp, func_payload = getfunc_response(tenant, namespace, name)
        if func_resp.ok and isinstance(func_payload, dict):
            live_func = func_payload
            live_unavailable = False
    except Exception:
        live_unavailable = True

    function_detail = build_function_detail_view(function_row, live_func=live_func, live_unavailable=live_unavailable)

    funcspec = "{}"
    full_funcspec = "{}"
    func_edit_data = None
    api_type = "knowledgebase"
    map_body = {}
    path = ""
    funcpolicy = "{}"
    fails = []
    snapshotaudit = []
    initial_model_status = MODEL_STATUS_PENDING
    client_setup = {}
    opencode_download_href = ""
    if function_detail["is_live_available"]:
        try:
            sample = live_func["func"]["object"]["spec"]["sample_query"]
            map_body = sample["body"]
            api_type = sample["apiType"]
            path = sample["path"]
            full_funcspec = json.dumps(live_func["func"]["object"]["spec"], indent=4)
            func_edit_data = project_func_for_edit(live_func)
            funcspec = json.dumps(func_edit_data["spec"], indent=4)
            funcpolicy = live_func.get("policy", "{}")
            version = live_func["func"]["object"]["spec"]["version"]
            fails = GetFailLogs(tenant, namespace, name, version)
            snapshotaudit = GetSnapshotAudit(tenant, namespace, name, version)
            format_dashboard_timestamps(snapshotaudit, "updatetime")
            format_dashboard_timestamps(fails, "createtime")
            func_health_state = str((((live_func.get("func") or {}).get("object") or {}).get("status") or {}).get("state") or "").strip()
            initial_model_status = infer_model_status(live_func.get("pods", []), func_health_state, fails)
            onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(tenant)
            client_setup = build_client_setup_for_ui(
                tenant=tenant,
                namespace=namespace,
                funcname=name,
                sample_query=sample,
                apikey=onboarding_apikey,
                apikey_name=onboarding_apikey_name,
                spec=((live_func.get("func") or {}).get("object") or {}).get("spec"),
            )
            sample_rest_call_for_ui = build_sample_rest_call_for_ui(
                tenant=tenant,
                namespace=namespace,
                funcname=name,
                sample_query=sample,
                apikey=onboarding_apikey,
            )
            if sample_rest_call_for_ui != "":
                live_func["sampleRestCall"] = sample_rest_call_for_ui
                live_func["sampleRestCallDisplay"] = mask_sample_rest_call_for_ui(
                    sample_rest_call_for_ui,
                    onboarding_apikey,
                )
            if session.get('access_token', '') != '' and client_setup.get("api_base_url"):
                opencode_download_href = url_for(
                    "prefix.DownloadModelOpenCodeConfig",
                    tenant=tenant,
                    namespace=namespace,
                    name=name,
                )
        except Exception:
            pass

    return render_template(
        "func.html",
        tenant=tenant,
        namespace=namespace,
        name=name,
        func=live_func,
        fails=fails,
        snapshotaudit=snapshotaudit,
        funcspec=funcspec,
        full_funcspec=full_funcspec,
        func_edit_data=func_edit_data,
        apiType=api_type,
        map=map_body,
        isAdmin=True,
        is_inferx_admin=is_inferx_admin,
        funcpolicy=funcpolicy,
        path=path,
        initial_model_status=initial_model_status,
        client_setup=client_setup,
        opencode_download_href=opencode_download_href,
        func_admin_href="",
        func_user_href="",
        func_edit_href="",
        tenant_models_href=dashboard_href("prefix.ListFunctions", tenant=tenant),
        models_list_href=dashboard_href("prefix.ListFunctions", tenant=tenant, namespace=namespace),
        is_function=True,
        function_detail=function_detail,
        can_manage=can_manage,
    )


@prefix_bp.route("/function/delete/<tenant>/<namespace>/<name>", methods=["POST"])
@require_login
def FunctionDelete(tenant, namespace, name):
    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    try:
        roles = listroles()
    except Exception as e:
        return json_error(f"failed to load roles: {e}", 502)

    if not has_admin_role_for_model(roles, tenant, namespace):
        return Response("No permission", status=403)

    result = delete_function_via_dashboard(tenant, namespace, name)
    if not result["ok"]:
        return json_error(result["message"], result["status"])

    return redirect(dashboard_href("prefix.ListFunctions"))


@prefix_bp.route("/function/cleanup/<tenant>/<namespace>/<name>", methods=["POST"])
@require_login
def FunctionCleanup(tenant, namespace, name):
    return FunctionDelete(tenant, namespace, name)


@prefix_bp.route("/")
@not_require_login
def Home():
    if session.get('access_token', '') != '':
        return redirect(url_for('prefix.EndpointList'))
    return redirect(url_for('prefix.EndpointList'))


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

    try:
        function_keys = {
            (str(r.get('tenant', '')), str(r.get('namespace', '')), str(r.get('name', '')))
            for r in list_function_rows()
        }
    except Exception:
        function_keys = set()
    funcs = [
        f for f in funcs
        if (
            str((f.get('func') or {}).get('tenant', '')),
            str((f.get('func') or {}).get('namespace', '')),
            str((f.get('func') or {}).get('name', '')),
        ) not in function_keys
    ]

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
    

    can_manage_models = user_can_manage_models(roles)
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
    view = request.args.get("view", "").strip().lower()

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
    func_health_state = str((((func.get("func") or {}).get("object") or {}).get("status") or {}).get("state") or "").strip()

    version = func["func"]["object"]["spec"]["version"]
    funcpolicy = func["policy"]
    fails = GetFailLogs(tenant, namespace, name, version)
    snapshotaudit = GetSnapshotAudit(tenant, namespace, name, version)
    format_dashboard_timestamps(snapshotaudit, "updatetime")
    format_dashboard_timestamps(fails, "createtime")

    # Show the same filtered partial spec used by the new create/edit flow.
    full_funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)
    try:
        func_edit_data = project_func_for_edit(func)
        funcspec = json.dumps(func_edit_data["spec"], indent=4)
    except Exception:
        # Fallback for unexpected legacy shapes so the page still renders.
        func_edit_data = None
        funcspec = json.dumps(func["func"]["object"]["spec"], indent=4)

    onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(tenant)
    client_setup = build_client_setup_for_ui(
        tenant=tenant,
        namespace=namespace,
        funcname=name,
        sample_query=sample,
        apikey=onboarding_apikey,
        apikey_name=onboarding_apikey_name,
        spec=((func.get("func") or {}).get("object") or {}).get("spec"),
    )
    sample_rest_call_for_ui = build_sample_rest_call_for_ui(
        tenant=tenant,
        namespace=namespace,
        funcname=name,
        sample_query=sample,
        apikey=onboarding_apikey,
    )
    if sample_rest_call_for_ui != "":
        func["sampleRestCall"] = sample_rest_call_for_ui
        func["sampleRestCallDisplay"] = mask_sample_rest_call_for_ui(
            sample_rest_call_for_ui,
            onboarding_apikey,
        )

    is_inferx_admin = is_inferx_admin_user()
    initial_model_status = infer_model_status(func.get("pods", []), func_health_state, fails)
    func_admin_href = dashboard_href("prefix.GetFunc", tenant=tenant, namespace=namespace, name=name, view="admin")
    func_user_href = dashboard_href("prefix.GetFunc", tenant=tenant, namespace=namespace, name=name)
    func_edit_href = "{}?{}".format(
        dashboard_href("prefix.FuncCreate"),
        urlencode({"edit": f"{tenant}/{namespace}/{name}"}),
    )
    tenant_models_href = dashboard_href("prefix.ListFunc", tenant=tenant)
    models_list_href = dashboard_href("prefix.ListFunc", tenant=tenant, namespace=namespace)
    template_name = "func.html" if is_inferx_admin and view == "admin" else "func_user.html"

    return render_template(
        template_name,
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
        path=sample["path"],
        initial_model_status=initial_model_status,
        client_setup=client_setup,
        opencode_download_href=(
            url_for(
                "prefix.DownloadModelOpenCodeConfig",
                tenant=tenant,
                namespace=namespace,
                name=name,
            )
            if session.get('access_token', '') != ''
            else ""
        ),
        func_admin_href=func_admin_href,
        func_user_href=func_user_href,
        func_edit_href=func_edit_href,
        tenant_models_href=tenant_models_href,
        models_list_href=models_list_href,
        is_function=False,
        function_detail=None,
    )


@prefix_bp.route("/integration/opencode/models/<tenant>/<namespace>/<name>.json", methods=["GET"])
@require_login
def DownloadModelOpenCodeConfig(tenant, namespace, name):
    try:
        func_resp, func = getfunc_response(tenant, namespace, name)
        if is_upstream_resource_unavailable(func_resp, func):
            return json_error(f"model `{tenant}/{namespace}/{name}` is not available", 404)
        if not func_resp.ok or func is None:
            return json_error(
                extract_upstream_error_message(func_resp, func) or f"failed to load model `{tenant}/{namespace}/{name}`",
                func_resp.status_code if func_resp is not None else 502,
            )

        deny_resp = deny_public_tenant_request(resource_item_tenant_name(func))
        if deny_resp is not None:
            return deny_resp

        spec = (((func.get("func") or {}).get("object") or {}).get("spec") or {})
        sample_query = spec.get("sample_query")
        if not isinstance(sample_query, dict):
            return json_error("This model does not expose integration metadata.", 400)

        onboarding_apikey, onboarding_apikey_name = resolve_onboarding_inference_apikey_for_ui(tenant)
        client_setup = build_client_setup_for_ui(
            tenant=tenant,
            namespace=namespace,
            funcname=name,
            sample_query=sample_query,
            apikey=onboarding_apikey,
            apikey_name=onboarding_apikey_name,
            spec=spec,
        )
        api_base_url = str(client_setup.get("api_base_url", "") or "").strip()
        model_name = str(client_setup.get("model_name", "") or "").strip()
        api_key = str(client_setup.get("api_key", "") or "").strip()
        if api_base_url == "" or model_name == "" or api_key == "":
            return json_error("OpenCode config is unavailable for this model.", 400)

        limits = resolve_opencode_limit_split(spec=spec)
        return build_opencode_download_response(
            filename_stem=f"inferx-opencode-model-{tenant}-{namespace}-{name}",
            api_base_url=api_base_url,
            api_key=api_key,
            model_name=model_name,
            context_length=limits["context"],
            output_length=limits["output"],
        )
    except Exception as e:
        return json_error(f"failed to build OpenCode config for model `{tenant}/{namespace}/{name}`: {e}", 500)


@prefix_bp.route("/faillogs")
@require_login_unless_gateway_aligned_anonymous_enabled
def GetFailLogsJson():
    tenant = request.args.get("tenant")
    namespace = request.args.get("namespace")
    name = request.args.get("name")
    version = request.args.get("version")

    for field_name, field_value in (
        ("tenant", tenant),
        ("namespace", namespace),
        ("name", name),
        ("version", version),
    ):
        if str(field_value or "").strip() == "":
            return json_error(f"Missing required query param: `{field_name}`", 400)

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    fails = GetFailLogs(tenant, namespace, name, version)
    format_dashboard_timestamps(fails, "createtime")
    return jsonify(fails)

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
            resource_kind="Instance",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            message="This instance is no longer available in the dashboard.",
            suggestion="It may have completed, been garbage collected, or belonged to a snapshot that is no longer active.",
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Instances",
            secondary_href=snapshot_secondary_href,
            secondary_label=snapshot_secondary_label,
            detail=extract_upstream_error_message(pod_resp, pod),
            status=404,
        )
    if not pod_resp.ok:
        return render_resource_load_error_page(
            resource_kind="Instance",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Instances",
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
            resource_kind="Instance",
            resource_name=podname,
            tenant=tenant,
            namespace=namespace,
            message="This instance is no longer available in the dashboard.",
            suggestion="It may have completed, been garbage collected, or belonged to a snapshot that is no longer active.",
            primary_href=dashboard_href("prefix.ListPod", tenant=tenant, namespace=namespace),
            primary_label="Back to Instances",
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
    funcname = request.args.get("funcname") or name
    version = request.args.get("version")
    id = request.args.get("id")

    deny_resp = deny_public_tenant_request(tenant)
    if deny_resp is not None:
        return deny_resp

    log = GetFailLog(tenant, namespace, name, version, id)

    audits = getpodaudit(tenant, namespace, name, version, id)
    format_dashboard_timestamps(audits, "updatetime")
    return render_template(
        "pod.html",
        tenant=tenant,
        namespace=namespace,
        podname=name,
        funcname=funcname,
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
