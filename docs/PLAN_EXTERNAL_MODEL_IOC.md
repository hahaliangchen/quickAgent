# QuickAgent 控制反转（外部供模）与模型运行时身份注入实施方案

> 适用仓库：`/home/goodtime/Documents/project/codex-workspace-mcp`（分支：`feat/context-as-code-v2`，**禁止碰 `master`**）
>
> 角色分工：本计划作为 GPT 开发施工规格书；完成后由宿主 / Antigravity 审查、验收、编译 release 并重启桌面端。
>
> 目标：
> 1. **控制反转（外部系统反向给 QuickAgent 供模）**：允许外部业务服务（如 `rag-go`）为自己的远程插件会话提供独立 OpenAI 兼容模型代理入口，**只驻留进程内存、绝不落盘到用户 `config.yml` 或 `remote-packages.json`、绝不泄露给前端设置接口**，且支持热更新与卸载自动清理，对主聊天与既有插件（AI PPT、Office、SSH 等）零影响。
> 2. **模型运行时自我身份感知**：让每个会话里的模型准确知道自己当前运行的模型名与提供商（如 `deepseek-flash`、`gpt-6.2-sol`、`qwen3.8-27b`），并在同一会话中途切换模型时实时同步，不破坏同模型多步的 KV-Cache 前缀命中。

---

## 模块 1：控制反转——外部供模注册表（`ExternalProviderRegistry`）

### 1.1 架构红线（解决两个源码级暗坑）
1. **绝不被 `load_config` 热重载冲掉**：
   - `src/model/model_catalog.rs` 的 `ModelCatalog::list()` 与 `AgentServiceState::with_latest_config()` 每次都会调用 `ai_proxy::load_config(&path)` 从磁盘重载 `config.yml`。
   - 因此外部供模**不能**只初始化一次写进 `Arc<RwLock<AiProxyConfig>>`，而必须做成独立的内存叠加层 **`ExternalProviderRegistry`**（`OnceLock<RwLock<...>>`），在 `ai_proxy::restricted_config(&config, plugin)` 与 `ai_proxy::plugin_catalog_view(&config, plugin)` 中按插件动态叠加！
2. **绝不落盘用户配置、绝不泄露 `api_key`**：
   - `save_settings` 与 `GET /agent/settings` 永远只读写用户自己的 `config.yml`，绝不把外部供模的 `url`/`api_key` 写进 `config.yml`，也不返回给前端设置页；
   - `src/server/skill_packages/remote.rs` 的 `remote::install()` 会把 `remote::Config` 持久化到磁盘配置。因此 `POST /agent/skill-packages/remote` 必须使用独立外层请求体 `ConnectRemoteRequest { url, token, model_provider? }`，只把 `remote::Config { url, token }` 传给 `remote::install` 落盘，`model_provider` **只写入内存 `ExternalProviderRegistry`**，绝不序列化到任何磁盘文件！

### 1.2 数据结构与双接入方式
建议新增独立小模块 `src/server/external_models.rs`（或在 `src/server/ai_proxy.rs` 内聚实现）：

```rust
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalModelProvider {
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_protocol")]
    pub protocol: String, // 默认 "openai-completions"
    pub provider_id: Option<String>, // 省略时按插件生成，如 format!("ext-{plugin_id}")
}
```

校验规则：
- `url` 必须非空、以 `http://` 或 `https://` 开头且不含控制字符；
- `model` 必须非空；
- `protocol` 仅允许宿主 `agent_api()` 支持的协议（默认 `"openai-completions"`，同时兼容 `"openai-responses"` / `"anthropic-messages"`）；
- `provider_id` 若提供，仅允许 ASCII 字母、数字、`-`、`_`，且避免与用户已有内置冲突时由叠加层按插件作用域覆盖。

**两种接入方式（同时支持，动态绑定优先级高于环境变量）**：
1. **方式 A：启动环境变量注入（必做）**
   - `EXTERNAL_LLM_PROVIDER_ID`（默认 `"external-upstream"`）
   - `EXTERNAL_LLM_BASE_URL`（如 `http://127.0.0.1:9004/internal/v1/kg/llm`）
   - `EXTERNAL_LLM_API_KEY`（如 `rag-go` 内部 Token，可为空）
   - `EXTERNAL_LLM_MODEL`（如 `rag-default`）
   - `EXTERNAL_LLM_PROTOCOL`（默认 `"openai-completions"`）
   - `EXTERNAL_LLM_PLUGINS`（逗号分隔插件 ID 列表，如 `rag-kg`）
   - 规则：
     - 当 `EXTERNAL_LLM_BASE_URL` 与 `EXTERNAL_LLM_MODEL` 非空且 `EXTERNAL_LLM_PLUGINS` 非空时，自动将列出的每个插件 ID 绑定到该外部供模；**未列出的插件和主聊天完全看不到它**。
     - 若 `EXTERNAL_LLM_PLUGINS` 为空且前两项非空，仅作为可选只读附加 provider 暴露给 `catalog_view`（不改变用户默认 `orchestrator_provider`/`orchestrator_model`，也不落盘到 `config.yml`）。
2. **方式 B：远程插件注册绑定 & 运行时热更新（必做）**
   - **注册接口扩展**：`POST /agent/skill-packages/remote` 请求体升级为 `ConnectRemoteRequest`：
     ```json
     {
       "url": "http://127.0.0.1:9004/kg-plugin",
       "token": "<KG_PLUGIN_TOKEN>",
       "model_provider": {
         "url": "http://127.0.0.1:9004/internal/v1/kg/llm",
         "api_key": "<KG_PLUGIN_TOKEN>",
         "model": "rag-default",
         "protocol": "openai-completions"
       }
     }
     ```
     安装插件成功后（拿回真实的 `plugin_id`，如 `"rag-kg"`），若携带 `model_provider`，先校验合法再写入内存 `ExternalProviderRegistry`；若安装失败，不得留下脏绑定。
   - **热更新接口**：新增 `PUT /agent/skill-packages/remote/{id}/model-provider`（以及 `DELETE /agent/skill-packages/remote/{id}/model-provider` 解除内存绑定）：
     - 校验插件 `{id}` 当前已连接挂载且为远程插件；
     - 用新的 `ExternalModelProvider` 原子覆盖内存绑定（支持不重启 QuickAgent 随时换代理地址、换内部 token 或换逻辑模型名）；
     - 响应绝不回显 `api_key`，只返回脱敏摘要 `{ "plugin_id", "provider", "model", "protocol", "url" }`。
   - **卸载自动清理**：远程插件被卸载/移除（`DELETE /agent/skill-packages/{id}` 或 `remote::remove` / `unmount`）时，自动清除 `ExternalProviderRegistry` 中对应的内存绑定。

### 1.3 与 `restricted_config` / `plugin_catalog_view` / 传输层的接线
1. **`ai_proxy::restricted_config(config, plugin)`**（[src/server/ai_proxy.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/server/ai_proxy.rs#L826)）：
   - 入口先查 `external_models::for_plugin(plugin)`：
   - 若命中该插件的外部供模 `ext`：
     - 在克隆出的 `scoped` 中插入对应的 `ProviderConfig`（`api_type = ext.protocol`、`base_url = ext.url`、`api_key = ext.api_key`、`model_map = { ext.model -> ext.model }`、`models = [ext.model]`）；
     - 将 `scoped.plugin_models` 中该 `plugin` 的允许列表设为 `vec![ModelRef { provider: provider_id, model: ext.model }]`；
     - 复用后续既有裁剪逻辑，自动把 `scoped.orchestrator_provider`、`scoped.orchestrator_model`、`scoped.default_provider` 锁定到该外部供模，并清空未授权的 `visual_tool` / `delegate_agent`；
   - 若未命中，原逻辑 100% 不变。
2. **`ai_proxy::plugin_catalog_view(config, plugin)`**：
   - 同样通过叠加后的 `restricted_config`（或共享辅助函数）产出该插件的模型列表，使 `plugin_sessions` 的 `method: "models"` 准确返回 `[ext.model]`，且绝不泄露 `api_key` 或 `url`。
3. **验证透传链路（不破坏既有行为）**：
   - 确认通过 `openai-completions` 调外部供模时，请求打到 `{url}/chat/completions`，携带 `Authorization: Bearer <api_key>` 与 `x-agent-affinity` 请求头，并正常流式解析 SSE 响应（含 `tool_calls`）。

---

## 模块 2：模型运行时自我身份注入（让模型知道自己是谁）

### 2.1 核心需求
- 当会话运行在 `deepseek-flash` 时，模型明确知道自己当前是 `deepseek-flash`；
- 当会话运行在 `gpt-6.2-sol` 时，模型明确知道自己当前是 `gpt-6.2-sol`；
- 当用户在**同一会话中途切换模型**（例如第 1 轮用 `deepseek-flash`，第 2 轮切到 `gpt-6.2-sol`），新模型必须立刻感知自己现在是 `gpt-6.2-sol`，绝不因为首轮 `pinned_system_prompt` 缓存而误报旧模型名；
- 在**同一模型连续多步**执行期间，`system` 消息内容必须逐字节完全一致，**绝不破坏 Prompt Cache / KV-Cache 命中率**。

### 2.2 精确实现位置
1. **[src/runtime/single_agent/model_call.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/single_agent/model_call.rs#L374-L401)**（`build_request_body_for_doc`）：
   - `pinned_system_prompt()` 保持不包含模型身份（确保换模型不会把旧模型名钉死在 `pinned_system_prompt` 里）；
   - 在 `build_request_body_for_doc` 构造最终 `system_text` 时，在末尾追加一段简洁稳定的运行时身份块（先防御性剔除任何已存在的同类块，避免重复）：
     ```text
     /* === RUNTIME IDENTITY === */
     Current runtime model: <s.model> (provider: <s.state.provider_name>)
     When asked which model you are, report this exact runtime model identifier (`<s.model>`) accurately; never guess from pretraining data or echo an earlier turn's different model.
     ```
   - 注意：因为 `build_request_body_for_doc` 覆盖全部主对话（代码/聊天）、插件会话及 `@virtual` 子帧，这一处修改即可全链路生效；且在同一个 `s.model` + `s.state.provider_name` 下每步完全不变，对 KV-Cache 零损耗。
2. **可选顺手增强 `host.context()`**：
   - 检查 [src/runtime/agent_program/declarations.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/agent_program/declarations.rs#L442) 及 `host.context()` 实现处，在返回值中补充 `model: s.model` 与 `provider: s.state.provider_name`（向后兼容新增只读字段），让 JS-VM 脚本也能读取当前运行模型。

---

## 3. 验收与测试要求

1. **控制反转（外部供模）单元测试与本地 Mock 端到端测试**：
   - 环境变量 `EXTERNAL_LLM_*` 绑定到指定插件 `rag-kg` 后：`restricted_config(cfg, "rag-kg")` 与 `plugin_catalog_view(cfg, "rag-kg")` 仅看到外部供模，而主聊天 `catalog_view` 及其他插件（如 `ppt-master`）完全不受影响；
   - `GET /agent/settings` 与 `save_settings` 后磁盘 `config.yml` **绝不包含**外部供模地址或 `api_key`；
   - `POST /agent/skill-packages/remote` 携带 `model_provider` 注册本地 Mock 远程插件后：
     - 磁盘 `remote-packages.json`（或 settings 文件）**绝不包含** `model_provider` / `api_key`；
     - 发起一次插件会话 turn，Mock 上游 `/chat/completions` 实际收到请求，校验 `Authorization: Bearer <api_key>`、`x-agent-affinity` 存在、`model == "rag-default"`，并跑通 SSE 返回触发一次远程插件工具调用；
     - 调用 `PUT /agent/skill-packages/remote/{id}/model-provider` 切换到第二个 Mock 上游后，下一轮请求立刻打到新上游（无需重启）；
     - 卸载该远程插件后，内存绑定自动清除。
2. **模型自我身份单元测试**（在 `model_call.rs` tests）：
   - 断言 `build_request_body_for_doc` 生成的 `system` 消息包含当前 `s.model` 与 `s.state.provider_name`；
   - 断言即使 `w.pinned_system_prompt` 来自上一轮旧模型，切换到新 `s.model` 后生成的 `system` 消息只含新模型名、不重复也不残留旧模型名；
   - 断言同一模型连续两次调用生成的 `system` 文本逐字节完全相同（保障 KV-Cache 前缀稳定）。
3. **编译与回归命令**：
   - 编译前用 `df -h .` 确认磁盘可用 > 20 GB；
   - 运行 `cargo test --bin codex-workspace-mcp -- external_models ai_proxy model_catalog plugin_sessions model_call declarations tool_wiring_invariants`，要求 **0 警告、0 失败**；
   - **不要终止或重启当前运行中的 `grow-desktop` 进程**，完成后汇报改动文件与验证结果即可。
