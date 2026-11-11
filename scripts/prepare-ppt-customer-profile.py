#!/usr/bin/env python3
"""Export a separate DeepSeek-only customer profile; never copy credentials."""
import argparse
import copy
import json
from pathlib import Path
from urllib.parse import urlsplit

import yaml

PROVIDER = "deepseek"
MODEL_FIELDS = {"id", "name", "contextWindow", "maxTokens", "input", "output", "reasoningEfforts"}
CAPABILITY_FIELDS = {"tool_calling", "reasoning_efforts", "default_effort", "fast_mode", "image_input"}


def project_profile(patches, generation):
    config = next((p.get("config", {}) for p in patches if p.get("id") == "llm-pi-ai"), {})
    provider = config.get("providers", {}).get(PROVIDER)
    if not isinstance(provider, dict):
        raise ValueError("缺少非自用提供商 deepseek；不会使用 deepseek-self 替代")
    url = urlsplit(provider.get("baseURL", ""))
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError("DeepSeek 地址必须是无内嵌凭据、查询参数的 HTTPS URL")
    models = [{k: copy.deepcopy(v) for k, v in m.items() if k in MODEL_FIELDS}
              for m in provider.get("models", []) if isinstance(m, dict) and m.get("id")]
    if not models:
        raise ValueError("deepseek 没有已配置的模型")
    ids = {model["id"] for model in models}
    default = "deepseek-flash" if "deepseek-flash" in ids else models[0]["id"]
    clean_provider = {
        "api": provider.get("api", "openai-completions"), "baseURL": provider["baseURL"],
        "displayName": "DeepSeek", "apiKeyEnv": "DEEPSEEK_API_KEY", "models": models,
    }
    clean_patches = [
        {"id": "llm-pi-ai", "config": {"providers": {PROVIDER: clean_provider}}},
        {"id": "agent-default-model", "config": {"provider": PROVIDER, "model": default, "reasoningEffort": "high"}},
    ]
    capabilities = {
        model: {k: copy.deepcopy(v) for k, v in item.items() if k in CAPABILITY_FIELDS}
        for model, item in generation.get("model_capabilities", {}).get(PROVIDER, {}).items()
        if model in ids and isinstance(item, dict)
    }
    clean_generation = {
        "reasoning_effort": "high", "fast_mode": False,
        "visual_tool": {"provider": PROVIDER, "model": default, "reasoning_effort": "high"},
        "delegate_agent": {}, "web_search_provider": PROVIDER,
        "model_capabilities": {PROVIDER: capabilities},
        "plugin_models": {"ppt-master": [{"provider": PROVIDER, "model": model["id"]} for model in models]},
        "plugin_generation": {"ppt-master": {"reasoning_effort": "high", "visual_reasoning_effort": "high"}},
    }
    return clean_patches, clean_generation


def write_profile(source, output):
    patches = yaml.safe_load((source / "providers.yml").read_text())
    generation_path = source / "agent-generation.yml"
    generation = yaml.safe_load(generation_path.read_text()) if generation_path.is_file() else {}
    providers, generation = project_profile(patches, generation or {})
    # Validate before creating the directory; existing customer settings must not
    # be silently overwritten, especially after a customer has filled in a key.
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    files = {
        "providers.yml": providers, "agent-generation.yml": generation,
        "credentials.yaml": {"version": 1, "refs": {}, "records": {}},
        "skill-packages.json": {"remotes": {"ppt-master": {
            "url": "http://127.0.0.1:8899/prod-api/aiPpt/plugin", "token": ""}},
            "enabled": {"ppt-master": True}},
        "customer-policy.json": {"provider": PROVIDER,
            "models": [m["model"] for m in generation["plugin_models"]["ppt-master"]],
            "menu_paths": ["/create-ppt", "/ppt-history"], "frontend_build_env": {"PPT_CUSTOMER_EDITION": "1"},
            "server_branch": "codex/ppt-remote-svg", "view_branch": "codex/ppt-host-agent",
            "credentials_included": False},
    }
    for name, value in files.items():
        path = output / name
        path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
        path.chmod(0o600)
    (output / "README-客户配置.txt").write_text(
        "仅保留非自用 deepseek 提供商及其模型，已清理其他提供商、默认路由和插件模型引用。\n"
        "本目录不含 API 密钥；请在目标环境设置 DEEPSEEK_API_KEY。\n"
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
    print("客户配置已生成；仅保留 %s: %s（不含密钥）" % (PROVIDER, ", ".join(policy["models"])))
