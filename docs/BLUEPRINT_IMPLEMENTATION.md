# 架构蓝图施工验收记录

分支：`feat/context-as-code-v2`。未提交、未合并；没有改动 `frontend/src/builtin_plugins/git/`。原始施工方案本身未修改。

## 验证

- `cargo test blueprint`：6 项通过；1 项 Playwright 服务夹具按设计忽略，由浏览器套件启动。
- `cargo test mcp_tests`：11 项；`cargo test local_api_proxy_tests`：7 项；`cargo test plugin_sessions`：2 项。
- `cargo test skill_packages`：14 项；`cargo test declarations`：17 项；`cargo test model_catalog`：3 项。7 组共 60 项通过，Rust 编译 0 warning。
- `npm --prefix frontend run typecheck` 和 `npm --prefix frontend run build` 均通过。
- `npm --prefix frontend run test:blueprint`：Light、Dark 各 1 项通过。用真实 Rust 服务验证导航、空态不写入、建图与多端口节点、鼠标拖线、红线编辑、布局保存和重载、源码行号、AI 草稿以及控制台无报错；另复核了四张截图。
- 扩展 `npm --prefix frontend test`：指定当前仓库的 `PLAYWRIGHT_MODULE` 后，131 项通过。默认环境会从相邻项目导入另一版本的 Playwright，缺少对应 Chromium。
- `git diff --check` 通过。生产代码、资源清单、测试和现行文档无旧插件引用；施工方案、历史文档与下方删除清单保留历史名称。

## 复审要点

- 读写在宿主进程内串行，JSON 单文件以临时文件加重命名原子替换；图、索引和共享实体不是跨文件事务。异常断电或外部并发编辑时，应检查清单和图文件是否一致。
- 锚点使用符号文本、行号和 Git 变更，不是 AST 解析；同名符号可能需要人工确认。跨画布关系来自显式持久化数据，不推断未记录的调用。
- 浏览器验收覆盖 AI 草稿联动和真实 REST 持久化，不发送模型请求；真实模型自主建图效果与桌面打包留待独立验收。

## 文件清单

### 新增（含本记录）

- `docs/BLUEPRINT_IMPLEMENTATION.md`
- `frontend/playwright.blueprint.config.mjs`
- `frontend/src/builtin_plugins/blueprint/BlueprintEdge.tsx`
- `frontend/src/builtin_plugins/blueprint/BlueprintNode.tsx`
- `frontend/src/builtin_plugins/blueprint/InspectorDrawer.tsx`
- `frontend/src/builtin_plugins/blueprint/api.ts`
- `frontend/src/builtin_plugins/blueprint/index.ts`
- `frontend/src/builtin_plugins/blueprint/layout.ts`
- `frontend/src/builtin_plugins/blueprint/page.tsx`
- `frontend/src/builtin_plugins/blueprint/style.module.css`
- `frontend/src/builtin_plugins/blueprint/types.ts`
- `frontend/tests/blueprint-workbench.spec.mjs`
- `src/server/builtin_plugins/blueprint/SKILL.md`
- `src/server/builtin_plugins/blueprint/impact.rs`
- `src/server/builtin_plugins/blueprint/mod.rs`
- `src/server/builtin_plugins/blueprint/model.rs`
- `src/server/builtin_plugins/blueprint/package.rs`
- `src/server/builtin_plugins/blueprint/plugin.json`
- `src/server/builtin_plugins/blueprint/routes.rs`
- `src/server/builtin_plugins/blueprint/store.rs`
- `src/server/builtin_plugins/blueprint/tests.rs`
- `src/server/builtin_plugins/blueprint/tool.json`
- `src/server/builtin_plugins/blueprint/verify.rs`

### 修改

- `README-technical.md`
- `desktop/tauri.conf.json`
- `docker/kylin-arm64/customer/assemble.py`
- `docs/agent-design-guide.md`
- `docs/architecture-v2.md`
- `docs/skill_packages.md`
- `frontend/package.json`
- `frontend/src/App.tsx`
- `frontend/src/builtin_plugins/index.ts`
- `frontend/src/components/ApprovalCards.tsx`
- `frontend/src/components/CodeChatDock.tsx`
- `frontend/src/components/CodexDock.tsx`
- `frontend/src/components/toolVisuals.tsx`
- `frontend/src/dsh/ui-primitives/markdown/CodeBlock.module.css`
- `frontend/src/dsh/ui-primitives/markdown/CodeBlock.tsx`
- `frontend/tests/workbench-model.test.mjs`
- `frontend/vite.config.ts`
- `scripts/skill_packages_smoke.py`
- `src/context/context_assets.rs`
- `src/model/model_catalog.rs`
- `src/runtime/agent_program/declarations.rs`
- `src/runtime/single_agent/context_engine.rs`
- `src/runtime/single_agent/host_tools.rs`
- `src/runtime/single_agent/model_call.rs`
- `src/runtime/single_agent/tool_proxy.rs`
- `src/server/ai_proxy.rs`
- `src/server/builtin_plugins/mod.rs`
- `src/server/parts/local_api_proxy_tests.rs`
- `src/server/parts/mcp_tests.inc.rs`
- `src/server/plugin_sessions.rs`
- `src/server/skill_packages.rs`

### 删除（历史）

- `code-flow.json`
- `frontend/src/builtin_plugins/code-flow/graph.ts`
- `frontend/src/builtin_plugins/code-flow/index.ts`
- `frontend/src/builtin_plugins/code-flow/page.tsx`
- `frontend/src/builtin_plugins/code-flow/style.module.css`
- `frontend/tests/code-flow.test.mjs`
- `src/server/builtin_plugins/code_flow/SKILL.md`
- `src/server/builtin_plugins/code_flow/backend.rs`
- `src/server/builtin_plugins/code_flow/mod.rs`
- `src/server/builtin_plugins/code_flow/plugin.json`
- `src/server/builtin_plugins/code_flow/tool.json`
