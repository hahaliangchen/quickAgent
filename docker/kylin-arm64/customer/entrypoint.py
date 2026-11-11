#!/usr/bin/env python3
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

import yaml

APP = Path("/opt/customer")
STATE = Path("/state")
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def write_config():
    STATE.mkdir(parents=True, exist_ok=True)
    for folder in ["logs", "workspace", "server", "server/projects", "server/uploads/file"]:
        (STATE / folder).mkdir(parents=True, exist_ok=True)
    resources = STATE / "server/resource"
    if not resources.exists():
        shutil.copytree(APP / "server/resource", resources)
    profile = STATE / "agent-config"
    if not profile.exists():
        shutil.copytree(APP / "config/agent", profile)
        profile.chmod(0o700)
    patches = yaml.safe_load((profile / "providers.yml").read_text())
    providers = next(p["config"]["providers"] for p in patches if p.get("id") == "llm-pi-ai")
    if set(providers) != {"deepseek"} or {m["id"] for m in providers["deepseek"]["models"]} != {"deepseek-flash", "deepseek-v4-pro"}:
        raise RuntimeError("Customer profile must contain only the non-personal deepseek provider and its two allowed models")
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
        "agent": {"ai-provider":"deepseek", "skill-resource-dir":"resource/ai_skills", "project-output-dir":"projects", "session-ttl":"72h", "session-key-prefix":"ppt-customer:", "max-consecutive-failures":3, "max-history-string-len":16384},
        "deepseek": {"api-key":os.getenv("DEEPSEEK_API_KEY",""), "model":"deepseek-flash", "base-url":"https://api.deepseek.com/v1"},
        "workbench": {"data-file":str(STATE / "server/workbench-data.json"), "model-provider":"deepseek", "llm-model":"deepseek-flash", "llm-base-url":"https://api.deepseek.com/v1", "llm-api-key":os.getenv("DEEPSEEK_API_KEY",""), "enable-heuristic-fallback":False},
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
    raise RuntimeError("Service readiness timed out: " + url)


def main():
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
        print("Customer PPT and QuickAgent are ready on port 8899", flush=True)
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
