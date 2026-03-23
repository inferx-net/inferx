#!/usr/bin/env python3

import argparse
import json
from pathlib import Path
from typing import Iterable, List

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG_DIR = REPO_ROOT / "config"
DEFAULT_HF_CACHE_HOSTPATH = "/opt/inferx/cache"
dashboard_app = None


def load_dashboard_app():
    try:
        import app as dashboard_app_module
    except ModuleNotFoundError as err:
        raise SystemExit(
            "dashboard app dependencies are missing. Run the importer from the dashboard environment "
            "(for example after `pip install -r dashboard/requirements.txt`). "
            f"Missing module: {err.name}"
        ) from err
    return dashboard_app_module


def parse_args():
    parser = argparse.ArgumentParser(
        description="Import one or more config/*.json model specs into CatalogModel."
    )
    parser.add_argument(
        "config_paths",
        nargs="+",
        help="Path(s) to config JSON files. Relative paths are resolved from the current working directory.",
    )
    parser.add_argument(
        "--display-name",
        help="Override display_name for single-file imports. Defaults to the config name.",
    )
    parser.add_argument(
        "--provider",
        help="Override provider. Defaults to config namespace or the source model prefix.",
    )
    parser.add_argument(
        "--modality",
        choices=["text", "image", "audio", "multimodal"],
        help="Override modality. Defaults from sample_query.apiType.",
    )
    parser.add_argument(
        "--brief-intro",
        help="Override brief_intro. Defaults to an importer-generated placeholder.",
    )
    parser.add_argument(
        "--detailed-intro",
        help="Override detailed_intro. Defaults to the brief intro.",
    )
    parser.add_argument(
        "--source-kind",
        default="huggingface",
        help="Catalog source_kind. Default: huggingface.",
    )
    parser.add_argument(
        "--source-model-id",
        help="Override source_model_id for single-file imports. Defaults from --model in commands.",
    )
    parser.add_argument(
        "--parameter-count-b",
        type=float,
        help="Optional parameter_count_b override for single-file imports.",
    )
    parser.add_argument(
        "--is-moe",
        action="store_true",
        help="Mark imported entries as Mixture-of-Experts models.",
    )
    parser.add_argument(
        "--tag",
        action="append",
        default=[],
        help="Repeatable tag. Can be used multiple times.",
    )
    parser.add_argument(
        "--use-case",
        action="append",
        default=[],
        help="Repeatable recommended use case. Can be used multiple times.",
    )
    parser.add_argument(
        "--image",
        help="Override spec.image before validation and import.",
    )
    parser.add_argument(
        "--hf-cache-hostpath",
        default=DEFAULT_HF_CACHE_HOSTPATH,
        help="Host path to use when adding the standard HuggingFace cache mount. Default: /opt/inferx/cache.",
    )
    parser.add_argument(
        "--skip-hf-offline-normalization",
        action="store_true",
        help="Do not inject the standard HuggingFace offline envs/cache mount before validation.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate and print the derived payload without writing to the catalog DB.",
    )
    parser.add_argument(
        "--create-only",
        action="store_true",
        help="Require create mode and reject explicit update targets (`--catalog-id` or `--slug`).",
    )
    parser.add_argument(
        "--update-only",
        action="store_true",
        help="Require an explicit update target (`--catalog-id` or `--slug`) and fail if it does not exist.",
    )
    parser.add_argument(
        "--catalog-id",
        type=int,
        help="Update target catalog entry id for single-file imports.",
    )
    parser.add_argument(
        "--slug",
        help="Update target catalog slug for single-file imports.",
    )
    return parser.parse_args()


def split_csv_values(values: Iterable[str]) -> List[str]:
    normalized = []
    seen = set()
    for raw in values:
        for item in str(raw or "").split(","):
            value = item.strip()
            if value == "" or value in seen:
                continue
            seen.add(value)
            normalized.append(value)
    return normalized


def resolve_config_path(raw_path: str) -> Path:
    path = Path(raw_path)
    if path.is_absolute():
        return path
    if path.exists():
        return path.resolve()
    candidate = (DEFAULT_CONFIG_DIR / raw_path).resolve()
    if candidate.exists():
        return candidate
    return path.resolve()


def load_config_spec(config_path: Path):
    with config_path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)

    if not isinstance(data, dict):
        raise ValueError(f"{config_path} must be a JSON object")

    spec = (
        (((data.get("object") or {}) if isinstance(data.get("object"), dict) else {}).get("spec"))
        if isinstance(data.get("object"), dict)
        else None
    )
    if not isinstance(spec, dict):
        raise ValueError(f"{config_path} is missing object.spec")

    return data, dashboard_app.clone_json_value(spec)


def infer_source_model_id(spec: dict) -> str:
    commands = spec.get("commands")
    model_values = dashboard_app.find_model_command_values(commands)
    if len(model_values) != 1 or str(model_values[0] or "").strip() == "":
        raise ValueError("config spec must contain exactly one valid `--model` command argument")
    return str(model_values[0]).strip()


def infer_provider(config_data: dict, source_model_id: str) -> str:
    namespace = str(config_data.get("namespace") or "").strip()
    if namespace != "":
        return namespace
    source_prefix = str(source_model_id or "").split("/", 1)[0].strip()
    if source_prefix != "":
        return source_prefix
    return "Unknown"


def infer_modality(spec: dict) -> str:
    sample_query = spec.get("sample_query") if isinstance(spec.get("sample_query"), dict) else {}
    api_type = dashboard_app.normalize_catalog_api_type(sample_query.get("apiType"))
    if api_type == "text2text":
        return "text"
    if api_type == "text2img":
        return "image"
    if api_type == "text2audio":
        return "audio"
    if api_type in ("image2text", "audio2text"):
        return "multimodal"
    return "text"


def normalize_huggingface_offline_spec(spec: dict, *, hf_cache_hostpath: str):
    envs = spec.get("envs")
    if not isinstance(envs, list):
        envs = []

    ordered_envs = []
    seen_env_keys = set()
    for pair in envs:
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        key, value = pair
        if not isinstance(key, str) or not isinstance(value, str):
            continue
        normalized_key = key.strip()
        if normalized_key == "" or normalized_key in seen_env_keys:
            continue
        seen_env_keys.add(normalized_key)
        ordered_envs.append([normalized_key, value])

    for key, value in dashboard_app.DEFAULT_MODEL_ENVS:
        if key not in seen_env_keys:
            ordered_envs.append([key, value])
            seen_env_keys.add(key)

    for key in sorted(dashboard_app.CATALOG_RESERVED_ENV_KEYS):
        if key not in seen_env_keys:
            ordered_envs.append([key, "1"])
            seen_env_keys.add(key)

    mounts = spec.get("mounts")
    if not isinstance(mounts, list):
        mounts = []

    normalized_mounts = []
    has_hf_cache_mount = False
    for mount in mounts:
        if not isinstance(mount, dict):
            continue
        hostpath = str(mount.get("hostpath") or "").strip()
        mountpath = str(mount.get("mountpath") or "").strip()
        if hostpath == "" or mountpath == "":
            continue
        normalized_mounts.append({"hostpath": hostpath, "mountpath": mountpath})
        if mountpath == dashboard_app.CATALOG_HF_CACHE_MOUNTPATH:
            has_hf_cache_mount = True

    if not has_hf_cache_mount:
        normalized_mounts.append(
            {
                "hostpath": hf_cache_hostpath,
                "mountpath": dashboard_app.CATALOG_HF_CACHE_MOUNTPATH,
            }
        )

    spec["envs"] = ordered_envs
    spec["mounts"] = normalized_mounts


def build_import_payload(config_path: Path, args, *, existing_entry=None):
    config_data, spec = load_config_spec(config_path)

    if args.image:
        spec["image"] = args.image.strip()

    source_model_id = (
        args.source_model_id.strip()
        if args.source_model_id and len(args.config_paths) == 1
        else infer_source_model_id(spec)
    )
    source_kind = str(args.source_kind or "huggingface").strip()

    if source_kind == "huggingface" and not args.skip_hf_offline_normalization:
        normalize_huggingface_offline_spec(
            spec,
            hf_cache_hostpath=str(args.hf_cache_hostpath or DEFAULT_HF_CACHE_HOSTPATH).strip(),
        )

    display_name = (
        args.display_name.strip()
        if args.display_name and len(args.config_paths) == 1
        else str(config_data.get("name") or config_path.stem).strip()
    )
    provider = (
        args.provider.strip()
        if args.provider and len(args.config_paths) == 1
        else infer_provider(config_data, source_model_id)
    )
    modality = (
        args.modality.strip()
        if args.modality and len(args.config_paths) == 1
        else infer_modality(spec)
    )
    brief_intro = (
        args.brief_intro.strip()
        if args.brief_intro and len(args.config_paths) == 1
        else f"{display_name} imported from {config_path.name}. Review intro, tags, and use cases before publishing broadly."
    )
    detailed_intro = (
        args.detailed_intro.strip()
        if args.detailed_intro and len(args.config_paths) == 1
        else brief_intro
    )

    payload = {
        "display_name": display_name,
        "provider": provider,
        "modality": modality,
        "brief_intro": brief_intro,
        "detailed_intro": detailed_intro,
        "source_kind": source_kind,
        "source_model_id": source_model_id,
        "parameter_count_b": (
            float(args.parameter_count_b)
            if args.parameter_count_b is not None and len(args.config_paths) == 1
            else None
        ),
        "is_moe": bool(args.is_moe),
        "tags": split_csv_values(args.tag),
        "recommended_use_cases": split_csv_values(args.use_case),
        "default_func_spec": spec,
    }

    return dashboard_app.normalize_catalog_admin_payload(payload, existing_entry=existing_entry)


def format_summary(entry: dict) -> str:
    return (
        f"id={entry.get('id')} "
        f"slug={entry.get('slug')} "
        f"source_model_id={entry.get('source_model_id')} "
        f"catalog_version={entry.get('catalog_version')}"
    )


def main():
    args = parse_args()
    global dashboard_app
    dashboard_app = load_dashboard_app()
    if args.create_only and args.update_only:
        raise SystemExit("--create-only and --update-only cannot be used together")
    if args.catalog_id is not None and args.slug:
        raise SystemExit("--catalog-id and --slug cannot be used together")
    if args.create_only and (args.catalog_id is not None or args.slug):
        raise SystemExit("--create-only cannot be used with --catalog-id or --slug")
    if args.update_only and args.catalog_id is None and not args.slug:
        raise SystemExit("--update-only requires --catalog-id or --slug")
    if len(args.config_paths) > 1 and any(
        value is not None and value != []
        for value in (
            args.display_name,
            args.provider,
            args.modality,
            args.brief_intro,
            args.detailed_intro,
            args.source_model_id,
            args.parameter_count_b,
            args.image,
            args.catalog_id,
            args.slug,
        )
    ):
        raise SystemExit("single-entry override flags and explicit update targets can only be used when importing exactly one config file")

    results = []
    update_target_entry = None
    if args.catalog_id is not None:
        try:
            update_target_entry = dashboard_app.query_catalog_entry_by_id(int(args.catalog_id), active_only=False)
        except LookupError:
            raise SystemExit(f"catalog entry {args.catalog_id} does not exist")
        except RuntimeError as err:
            raise SystemExit(str(err))
    elif args.slug:
        normalized_slug = str(args.slug or "").strip()
        if normalized_slug == "":
            raise SystemExit("--slug must be a non-empty string")
        try:
            update_target_entry = dashboard_app.query_catalog_entry_by_slug(normalized_slug, active_only=False)
        except LookupError:
            raise SystemExit(f"catalog entry `{normalized_slug}` does not exist")
        except RuntimeError as err:
            raise SystemExit(str(err))

    for raw_path in args.config_paths:
        config_path = resolve_config_path(raw_path)
        if not config_path.exists():
            raise SystemExit(f"config file not found: {config_path}")

        existing_entry = update_target_entry
        payload = build_import_payload(config_path, args, existing_entry=existing_entry)

        if args.dry_run:
            action = "update" if existing_entry is not None else "create"
            print(json.dumps({"action": action, "payload": payload}, indent=2, sort_keys=True))
            continue

        if existing_entry is not None:
            entry = dashboard_app.update_catalog_entry(existing_entry["id"], payload)
            action = "updated"
        else:
            entry = dashboard_app.insert_catalog_entry(payload)
            action = "created"
        results.append((action, entry))

    if args.dry_run:
        return

    for action, entry in results:
        print(f"{action}: {format_summary(entry)}")


if __name__ == "__main__":
    main()
