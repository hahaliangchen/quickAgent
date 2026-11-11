#!/usr/bin/env python3
"""Assemble the compiled snapshots into an offline ARM64 deployment folder."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def copy_tree(source, destination):
    shutil.copytree(source, destination, dirs_exist_ok=True, ignore=shutil.ignore_patterns(
        ".git", "__pycache__", "*.pyc", ".env", ".env.*", "credentials*.json", "credentials*.yaml", "*.map", "*.log", "*.sql", "node_modules"))


def verify_elf(path):
    header = subprocess.check_output(["readelf", "-h", str(path)], text=True)
    if "AArch64" not in header:
        raise RuntimeError("Not ARM64: " + str(path))
    info = subprocess.check_output(["readelf", "--version-info", "--wide", str(path)], text=True)
    import re
    versions = {tuple(map(int, version.split("."))) for version in re.findall(r"\bName: GLIBC_(\d+\.\d+(?:\.\d+)?)", info.partition("Version needs section")[2])}
    maximum = max(versions, default=(0, 0))
    if maximum > (2, 28):
        raise RuntimeError("glibc requirement exceeds 2.28: " + str(path))
    return {"file":path.name, "architecture":"aarch64", "required_glibc":".".join(map(str, maximum)) if versions else "static", "sha256":hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--export", action="store_true")
    args = parser.parse_args()
    work = args.work.resolve()
    stage = work / "stage"
    source = work / "source"
    app = stage / "app"
    for name in ["quickagent", "ai-ppt-server", "customer-init"]:
        if not (app / "bin" / name).is_file():
            raise RuntimeError("Missing binary: " + name)
    for path in [source/"agent/frontend/dist/index.html", source/"view/build/index.html", source/"engine/rust-engine/pkg/ppt_engine_bg.wasm", source/"engine/rust-engine/pkg-node/ppt_engine_bg.wasm"]:
        if not path.is_file(): raise RuntimeError("Missing application artifact: " + str(path))
    for previous in [app/"agent-web", app/"server/web"]:
        if previous.exists(): shutil.rmtree(previous)
    copy_tree(source/"agent/frontend/dist", app/"agent-web")
    for name in ["code_flow", "code_index", "git", "web"]:
        copy_tree(source/"agent/src/builtin_plugins"/name, app/"agent-resources/src/builtin_plugins"/name)
    copy_tree(source/"view/build", app/"server/web")
    # Other modules' example documents are not customer PPT assets.
    for name in ["filing-demo"]:
        if (app/"server/web"/name).exists(): shutil.rmtree(app/"server/web"/name)
    for path in (app/"server/web").rglob("*.pdf"): path.unlink()
    for name in ["ai_skills", "document-templates", "function", "mcp", "onlyoffice-plugins", "package", "plugin", "workbench"]:
        if (source/"server/resource"/name).is_dir():
            copy_tree(source/"server/resource"/name, app/"server/resource"/name)
    renderer = stage / "renderer/engine"
    for name in ["src", "tools", "rust-engine/pkg", "rust-engine/pkg-node"]:
        copy_tree(source/"engine"/name, renderer/name)
    for name in ["package.json", "package-lock.json", "tsconfig.json", "arm64-native-dependencies.json"]:
        shutil.copy2(source/"engine"/name, renderer/name)
    # Runtime dependencies were installed for ARM64, never copied from the host.
    if (renderer/"node_modules").exists(): shutil.rmtree(renderer/"node_modules")
    shutil.copytree(source/"engine/node_modules", renderer/"node_modules", symlinks=True)
    profile = app / "config/agent"
    if profile.exists(): shutil.rmtree(profile)
    spec = importlib.util.spec_from_file_location("customer_profile", ROOT/"scripts/prepare-ppt-customer-profile.py")
    exporter = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(exporter)
    policy = exporter.write_profile(Path.home()/".config/grow", profile)
    shutil.copy2(HERE/"entrypoint.py", app/"bin/entrypoint.py")
    for file in ["app.Dockerfile", "renderer.Dockerfile"]:
        shutil.copy2(HERE/file, stage/file)
    licenses = app/"licenses"
    licenses.mkdir(exist_ok=True)
    shutil.copy2(source/"agent/third_party/DSH-LICENSE.txt", licenses/"DSH-LICENSE.txt")
    validation = [verify_elf(app/"bin"/name) for name in ["quickagent", "ai-ppt-server", "customer-init"]]
    manifest = {"format":1, "target":{"os":"Kylin V10 Halberd", "architecture":"aarch64", "glibc":"2.28", "kernel":"4.19"},
                "sources":json.loads((work/"sources.json").read_text()), "customer_policy":policy,
                "binaries":validation, "native_tools":json.loads((work/"native-tools/provenance.json").read_text()),
                "frontend_native_dependencies":json.loads((source/"view/arm64-native-dependencies.json").read_text()),
                "sdk_images":{}, "credentials_included":False}
    manifest["build_recipe_sha256"] = {p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(HERE.iterdir()) if p.is_file()}
    manifest["compiled_customer_source_files_sha256"] = {}
    for relative in ["view/src/gva/router/pptCustomerMenus.ts", "view/src/gva/router/defaultEntryPath.ts", "view/src/view/createPpt/index.tsx", "server/cmd/customer-init/main.go"]:
        manifest["compiled_customer_source_files_sha256"][relative] = hashlib.sha256((source/relative).read_bytes()).hexdigest()
    for image in ["quickagent-kylin-arm64-env:v1", "quickagent-ppt-browser-arm64-env:v1"]:
        record=json.loads(subprocess.check_output(["docker","image","inspect",image]))[0]
        manifest["sdk_images"][image]=record["Id"]
    output=args.output.resolve()
    output.mkdir(parents=True,exist_ok=True)
    (output/"manifest.json").write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+"\n")
    for name in ["compose.yaml","deploy.sh"]:
        shutil.copy2(HERE/name,output/name)
    (output/"deploy.sh").chmod(0o755)
    (output/"README-部署.txt").write_text(
        "麒麟 ARM64 客户部署包（Web 版）\n\n"
        "目标：aarch64 / 麒麟 V10 Halberd / glibc 2.28 / 内核 4.19。\n"
        "前置：目标机已安装 ARM64 Docker Engine 24 或更新版本及 Docker Compose v2。无需安装 Go、Rust、Node 或 Python。\n"
        "包内包含 QuickAgent、AI PPT 前后端、PPTX/WASM 渲染器、中文字体、MySQL 8 和 Redis 7 的 ARM64 镜像。\n"
        "前端分支 codex/ppt-host-agent；后端分支 codex/ppt-remote-svg。具体提交、工作区快照校验值见 manifest.json。\n\n"
        "部署：\n1. 解压本目录后运行 ./deploy.sh load，校验文件并加载离线镜像。\n"
        "2. 运行 ./deploy.sh init。首次在空数据库初始化，生成本机随机密码和 JWT 密钥。\n"
        "3. 编辑新生成的 .env，填写经授权的非自用 DEEPSEEK_API_KEY。\n"
        "4. 运行 ./deploy.sh up；访问 http://服务器地址:8899。\n"
        "5. 用户名 admin；初始密码为本机 .env 的 CUSTOMER_ADMIN_PASSWORD，登录后可修改。\n\n"
        "客户入口只保留创建 PPT、历史会话；Agent 和数据库模型目录只包含非自用 DeepSeek Flash / V4 Pro。\n"
        "QuickAgent 的设置端口和渲染器不发布到宿主，仅通过本机/内部网络调用。\n"
        "查看状态 ./deploy.sh status；日志 ./deploy.sh logs；停止 ./deploy.sh stop。\n"
        "数据保存在 state/、Compose 的 mysql-data 与 redis-data volume；升级前保留并备份这些数据及 .env。\n"
        "初始化器拒绝在已有表的数据库执行首次初始化，不导入开发机数据库或业务数据。\n"
        "离线镜像可在无外网环境加载与启动；调用 DeepSeek 仍需能访问其 API，并配置有效密钥。\n"
        "实际 ARM64 服务器保持 QUICKAGENT_ARM64_EMULATED=0；完整的 x86 QEMU 验证使用源码中的 smoke.py（含一次性 Redis 探针测试覆盖）。\n"
    )
    tags=["quickagent-customer-ppt-arm64:20261111","quickagent-customer-ppt-renderer-arm64:20261111"]
    for dockerfile,tag in zip(["app.Dockerfile","renderer.Dockerfile"],tags):
        subprocess.run(["docker","build","--platform","linux/arm64","-t",tag,"-f",str(stage/dockerfile),str(stage)],check=True)
    for source_tag,target in [("mysql:8.0","quickagent-customer-mysql-arm64:8.0"),("redis:7-alpine","quickagent-customer-redis-arm64:7")]:
        subprocess.run(["docker","tag",source_tag,target],check=True)
        tags.append(target)
    manifest["release_images"] = {}
    for tag in tags:
        data=json.loads(subprocess.check_output(["docker","image","inspect","--platform","linux/arm64",tag]))[0]
        if data["Architecture"]!="arm64": raise RuntimeError("Non-ARM64 release image: "+tag)
        manifest["release_images"][tag] = {"id": data["Id"], "architecture": data["Architecture"]}
    (output/"manifest.json").write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+"\n")
    if args.export:
        subprocess.run(["docker","save","--platform","linux/arm64","-o",str(output/"images.tar"),*tags],check=True)
    print("Deployment folder: "+str(output),flush=True)


if __name__=="__main__": main()
