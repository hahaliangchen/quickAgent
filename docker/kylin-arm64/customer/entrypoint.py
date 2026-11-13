#!/usr/bin/env python3
import base64
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

from customer_templates import prune_unicom_templates

APP = Path("/opt/customer")
STATE = Path("/state")
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def gateway_request():
    credentials = base64.b64encode(("admin:" + os.environ["CUSTOMER_ADMIN_PASSWORD"]).encode()).decode()
    return urllib.request.Request("http://127.0.0.1:3002/agent/settings/data",
                                  headers={"Authorization": "Basic " + credentials})


def check_health():
    for target in ["http://127.0.0.1:8899/prod-api/health/filing-worker",
                   "http://127.0.0.1:3000/health", gateway_request()]:
        with HTTP.open(target, timeout=3) as response:
            if response.status != 200:
                raise RuntimeError("Customer service is not ready")
    return 0


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
    if sys.argv[1:] == ["healthcheck"]:
        return check_health()
    write_config()
    if sys.argv[1:] == ["init"]:
        os.chdir(STATE / "server")
        os.execv(APP / "bin/customer-init", [str(APP / "bin/customer-init")])
    if sys.argv[1:] == ["check"]:
        subprocess.run([str(APP / "bin/ai-ppt-server"), "--help"], check=True)
        print("Customer profile and ARM64 backend executable: OK")
        return 0
    if not (STATE / "customer-initialized.json").is_file():
        raise RuntimeError("Initialize a fresh customer database with deploy.sh init before starting")
    children = []
    stopping = False
    def stop(signum, frame):
        nonlocal stopping
        stopping = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        children.append(subprocess.Popen([str(APP / "bin/ai-ppt-server")], cwd=STATE / "server", start_new_session=True))
        wait_ready("http://127.0.0.1:8899/prod-api/health/filing-worker", children, timeout=300)
        children.append(subprocess.Popen([str(APP / "bin/quickagent")], cwd=STATE / "workspace", start_new_session=True))
        wait_ready("http://127.0.0.1:3000/health", children)
        wait_ready("http://127.0.0.1:3001/agent/", children)
        children.append(subprocess.Popen([str(APP / "bin/customer-gateway")], start_new_session=True))
        wait_ready(gateway_request(), children)
        print("Customer PPT is ready on 8899; authenticated QuickAgent gateway on 3002", flush=True)
        while not stopping:
            for child in children:
                code = child.poll()
                if code is not None:
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
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print("Customer startup failed: " + str(error), file=sys.stderr, flush=True)
        sys.exit(1)
