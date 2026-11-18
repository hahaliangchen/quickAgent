#!/usr/bin/env python3
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

from customer_templates import prune_unicom_templates
from customer_logging import CustomerLog, start_collector, stream_output

APP = Path("/opt/customer")
STATE = Path("/state")
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
RUNTIME_LOG = None


def start_logged(command, service, cwd=None):
    process = subprocess.Popen(command, cwd=cwd, start_new_session=True,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    thread = threading.Thread(target=stream_output, args=(process.stdout, service, RUNTIME_LOG), daemon=True)
    thread.start()
    RUNTIME_LOG.write(service, "started", pid=process.pid)
    return process, thread


def gateway_request():
    credentials = base64.b64encode(("admin:" + os.environ["CUSTOMER_ADMIN_PASSWORD"]).encode()).decode()
    return urllib.request.Request("http://127.0.0.1:3002/agent/settings/data",
                                  headers={"Authorization": "Basic " + credentials})


def ppt_registration_request():
    request = gateway_request()
    return urllib.request.Request("http://127.0.0.1:3002/agent/skill-packages", headers=dict(request.header_items()))


def ppt_registered():
    path = STATE / "agent-config/skill-packages.json"
    if path.is_file() and "ppt-master" in json.loads(path.read_text()).get("removed", []):
        return True
    with HTTP.open(ppt_registration_request(), timeout=3) as response:
        packages = json.load(response)["packages"]
    return any(p.get("id") == "ppt-master" and p.get("remote") and p.get("library") == "ppt"
               and p.get("has_skill") and p.get("tools") for p in packages)


def check_health():
    for target in ["http://127.0.0.1:8899/prod-api/health/filing-worker",
                   "http://127.0.0.1:3000/health", gateway_request()]:
        with HTTP.open(target, timeout=3) as response:
            if response.status != 200:
                raise RuntimeError("Customer service is not ready")
    if not ppt_registered():
        raise RuntimeError("PPT plugin has not registered its skill and ppt library")
    return 0


def ensure_ppt_registration(profile):
    path = profile / "skill-packages.json"
    config = json.loads(path.read_text()) if path.exists() else {}
    # Preserve an explicit removal, disabled plugin, custom URL and credentials.
    if "ppt-master" in config.get("remotes", {}) or "ppt-master" in config.get("removed", []):
        return
    config.setdefault("remotes", {})["ppt-master"] = {
        "url": "http://127.0.0.1:8899/prod-api/aiPpt/plugin", "token": ""}
    temporary = path.with_name(path.name + ".upgrade-tmp")
    temporary.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n")
    temporary.chmod(0o600)
    temporary.replace(path)


def upgrade_customer_defaults(profile):
    policy_path = profile / "customer-policy.json"
    generation_path = profile / "agent-generation.yml"
    if not policy_path.is_file() or not generation_path.is_file():
        return
    policy = json.loads(policy_path.read_text())
    if policy.get("customer_custom_models") or policy.get("provider") != "deepseek":
        return
    # Old releases wrote these exact factory defaults. Migrate them once;
    # independently customized model choices and allowlists remain untouched.
    import yaml
    generation = yaml.safe_load(generation_path.read_text()) or {}
    changed = False
    builtin_models = {"deepseek-flash", "deepseek-v4-pro"}
    allowed = generation.get("plugin_models", {}).get("ppt-master")
    if isinstance(allowed, list) and len(allowed) == 2 and all(
        isinstance(row, dict) and row.get("provider") == "deepseek" for row in allowed
    ) and {row.get("model") for row in allowed} == builtin_models:
        del generation["plugin_models"]["ppt-master"]
        changed = True
    if generation.get("visual_tool") == {"provider": "deepseek", "model": "deepseek-flash", "reasoning_effort": "high"}:
        generation["visual_tool"] = {}
        changed = True
    if changed:
        temporary = generation_path.with_name(generation_path.name + ".upgrade-tmp")
        temporary.write_text(json.dumps(generation, ensure_ascii=False, indent=2) + "\n")
        temporary.chmod(0o600)
        temporary.replace(generation_path)
    policy["customer_custom_models"] = True
    temporary = policy_path.with_name(policy_path.name + ".upgrade-tmp")
    temporary.write_text(json.dumps(policy, ensure_ascii=False, indent=2) + "\n")
    temporary.chmod(0o600)
    temporary.replace(policy_path)


def write_config():
    STATE.mkdir(parents=True, exist_ok=True)
    for folder in ["logs", "workspace", "server", "server/projects", "server/uploads/file"]:
        (STATE / folder).mkdir(parents=True, exist_ok=True)
    resources = STATE / "server/resource"
    if not resources.exists():
        shutil.copytree(APP / "server/resource", resources)
    removed = prune_unicom_templates(resources / "ai_skills/ppt-master")
    if removed:
        print("Removed bundled Unicom templates: " + ", ".join(removed), flush=True)
    profile = STATE / "agent-config"
    if not profile.exists():
        shutil.copytree(APP / "config/agent", profile)
        profile.chmod(0o700)
    # The bundled profile is sanitized during assembly. Persisted profiles belong
    # to the customer; let the Agent validate and load their selected providers.
    upgrade_customer_defaults(profile)
    ensure_ppt_registration(profile)
    config = {
        "system": {"db-type":"mysql", "oss-type":"local", "addr":8899, "router-prefix":"/prod-api", "web-root":str(APP / "server/web"),
                   "enable-api-rbac":True, "use-redis":True, "use-mongo":False, "use-multipoint":False, "disable-auto-migrate":False,
                   "iplimit-count":1000, "iplimit-time":3600, "password-salt":"ai-ppt-login-salt-v1"},
        "jwt": {"signing-key":os.environ["JWT_SIGNING_KEY"], "expires-time":"24h", "buffer-time":"1h", "issuer":"PPT Customer"},
        "mysql": {"path":os.getenv("MYSQL_HOST","db"), "port":"3306", "config":"charset=utf8mb4&parseTime=True&loc=Local", "db-name":os.getenv("MYSQL_DATABASE","ai_ppt"),
                  "username":os.getenv("MYSQL_USER","ai_ppt"), "password":os.environ["MYSQL_PASSWORD"], "max-idle-conns":5, "max-open-conns":20, "log-mode":"error", "log-zap":False},
        "redis": {"addr":os.getenv("REDIS_ADDR","redis:6379"), "password":os.getenv("REDIS_PASSWORD",""), "db":0},
        "zap": {"level":"info", "format":"console", "director":str(STATE / "logs"), "encode-level":"LowercaseLevelEncoder", "stacktrace-key":"stacktrace", "log-in-console":True, "retention-day":7},
        "local": {"path":"uploads/file", "store-path":"uploads/file"},
        "captcha": {"key-long":6, "img-width":240, "img-height":80, "open-captcha":100, "open-captcha-timeout":3600},
        "agent": {"ai-provider":"", "skill-resource-dir":"resource/ai_skills", "project-output-dir":"projects", "session-ttl":"72h", "session-key-prefix":"ppt-customer:", "max-consecutive-failures":3, "max-history-string-len":16384},
        "deepseek": {"api-key":"", "model":"", "base-url":""},
        "workbench": {"data-file":str(STATE / "server/workbench-data.json"), "model-provider":"", "llm-model":"", "llm-base-url":"", "llm-api-key":"", "enable-heuristic-fallback":False},
        "onlyoffice": {
            "version": "1.15.0",
            "server-address": os.getenv("ONLYOFFICE_SERVER_ADDRESS", ""),
            "doc-server-host": os.getenv("ONLYOFFICE_DOC_SERVER_HOST", ""),
            "doc-server-api-url": "web-apps/apps/api/documents/api.js",
            "doc-server-preloader-url": "web-apps/apps/api/documents/api.js",
            "workbench-api-base-url": os.getenv("ONLYOFFICE_WORKBENCH_API_BASE_URL", ""),
            "plugin-base-url": os.getenv("ONLYOFFICE_PLUGIN_BASE_URL", ""),
            "editor-entry-url": "/onlyoffice-editor",
            "direct-download-enabled": True,
            "save-error-fallback-enabled": False,
            "jwt-secret": os.getenv("ONLYOFFICE_JWT_SECRET", ""),
            "open-ticket-expire-seconds": 300,
            "jwt-is-enabled": os.getenv("ONLYOFFICE_JWT_ENABLED", "true").strip().lower() in {"1", "true", "yes", "on"},
            "storage-path": "filestore",
        },
        "yalongjiang": {"enabled":False, "native-login-enabled":True},
    }
    config_path = STATE / "server-config.yaml"
    config_path.write_text(json.dumps(config, ensure_ascii=False, indent=2) + "\n")
    config_path.chmod(0o600)
    os.environ.update({"GIN_MODE":"release", "GVA_CONFIG":str(config_path), "GROW_CONFIG_DIR":str(profile),
        "DSH_HOME":str(STATE / "no-legacy-profile"), "WORKSPACE_ROOT":str(STATE / "workspace"),
        "AGENT_BIND":"127.0.0.1:3001", "MCP_BIND":"127.0.0.1:3000", "AGENT_WEB_DIR":str(APP / "agent-web"),
        "GROW_RESOURCES_DIR":str(APP / "agent-resources"), "PPT_AGENT_URL":"http://127.0.0.1:3001",
        "PPT_RENDER_URL":os.getenv("PPT_RENDER_URL","http://renderer:8787")})
    # Installed code must not be shadowed by an older mounted resource tree.
    # Mutable customer templates and projects remain under /state.
    os.environ["PPT_BUNDLED_SKILL_DIR"] = str(APP / "server/resource/ai_skills/ppt-master")


def _settings_request(payload=None):
    headers = {} if payload is None else {"Content-Type": "application/json"}
    data = None if payload is None else json.dumps(payload, ensure_ascii=False).encode()
    return urllib.request.Request("http://127.0.0.1:3001/agent/settings/data",
                                  data=data, headers=headers,
                                  method="GET" if payload is None else "PUT")


def _normalize_models_yaml_providers(raw_providers):
    if isinstance(raw_providers, dict):
        items = [{"id": key, **(value or {})} for key, value in raw_providers.items()]
    elif isinstance(raw_providers, list):
        items = raw_providers
    else:
        raise ValueError("models.yaml: 'providers' must be a list or mapping")
    normalized = []
    for row in items:
        if not isinstance(row, dict):
            raise ValueError("models.yaml: each provider entry must be a mapping")
        pid = str(row.get("id") or "").strip()
        if not pid:
            raise ValueError("models.yaml: provider 'id' is required")
        url = str(row.get("url") or row.get("base_url") or row.get("baseURL") or "").strip()
        if not url:
            raise ValueError(f"models.yaml: provider '{pid}' requires 'url'")
        raw_models = row.get("models") or []
        models = []
        for entry in raw_models:
            if isinstance(entry, dict):
                mid = str(entry.get("id") or "").strip()
            else:
                mid = str(entry or "").strip()
            if mid:
                models.append(mid)
        if not models:
            raise ValueError(f"models.yaml: provider '{pid}' requires at least one model")
        entry = {
            "id": pid,
            "url": url,
            "api_type": str(row.get("api_type") or row.get("api") or "openai-completions").strip(),
            "models": models,
        }
        display_name = row.get("display_name") or row.get("displayName")
        if display_name is not None and str(display_name).strip():
            entry["display_name"] = str(display_name).strip()
        if "api_key" in row:
            key = "" if row["api_key"] is None else str(row["api_key"]).strip()
            if key:
                entry["api_key"] = key
            else:
                entry["clear_api_key"] = True
        elif row.get("clear_api_key"):
            entry["clear_api_key"] = True
        else:
            entry["_key_omitted"] = True
        normalized.append(entry)
    return normalized


def _parse_ppt_models(raw_items):
    refs = []
    for item in raw_items or []:
        if isinstance(item, str) and "/" in item:
            provider, model = item.split("/", 1)
            refs.append({"provider": provider.strip(), "model": model.strip()})
        elif isinstance(item, dict) and item.get("provider") and item.get("model"):
            refs.append({"provider": str(item["provider"]).strip(), "model": str(item["model"]).strip()})
        else:
            raise ValueError("models.yaml: ppt_models entries must be 'provider/model' or {provider, model}")
    return refs


def sync_models_file(profile, models_path=None):
    import yaml
    path = Path(models_path or os.environ.get("CUSTOMER_MODELS_FILE", "/config/models.yaml"))
    if not path.is_file():
        return False
    raw = path.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    marker = Path(profile) / ".models-yaml-applied.sha256"
    doc = yaml.safe_load(raw.decode("utf-8"))
    if doc is None:
        doc = {}
    if not isinstance(doc, dict):
        raise ValueError("models.yaml: top-level YAML value must be a mapping")
    providers = _normalize_models_yaml_providers(doc.get("providers") or [])
    if not providers and not doc.get("clear_when_empty"):
        return False
    if marker.is_file() and marker.read_text().strip() == digest and not doc.get("force_apply"):
        return False
    mode = str(doc.get("mode") or "replace").strip().lower()
    if mode not in {"replace", "merge"}:
        raise ValueError("models.yaml: 'mode' must be 'replace' or 'merge'")
    with HTTP.open(_settings_request(), timeout=30) as response:
        current = json.load(response)
    if not providers:
        payload = {
            "revision": current["revision"],
            "providers": [],
            "orchestrator_provider": None,
            "orchestrator_model": None,
            "web_search_provider": str(doc.get("web_search_provider") or ""),
            "reasoning_effort": "",
            "fast_mode": False,
            "visual_tool": {},
            "delegate_agent": {},
            "plugin_models": {},
        }
    else:
        configured = []
        for row in providers:
            item = {k: v for k, v in row.items() if k != "_key_omitted"}
            if row.get("_key_omitted") and mode == "replace":
                item["clear_api_key"] = True
            configured.append(item)
        if mode == "merge":
            by_id = {}
            for existing in current.get("providers") or []:
                item = {
                    "id": existing["id"],
                    "url": existing["url"],
                    "api_type": existing.get("api_type") or "openai-completions",
                    "models": list(existing.get("models") or []),
                }
                if existing.get("display_name"):
                    item["display_name"] = existing["display_name"]
                by_id[existing["id"]] = item
            for item in configured:
                by_id[item["id"]] = item
            merged_providers = list(by_id.values())
        else:
            merged_providers = configured
        default_provider = str(doc.get("default_provider") or doc.get("orchestrator_provider") or "").strip()
        default_model = str(doc.get("default_model") or doc.get("orchestrator_model") or "").strip()
        if not default_provider and not default_model:
            if mode == "merge" and current.get("orchestrator_provider") and current.get("orchestrator_model"):
                default_provider = current["orchestrator_provider"]
                default_model = current["orchestrator_model"]
            else:
                default_provider = configured[0]["id"]
                default_model = configured[0]["models"][0]
        elif not default_provider or not default_model:
            raise ValueError("models.yaml: specify both default_provider and default_model")
        payload = {
            "revision": current["revision"],
            "providers": merged_providers,
            "orchestrator_provider": default_provider,
            "orchestrator_model": default_model,
        }
        if "web_search_provider" in doc:
            payload["web_search_provider"] = "" if doc["web_search_provider"] is None else str(doc["web_search_provider"]).strip()
        elif mode == "replace":
            payload["web_search_provider"] = ""
        if "reasoning_effort" in doc:
            payload["reasoning_effort"] = "" if doc["reasoning_effort"] is None else str(doc["reasoning_effort"]).strip()
        elif mode == "replace":
            payload["reasoning_effort"] = ""
        if "fast_mode" in doc:
            payload["fast_mode"] = bool(doc["fast_mode"])
        elif mode == "replace":
            payload["fast_mode"] = False
        if "visual_tool" in doc:
            payload["visual_tool"] = doc["visual_tool"] or {}
        elif mode == "replace":
            payload["visual_tool"] = {}
        if "delegate_agent" in doc:
            payload["delegate_agent"] = doc["delegate_agent"] or {}
        elif mode == "replace":
            payload["delegate_agent"] = {}
        if "plugin_models" in doc:
            payload["plugin_models"] = doc["plugin_models"] or {}
        elif "ppt_models" in doc:
            refs = _parse_ppt_models(doc.get("ppt_models"))
            payload["plugin_models"] = {"ppt-master": refs} if refs else {}
        elif mode == "replace":
            payload["plugin_models"] = {}
    try:
        with HTTP.open(_settings_request(payload), timeout=30) as response:
            saved = json.load(response)
    except urllib.error.HTTPError as error:
        body = error.read().decode(errors="replace")[:500]
        raise RuntimeError(f"Failed to apply models.yaml (HTTP {error.code}): {body}") from error
    temporary = marker.with_name(marker.name + ".tmp")
    temporary.write_text(digest + "\n")
    temporary.chmod(0o600)
    temporary.replace(marker)
    if RUNTIME_LOG is not None:
        RUNTIME_LOG.write(
            "supervisor",
            "models_yaml_applied",
            mode=mode,
            provider_count=len(saved.get("providers") or []),
            orchestrator_provider=saved.get("orchestrator_provider"),
            orchestrator_model=saved.get("orchestrator_model"),
        )
    return True


def wait_ready(url, processes, timeout=180):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for child in processes:
            if child.poll() is not None:
                raise RuntimeError("Service exited during startup: " + str(child.returncode))
        try:
            with HTTP.open(url, timeout=2) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(.5)
    raise RuntimeError("Service readiness timed out: " + (url.full_url if isinstance(url, urllib.request.Request) else url))


def main():
    global RUNTIME_LOG
    if sys.argv[1:] == ["healthcheck"]:
        return check_health()
    RUNTIME_LOG = CustomerLog(os.environ.get("CUSTOMER_LOG_DIR", str(STATE / "logs")))
    RUNTIME_LOG.write("supervisor", "starting", command=sys.argv[1:] or ["serve"])
    write_config()
    if sys.argv[1:] == ["init"]:
        process, output = start_logged([str(APP / "bin/customer-init")], "init", STATE / "server")
        code = process.wait()
        output.join(timeout=5)
        RUNTIME_LOG.write("init", "exited", code=code)
        return code
    if sys.argv[1:] == ["check"]:
        subprocess.run([str(APP / "bin/ai-ppt-server"), "--help"], check=True)
        print("Customer profile and ARM64 backend executable: OK")
        return 0
    if not (STATE / "customer-initialized.json").is_file():
        raise RuntimeError("Initialize a fresh customer database with deploy.sh init before starting")
    children = []
    outputs = []
    collector_stop, collector_thread = start_collector(STATE, RUNTIME_LOG)
    def start(command, service, cwd=None):
        process, output = start_logged(command, service, cwd)
        children.append(process)
        outputs.append(output)
    stopping = False
    def stop(signum, frame):
        nonlocal stopping
        stopping = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        start([str(APP / "bin/ai-ppt-server")], "backend", STATE / "server")
        wait_ready("http://127.0.0.1:8899/prod-api/health/filing-worker", children, timeout=300)
        start([str(APP / "bin/quickagent")], "agent", STATE / "workspace")
        wait_ready("http://127.0.0.1:3000/health", children)
        wait_ready("http://127.0.0.1:3001/agent/", children)
        sync_models_file(STATE / "agent-config")
        start([str(APP / "bin/customer-gateway")], "gateway")
        wait_ready(gateway_request(), children)
        deadline = time.monotonic() + 60
        while not ppt_registered():
            if time.monotonic() >= deadline:
                raise RuntimeError("PPT plugin registration timed out; inspect Agent startup logs")
            time.sleep(.5)
        print("Customer PPT is ready on 8899; authenticated QuickAgent gateway on 3002", flush=True)
        RUNTIME_LOG.write("supervisor", "ready")
        while not stopping:
            for child in children:
                code = child.poll()
                if code is not None:
                    RUNTIME_LOG.write("supervisor", "service_exited", pid=child.pid, code=code)
                    raise RuntimeError("Customer service exited: " + str(code))
            time.sleep(.5)
    finally:
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
        for child in reversed(children):
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        for output in outputs:
            output.join(timeout=2)
        collector_stop.set()
        collector_thread.join(timeout=5)
        RUNTIME_LOG.write("supervisor", "stopped")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        if RUNTIME_LOG is not None:
            RUNTIME_LOG.write("supervisor", "startup_failed", message=str(error))
        print("Customer startup failed: " + str(error), file=sys.stderr, flush=True)
        sys.exit(1)
