#!/usr/bin/env python3
"""Export sanitized customer defaults; never copy development credentials."""
import argparse
import json
from pathlib import Path

def project_profile(patches, generation):
    # A customer config is deliberately independent of every development route.
    clean_patches = [{"id": "llm-pi-ai", "config": {"providers": {}}}]
    clean_generation = {
        "reasoning_effort": None, "fast_mode": False,
        "visual_tool": {},
        "delegate_agent": {}, "web_search_provider": None,
        "model_capabilities": {},
        "plugin_models": {},
        "plugin_generation": {},
    }
    return clean_patches, clean_generation


def write_profile(source, output):
    # Keep the source argument for CLI compatibility; read no personal config.
    providers, generation = project_profile([], {})
    # Validate before creating the directory; existing customer settings must not
    # be silently overwritten, especially after a customer has filled in a key.
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    files = {
        "providers.yml": providers, "agent-generation.yml": generation,
        "credentials.yaml": {"version": 1, "refs": {}, "records": {}},
        "skill-packages.json": {"remotes": {"ppt-master": {
            "url": "http://127.0.0.1:8899/prod-api/aiPpt/plugin", "token": ""}},
            "enabled": {"ppt-master": True}},
        "customer-policy.json": {"providers": [], "models": [],
            "customer_custom_models": True,
            "menu_paths": ["/create-ppt", "/ppt-history"], "frontend_build_env": {"PPT_CUSTOMER_EDITION": "1"},
            "credentials_included": False},
    }
    for name, value in files.items():
        path = output / name
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
        path.chmod(0o600)
    (output / "README-客户配置.txt").write_text(
        "本目录不含提供商、模型、默认路由、插件模型引用或 API 密钥；由客户自行配置。\n"
        "支持在设置页添加客户自部署模型：填写 OpenAI 兼容地址和实际模型 ID，免鉴权服务的 API 密钥留空。\n"
        "联网搜索默认未启用，视觉和协作模型跟随主模型，PPT 插件默认可使用所有已登记模型。\n"
        "将 GROW_CONFIG_DIR 指向本目录，使用 AI PPT tools/start-ppt.py 启动以隔离旧配置迁移。\n"
        "前端客户构建必须设置 PPT_CUSTOMER_EDITION=1，菜单仅保留创建 PPT 和历史会话。\n"
        "后端仅当端口/前缀为 8899、/prod-api 时可直接使用插件地址，否则按部署值修改 skill-packages.json。\n"
    )
    return files["customer-policy.json"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.home() / ".config/grow")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    policy = write_profile(args.source.resolve(), args.output.resolve())
    print("客户配置已生成：提供商、模型和凭据全部为空。")
