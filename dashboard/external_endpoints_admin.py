"""Admin subpage for external OpenAI-compatible endpoints — external-specific config ONLY.

Registered from app.py via `register_external_endpoints_admin(prefix_bp, ...)` so the
shared helpers (auth decorators, gateway headers) are injected rather than imported,
avoiding a circular import.

Scope is deliberately narrow: this page only creates/edits/deletes the
`ExternalEndpoint` row (slug, base_url, upstream_model, provider_api_key). It is NOT
the home for publish / catalog metadata / OpenRouter state. Once an external endpoint
exists, operators manage its publish state, catalog metadata, and OpenRouter listing
from the *shared* endpoint admin pages (which are kind-aware), exactly like a
self-hosted model. See the "keep a small External Endpoints page" decision in
docs/external-endpoint-gateway.md.
"""

import requests
from flask import jsonify, render_template, request


def register_external_endpoints_admin(
    prefix_bp,
    *,
    apihostaddr,
    gateway_request_headers,
    response_json_or_none,
    json_error,
    require_login,
    require_admin,
):
    def gw_external(method, subpath, json_body=None):
        url = f"{apihostaddr}/admin/external-endpoints{subpath}"
        resp = requests.request(
            method,
            url,
            headers=gateway_request_headers(json_body=json_body is not None),
            json=json_body,
            timeout=60,
        )
        body = (resp.text or "").strip()
        if not resp.ok:
            raise RuntimeError(body or f"HTTP {resp.status_code}")
        return response_json_or_none(resp)

    @require_login
    @require_admin
    def page():
        return render_template("admin_external_endpoints.html")

    @require_login
    @require_admin
    def list_data():
        try:
            data = gw_external("GET", "/")
        except Exception as e:
            return json_error(f"failed to list external endpoints: {e}", 502)
        return jsonify(data if isinstance(data, list) else [])

    @require_login
    @require_admin
    def create():
        req = request.get_json(silent=True) or {}
        try:
            view = gw_external("POST", "/", json_body=req)
        except Exception as e:
            return json_error(f"failed to create external endpoint: {e}", 502)
        return jsonify(view or {})

    @require_login
    @require_admin
    def item(slug):
        try:
            if request.method == "GET":
                return jsonify(gw_external("GET", f"/{slug}") or {})
            if request.method == "PUT":
                req = request.get_json(silent=True) or {}
                return jsonify(gw_external("PUT", f"/{slug}", json_body=req) or {})
            return jsonify(gw_external("DELETE", f"/{slug}") or {})
        except Exception as e:
            return json_error(f"external endpoint op failed: {e}", 502)

    rules = [
        ("/admin/external-endpoints", page, ["GET"], "admin_external_endpoints_page"),
        ("/admin/external-endpoints/list", list_data, ["GET"], "admin_external_endpoints_list"),
        ("/admin/external-endpoints/create", create, ["POST"], "admin_external_endpoints_create"),
        ("/admin/external-endpoints/item/<slug>", item, ["GET", "PUT", "DELETE"], "admin_external_endpoints_item"),
    ]
    for path, view, methods, endpoint in rules:
        prefix_bp.add_url_rule(path, view_func=view, methods=methods, endpoint=endpoint)
