# 上下文即代码（Context-as-Code v2）架构规范

更新：2026-11-19  
状态：**已实现并通过单测与真模型联调验证**（`feat/context-as-code-v2`）

> **核心原则**：
> 1. **模型 ABI 是纯 JavaScript，宿主 ABI 是 Rust 工具代理与结构化运行时**：发往模型提供商的请求中 **零 JSON Schema `tools`**；所有宿主能力通过 `system` 里的静态 `.d.ts` 声明暴露，模型直接续写 `async function executeTask() {` 函数体，并以 `return <<<`（零转义 Markdown 终结符）或纯内存 JS `return` 结束本轮。
> 2. **每轮全新空 VM + `user` ↔ `assistant` 成对结构**：每一轮用户请求强制创建空白 `agent_program::Runtime`，轮内代码与中性 `// =>` 轨迹只追加不回写；本轮回复发出时剥离尾部 `EPHEMERAL_PEEK`、配对 `(user #N, assistant #N)` 并立即销毁整个 VM，保证轮内与跨轮 100% 前缀 KV Cache 命中。
> 3. **内外同构、单层隔离 `@virtual` 栈帧 + 惰性资源句柄（`ResourceHandle`）**：复杂语义判断与并发扇出通过单层隔离 `@virtual` 函数完成，返回即 GC 并向父轨迹写入事实回执；大文件与大输出以 `Ref#N` 句柄管理，支持按需分页、跨 `@virtual` 零拷贝透传，超 24KB 自动归档 `asset_id` 供跨轮取回。

---

## 1. 模型协议（Pure JS + `return <<<`）

每个模型步（Step）只允许以下两种输出形态之一（[src/model_stream.rs](../src/model_stream.rs)）：

1. **执行纯 JS 代码段**（无 Markdown 代码块围栏、无 XML 标签、无伪造 `// =>`）
   - 宿主通过 SWC 编译并执行在当前轮活体 `Runtime` 中，所有 `io.*` / `git.*` / `process.*` / `host.*` / `session.*` 调用在 VM 中挂起为 `ActionKind::Tool` 并代理到 Rust [host_tools.rs](../src/single_agent/host_tools.rs)（完整复用权限、沙箱与文件基线校验）。
   - 非 JS Markdown 围栏（如直接输出 ```` ```rust ... ``` ````）在编译前即被拦截为 `MARKDOWN_NOT_JS`，避免被 JS 语法误解析为标签模板字面量。
2. **向用户交付最终 Markdown 回复（`return <<<`）**
   - 以 `return <<<` 开头，其后直至 EOF 的全部原始内容作为用户 Markdown 原样直出，**绝不作为 JS 执行**（因此展示 ```` ```js ```` / ```` ```rust ```` 示例代码无需任何转义）。

### 1.1 循环终止与 First-Trap Truncation 规则

驱动循环是否终止仅看一个硬指标：**本步执行期间是否触发了外部宿主/虚函数/库调用（`external_tool_calls > 0`）**：

- **`external_tool_calls == 0`**（纯 `return <<<`、纯内存 JS 投影 + `return <<<`、或纯内存显式 `return <expr>;`）：立即落盘永久 `// =>`、发出 `assistant/message`、剥离 `EPHEMERAL_PEEK`、销毁本轮 VM 并结束本轮。
- **`external_tool_calls > 0`**（如调用了 `io.read`、`io.write`、`process.*`、`@virtual` 或 `require` 库方法）：即使模型在同一回复尾部抢跑拼接了 `return <<<`，宿主也只执行前半段真实 JS，**丢弃抢跑的 `return <<<`**，将真实结果放进 `// =>` / `EPHEMERAL_PEEK` 后进入下一步（First-Trap Truncation）。

---

## 2. 上下文布局与三级值展示（100% KV Cache 友好）

发往 Provider 的消息始终保持 `[system, user #1, assistant #1, ..., user #N]`（[src/single_agent/context_doc.rs](../src/single_agent/context_doc.rs)）：

| 区域 | 内容 | 生命周期 | KV Cache |
| --- | --- | --- | --- |
| `system` | 固定协议说明 + 静态 `.d.ts`（含 `interface ResourceHandle`）+ 可 `require` 编排库清单 | 会话内恒定 | 100% 命中 |
| `user #t` / `assistant #t` (`t < N`) | 历史轮：用户原话 + 干净纯代码 + 中性永久 `// =>` 轨迹 / 原生最终回答 | 永久冻结 | 跨轮 100% 命中 |
| `user #N` 上半部 | 当前轮：用户原话 + `async function executeTask() {` + 只追加代码段与永久 `// =>` | 只追加（Append-Only） | 轮内每步命中 |
| `user #N` 下半部 | `EPHEMERAL_PEEK`（仅本步可见的临时展开 / 大索引 / 编译报错） | 仅存活 1 步，下步替换、轮末剥离 | 仅尾部重算 |

### 2.1 三级值分级与防作弊锁

| 级别 | 字符数 | 永久 `// =>` 轨迹 | `EPHEMERAL_PEEK`（仅 1 步） |
| --- | --- | --- | --- |
| Tier 1（短值） | `<= 2,000` | 原文写入 `// =>`（同一步长值重复别名自动折叠为 `(same as <var>)`） | 无 |
| Tier 2（中值） | `2,001..=24,000` | 中性单行句柄签名，如 `[File("src/auth.rs") \| Ref#1 \| 61 lines, 3722 chars \| .slice() .grep() .outline()]` | 完整带行号源码（同一步跨别名去重，仅展开 1 次；若当步已传给 `@virtual` 则**不展开**） |
| Tier 3（大值） | `> 24,000` | 中性单行句柄签名 + 持久化资产号 `\| asset: asset-...` | 高熵结构索引（符号大纲 / 错误行 + 首尾行），引导用句柄方法按需分页 |

- **中性签名原则**：永久 `// =>` 不出现 `live in JS memory` 等会随轮次封存而失真的词，保证历史字节零改写且语义严谨。
- **反作弊切片锁**：尝试在 JS 里把 `> 2KB` 的字符串整体切片赋值给新变量时，同样落入 Tier 2/3，不会灌入永久轨迹。

---

## 3. 惰性资源句柄（Lazy Resource Handles）

文件读取（`io.read` / `io.read_lines` / `io.read_chunk`）与大命令输出（`> 2KB`）自动打标单调句柄号 `"__ref": "Ref#N"`（[src/agent_program/js_methods.rs](../src/agent_program/js_methods.rs)、[src/agent_program/runtime.rs](../src/agent_program/runtime.rs)）：

1. **零 I/O 内置分页方法**（通过 `Effect::Invoke` 定向派发，不污染 `Object.prototype`）：
   - `h.slice(startLine, endLine?)`：1-based 含头尾行号切片；支持**多区间数组** `h.slice([[1, 10], [260, 265]])`（区间之间以 `// ... (skipped N lines) ...` 连接）。
   - `h.grep(pattern, contextLines?)`：大小写不敏感子串或 `/regex/` 检索，附上下文行（默认 1 行，输出封顶 60 行）。
   - `h.outline()`：提取 Rust/TS/JS/Python/Go/Markdown 符号与标题大纲（带 1-based 行号）。
   - `h.head(n?)` / `h.tail(n?)`：首/尾 `n` 行（默认 30 行）。
2. **Pass-by-Handle 零拷贝透传与自动解引用**：
   - 外层把 `ResourceHandle`（或句柄数组、`{ file, content }`）传给 `@virtual` 时，该 `Ref#` 与内容哈希标记为已委派：**外层 `EPHEMERAL_PEEK` 跳过原始文本展开**，外层 `[virtual ...]` 回执参数记为 `Ref#1("src/auth.rs")`；
   - `@virtual` 子帧在 Step 1 自动将该参数解引用为带行号的干净源码视图，并可继续直接调用 `h.grep()` / `h.slice()`。

---

## 4. 内外同构的隔离 `@virtual` 栈帧与并发控制

顶层 JS 中可通过 `/** @virtual <契约；建议要求返回 summary 字段> */ async function fn(arg) {}`（可选 `@uses outerVar`）声明单层虚函数（[src/single_agent/virtual_frame.rs](../src/single_agent/virtual_frame.rs)）：

- **严格上下文与权限隔离**：子帧仅看到自身函数签名、契约注释、绑定参数、显式 `@uses` 变量与剥离了 `process` / `session` 的非 shell `.d.ts`；绝不继承外层历史、外层未捕获变量或 `MAIN_PROMPT`。
- **硬拦截红线**：
  - 禁止嵌套 `@virtual`（`VIRTUAL_LAYER_LIMIT`）
  - 禁止一切 shell 工具（`VIRTUAL_SHELL_FORBIDDEN`）
  - 禁止子帧使用 `return <<<`（`VIRTUAL_RETURN_REQUIRED`，必须原生 JS `return <json>;`）
  - 禁止子帧直接调用 `host.ask_user`（`VIRTUAL_ASK_USER_FORBIDDEN`，应返回 `{ "$task_error": "..." }` 抛给外层 `try / catch`）
- **真并发与内存安全护栏**：外层 `drive_vm_segment` 使用 `runtime.next_concurrent()` + `tokio::task::JoinSet` 支撑 `Promise.all` 真并发；严格按 `is_async_inflight` 区分同步/异步 action（同步调用立即执行并 `resume_action`，异步调用才进 `JoinSet`），叠加 `spawned_ids` 去重与 **64 并发上限**，杜绝任务风暴。
- **异构模型路由**：支持通过 `delegate_agent_settings` 将 `@virtual` 子帧路由到高性价比快速模型（如外层 `gpt-6.1-terra` + 子帧 `deepseek-flash`）。
- **栈帧 GC 与事实回执**：子帧 `return` 瞬间销毁整个子 `Runtime` 与 `ContextDocument`，并向父轨迹写入运行时真实统计回执：
  ```js
  // => [virtual inspect(Ref#1("src/auth.rs")) | 1 step(s) | read: src/auth.rs | returned: { summary: "...", ... }]
  ```

---

## 5. 过程代码复用、持久化与回滚

1. **三层代码复用**（[src/single_agent/flow_runtime.rs](../src/single_agent/flow_runtime.rs)）：
   - **同轮复用**：同轮活体 VM 内的普通 JS 函数/常量与 `Promise.all` 批量执行；
   - **跨轮参考**：历史轮只留干净代码 + `// =>`，新轮空白 VM 按需摘抄组合重跑（禁止为找旧值重跑有副作用的写入/命令）；
   - **跨会话模块复用**：通过 `require("<name>")`（同步）或 `require("<name>/promises")`（异步并发）加载内置预设库（`batch-files`、`repo-verify`）、项目技能库（`.agents/skills/<name>/modules/*.js`）或已启用插件模块，每个库调用在独立 Layer-1 帧运行（上限深度 4、动作上限 256），并在主轨迹留 `[library <lib>.<method> | ...]` 事实回执。
2. **大产物持久化归档与跨轮取回**：
   - `> 24KB` 的成功工具结果自动通过 `context_assets::register` 落盘为内容寻址资产，永久 `// =>` 附带 `asset: asset-...`，跨轮可通过 `session.read_asset({ asset_id, char_offset, max_chars })` 按偏移分页读取。
3. **用户澄清挂起（`host.ask_user` / `NEEDS_USER`）**：
   - 外层调用 `host.ask_user({ question })` 时不可被 JS `try/catch` 吞掉，宿主记录 `agent/needs_user` 事件与 `// [paused for user input via host.ask_user]`，将问题直接作为本轮 `assistant/message` 发出并销毁本轮 VM，下一轮用户回答后自然接续。
4. **每轮脚本归档与 `rewind` 回滚**：
   - 每轮完成自动将干净 JS 轨迹归档为 `host_folder` 下的 `turn-NN.js`；`crate::rewind::rewind(root, task, turn)` 同步恢复磁盘文件快照与 `Window.context_doc` 历史状态。

---

## 6. 核心模块索引与测试覆盖

| 模块 | 职责 | 验证套件 |
| --- | --- | --- |
| [src/model_stream.rs](../src/model_stream.rs) | 裸 JS / `return <<<` / `CodeAndReply` / 非 JS 围栏守卫 | `model_stream::tests::run_` |
| [src/single_agent/context_doc/mod.rs](../src/single_agent/context_doc/mod.rs) | 不可变追加轨迹与 Wire 渲染（配套 `format.rs`、`indexers.rs`） | [step1_protocol.rs](../src/single_agent/tests/step1_protocol.rs), [step3_handles.rs](../src/single_agent/tests/step3_handles.rs) |
| [src/single_agent/model_call.rs](../src/single_agent/model_call.rs) | 零 `tools` Wire 请求构建、静态 `.d.ts` 与编排库声明 | [step1_protocol.rs](../src/single_agent/tests/step1_protocol.rs) |
| [src/single_agent/virtual_frame.rs](../src/single_agent/virtual_frame.rs) | 同构隔离 `@virtual` 帧、异构路由、栈帧 GC、事实回执 | [step2_virtual.rs](../src/single_agent/tests/step2_virtual.rs), [live_models.rs](../src/single_agent/tests/live_models.rs) |
| [src/agent_program/resource_handle.rs](../src/agent_program/resource_handle.rs) | `ResourceHandle` `.slice()`/`.grep()`/`.outline()`/`.head()`/`.tail()` | [step3_handles.rs](../src/single_agent/tests/step3_handles.rs), `agent_program::tests::` |
| [src/single_agent/flow_runtime.rs](../src/single_agent/flow_runtime.rs) | `require("<lib>")` / `ActionKind::LibraryCall` / `host.*` | [step4_durability.rs](../src/single_agent/tests/step4_durability.rs) |
| [src/single_agent/vm_driver.rs](../src/single_agent/vm_driver.rs) | VM 片段驱动、`Promise.all` 并发 `JoinSet`、同步/异步分流与并发护栏 | [step2_virtual.rs](../src/single_agent/tests/step2_virtual.rs), [step4_durability.rs](../src/single_agent/tests/step4_durability.rs) |
| [src/single_agent/tool_proxy.rs](../src/single_agent/tool_proxy.rs) | Rust 宿主工具代理、`ask_user`、`> 24KB` 持久化资产归档 | [step4_durability.rs](../src/single_agent/tests/step4_durability.rs) |
| [src/single_agent.rs](../src/single_agent.rs) | v2 顶层会话主循环、First-Trap Truncation、每轮 VM 生命周期 | [tests/mod.rs](../src/single_agent/tests/mod.rs) |
