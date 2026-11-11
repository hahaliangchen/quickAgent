#!/usr/bin/env python3
"""Build a customer release from isolated snapshots of the selected worktrees."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import urllib.request
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[3]
TOOLS = "quickagent-kylin-arm64-env:v1"
BROWSER = "quickagent-ppt-browser-arm64-env:v1"


def run(argv, *, cwd=None, env=None, log=None):
    options = {"cwd": cwd, "env": env, "check": True}
    if log:
        with Path(log).open("a") as output:
            subprocess.run(argv, stdout=output, stderr=subprocess.STDOUT, **options)
    else:
        subprocess.run(argv, **options)


def git(path, *args):
    return subprocess.check_output(["git", "-C", str(path), *args], text=True).strip()


def snapshot(source, destination, expected=None):
    branch = git(source, "branch", "--show-current")
    if expected and branch != expected:
        raise RuntimeError(f"{source.name}: expected {expected}, got {branch}")
    files = subprocess.check_output(["git", "-C", str(source), "ls-files", "-z", "--cached", "--others", "--exclude-standard"]).decode().split("\0")
    digest = hashlib.sha256()
    copied = 0
    for name in sorted(set(files)):
        if not name:
            continue
        relative = Path(name)
        if any(part in {".git", ".codex-workspace-mcp", "node_modules", "target", "logs", "log", "runs", "scratch", "projects", "__pycache__", "ppt-dependency-bundles"} for part in relative.parts):
            continue
        if relative.parts[0] in {"build", "dist", "dist-ppt-plugin", "dist-ppt-agent-test", "runtime", ".runtime", ".ppt-deps", "memory", "web_uploads"}:
            continue
        if relative.name.startswith(".env") or relative.name in {"credentials.json", "credentials.yaml"} or relative.suffix in {".sql", ".log"}:
            continue
        if len(relative.parts) == 1 and (name.startswith("config.") and relative.suffix in {".yaml", ".yml"}):
            continue
        path = source / relative
        if not path.is_file():
            continue
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target, follow_symlinks=True)
        content = path.read_bytes()
        digest.update(name.encode() + b"\0" + hashlib.sha256(content).digest())
        copied += 1
    return {"branch": branch, "commit": git(source, "rev-parse", "HEAD"), "dirty": bool(git(source, "status", "--porcelain")),
            "snapshot_sha256": digest.hexdigest(), "files": copied}


def download(url, path, expected):
    if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == expected:
        return
    with urllib.request.urlopen(url, timeout=180) as response, path.open("wb") as output:
        shutil.copyfileobj(response, output)
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise RuntimeError("Tool checksum mismatch: " + path.name)


def prepare_native_tools(work):
    native = work / "native-tools"
    native.mkdir(exist_ok=True)
    with urllib.request.urlopen("https://go.dev/dl/?mode=json&include=all", timeout=45) as response:
        releases = json.load(response)
    entry = next(f for release in releases if release["version"] == "go1.27.1" for f in release["files"] if f["filename"] == "go1.27.1.linux-amd64.tar.gz")
    archive = native / entry["filename"]
    download("https://go.dev/dl/" + entry["filename"], archive, entry["sha256"])
    if not (native / "go/bin/go").is_file():
        run(["tar", "-xzf", str(archive), "-C", str(native)])
    with urllib.request.urlopen("https://api.github.com/repos/wasm-bindgen/wasm-pack/releases/tags/v0.15.1", timeout=45) as response:
        release = json.load(response)
    asset = next(a for a in release["assets"] if a["name"] == "wasm-pack-v0.15.1-x86_64-unknown-linux-musl.tar.gz")
    archive = native / asset["name"]
    download(asset["browser_download_url"], archive, asset["digest"].removeprefix("sha256:"))
    if not (native / "wasm-pack").is_file():
        run(["tar", "-xzf", str(archive), "-C", str(native), "--strip-components=1"])
    (native / "provenance.json").write_text(json.dumps({"go": entry, "wasm_pack": {"url": asset["browser_download_url"], "sha256": asset["digest"]}}, indent=2) + "\n")


def build(work, only=None):
    source = work / "source"
    stage = work / "stage"
    logs = work / "logs"
    logs.mkdir(exist_ok=True)
    stage.mkdir(exist_ok=True)
    for directory in ["app/bin", "app/server/resource"]:
        (stage / directory).mkdir(parents=True, exist_ok=True)
    native = work / "native-tools"
    destination = source / "server/cmd/customer-init/main.go"
    shutil.copy2(Path(__file__).with_name("bootstrap.go"), destination)
    shutil.copy2(Path(__file__).with_name("native-deps.cjs"), work / "native-deps.cjs")
    name = "quickagent-customer-cc-" + work.name[-12:].lower()
    # Native Rust drives the exact Kylin ARM64 compiler/linker. All temporary
    # paths and registry C sources are visible at identical absolute paths.
    run(["docker", "run", "-d", "--name", name, "--platform", "linux/arm64", "--network", "none",
         "-v", f"{work}:{work}", "-v", f"{Path.home() / '.cargo/registry'}:{Path.home() / '.cargo/registry'}:ro",
         "-v", f"{Path.home() / '.rustup'}:{Path.home() / '.rustup'}:ro", TOOLS, "sleep", "infinity"])
    wrappers = work / "wrappers"
    wrappers.mkdir(exist_ok=True)
    for tool in ["gcc", "ar"]:
        path = wrappers / ("kylin-aarch64-" + tool)
        path.write_text(f'#!/usr/bin/env bash\nset -euo pipefail\nexec docker exec -i -w "$PWD" {name} /usr/bin/{tool} "$@"\n')
        path.chmod(0o755)
    temp = work / "tmp"
    temp.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update({"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_JOBS": "2", "TMPDIR": str(temp),
                "CARGO_TARGET_DIR": str(work / "cache/agent-target"),
                "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER": str(wrappers / "kylin-aarch64-gcc"),
                "CC_aarch64_unknown_linux_gnu": str(wrappers / "kylin-aarch64-gcc"),
                "AR_aarch64_unknown_linux_gnu": str(wrappers / "kylin-aarch64-ar")})

    probe = temp / "probe.rs"
    probe.write_text('fn main() { println!("Kylin ARM64 cross-linker OK"); }\n')
    run(["rustc", "--target", "aarch64-unknown-linux-gnu", "-C", "linker=" + str(wrappers / "kylin-aarch64-gcc"), str(probe), "-o", str(temp / "probe")], cwd=work, env=env)
    run(["docker", "exec", name, str(temp / "probe")])
    run(["docker", "exec", name, "node", str(source / "view/scripts/build-ppt-plugin.mjs"), str(source / "server/api/v1/system/ppt_ui")])

    def rust():
        run(["cargo", "build", "--locked", "--release", "--target", "aarch64-unknown-linux-gnu", "--bin", "codex-workspace-mcp"],
            cwd=source / "agent", env=env, log=logs / "agent-rust.log")
        (stage / "app/bin").mkdir(parents=True, exist_ok=True)
        shutil.copy2(work / "cache/agent-target/aarch64-unknown-linux-gnu/release/codex-workspace-mcp", stage / "app/bin/quickagent")

    def go():
        command = r'''set -Eeuo pipefail
export GOOS=linux GOARCH=arm64 GOARM64=v8.0 CGO_ENABLED=0 GOTOOLCHAIN=local GOMAXPROCS=2 GOFLAGS=-p=2
export PATH=/opt/native-go/bin:$PATH
cd "$BUILD_ROOT/source/server"
mkdir -p "$BUILD_ROOT/stage/app/bin"
go build -mod=readonly -buildvcs=false -trimpath -ldflags='-s -w' -o "$BUILD_ROOT/stage/app/bin/ai-ppt-server" .
go build -mod=readonly -buildvcs=false -trimpath -ldflags='-s -w' -o "$BUILD_ROOT/stage/app/bin/customer-init" ./cmd/customer-init
'''
        run(["docker", "run", "--rm", "--platform", "linux/arm64", "--network", "host",
             "-e", "HTTP_PROXY", "-e", "HTTPS_PROXY", "-e", "ALL_PROXY", "-e", "BUILD_ROOT=" + str(work),
             "-v", f"{work}:{work}", "-v", f"{native / 'go'}:/opt/native-go:ro",
             "-v", "rag-go-go-mod-cache:/root/go/pkg/mod", "-v", "quickagent-customer-go-cache:/root/.cache/go-build",
             TOOLS, "bash", "-c", command], log=logs / "ai-ppt-go.log")

    def web():
        wasm_env = os.environ.copy()
        wasm_env.update({"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_JOBS": "2", "CARGO_TARGET_DIR": str(work / "cache/wasm-target"), "TMPDIR": str(temp)})
        for target, output in [("bundler", "pkg"), ("nodejs", "pkg-node")]:
            run([str(native / "wasm-pack"), "build", "rust-engine", "--target", target, "--out-dir", output, "--no-opt", "--", "--locked"],
                cwd=source / "engine", env=wasm_env, log=logs / "engine-wasm.log")
        command = r'''set -Eeuo pipefail
export npm_config_proxy="${HTTP_PROXY:-}" npm_config_https_proxy="${HTTPS_PROXY:-}"
export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 PPT_CUSTOMER_EDITION=1 NODE_OPTIONS=--max-old-space-size=4096 RAYON_NUM_THREADS=2
export PPTX_ENGINE_DIR="$BUILD_ROOT/source/engine"
ensure_deps() {
    key="$(sha256sum package.json package-lock.json | sha256sum | cut -d' ' -f1)"
    if [[ ! -d node_modules || "$(cat .customer-deps 2>/dev/null || true)" != "$key" ]]; then
        npm ci --include=optional --ignore-scripts --no-audit --no-fund
        printf '%s\n' "$key" > .customer-deps
    fi
    node "$BUILD_ROOT/native-deps.cjs"
}
cd "$BUILD_ROOT/source/engine"
ensure_deps
cd "$BUILD_ROOT/source/agent/frontend"
ensure_deps
npm run build
cd "$BUILD_ROOT/source/view"
ensure_deps
npm run sync:pdf-worker
export PPT_AGENT_BASE=/ppt-agent VUE_APP_BASE_API=/prod-api USER_BASE_URL=/ USER_BASENAME=/ PUBLIC_PATH=/ ENV=prod
node node_modules/@rspack/cli/bin/rspack.js build -c rspack/rspack.prod.js
node scripts/build-ppt-plugin.mjs "$BUILD_ROOT/stage/app/server/resource/ppt-agent-plugin"
'''
        run(["docker", "run", "--rm", "--platform", "linux/arm64", "--network", "host",
             "-e", "HTTP_PROXY", "-e", "HTTPS_PROXY", "-e", "ALL_PROXY", "-e", "BUILD_ROOT=" + str(work),
             "-v", f"{work}:{work}", TOOLS, "bash", "-c", command], log=logs / "web.log")

    try:
        with ThreadPoolExecutor(max_workers=3) as pool:
            choices = {"rust": rust, "go": go, "web": web}
            jobs = [pool.submit(fn) for key, fn in choices.items() if only is None or key in only]
            for job in jobs:
                job.result()
    finally:
        run(["docker", "rm", "-f", name])
    print("Application compilation complete: " + str(stage), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", type=Path)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--only", nargs="+", choices=["rust", "go", "web"])
    args = parser.parse_args()
    stamp = datetime.now(ZoneInfo("Asia/Shanghai")).strftime("%Y%m%d-%H%M%S")
    work = (args.work or ROOT / "target/kylin-arm64" / ("customer-build-" + stamp)).resolve()
    if not args.resume:
        work.mkdir(parents=True, exist_ok=False)
        sources = {}
        for key, directory, branch in [
            ("agent", ROOT, None),
            ("server", ROOT.parent / "ai-ppt-server", "codex/ppt-remote-svg"),
            ("view", ROOT.parent / "ai-ppt-view", "codex/ppt-host-agent"),
            ("engine", Path.home() / "project/pptx-editor-engine", None),
        ]:
            sources[key] = snapshot(directory, work / "source" / key, branch)
        destination = work / "source/server/cmd/customer-init/main.go"
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(Path(__file__).with_name("bootstrap.go"), destination)
        (work / "sources.json").write_text(json.dumps(sources, ensure_ascii=False, indent=2) + "\n")
        prepare_native_tools(work)
    print("BUILD_WORK=" + str(work), flush=True)
    if not args.prepare_only:
        build(work, args.only)
