# 主会话右侧只读文档预览抽屉与五件套统一预览协议计划

> 目标体验：在 QuickAgent 主会话（及附件卡片、变更检查器）中点击 `.pdf` / `.docx` / `.xlsx` / `.pptx` / `.md`（以及图片/常见源码文本）引用时，右侧抽屉平滑展开并以**多标签只读形式直接渲染**该文件，实现“左手与 AI 对话、右手实时审阅成品”；点击右上角按钮可**一键在办公创作台打开编辑**。

---

## 1. 总体架构与分工

系统分为 **Part 1（`codex-workspace-mcp` 宿主通用底座）** 与 **Part 2（办公五件套插件只读模式适配）**。Part 1 完成后 Markdown / 图片 / 代码预览立刻生效，Part 2 可按插件仓库逐个或并行接入。

---

## 2. 宿主与插件统一预览协议（v1）

### 2.1 URL 启动参数
宿主加载插件只读预览 iframe 时传入：
```text
/agent/skill-packages/{id}/pages/studio/index.html
  ?mode=preview
  &file=/agent/tasks/{task_id}/workspace-file?path=docs%2Freport.pdf
  &name=report.pdf
  &theme=light|dark
```
- **`mode=preview`**：进入只读模式，隐藏新建、打开、保存、导出与写操作工具栏，只保留克制的只读浏览控件（PDF：页码/缩放/目录/搜索；Excel：底部多 Sheet 切换/只读公式栏/选区聚合状态栏；Word：大纲/A4 阅读；PPT：缩略图/翻页/缩放）。
- **`file=<same_origin_path>`**：同源 `/agent/...` 相对路径（`/agent/tasks/{id}/workspace-file?path=...` 或 `/agent/tasks/{id}/attachments/{receipt}`）。**插件侧必须校验 `new URL(file, location.origin).origin === location.origin && pathname.startsWith('/agent/')`**，拒绝任何外部源或非 `/agent/` 路径。
- **`theme=light|dark`**：首帧即与宿主主题同步。

### 2.2 宿主 ↔ 插件同源 `postMessage` 协议
- 宿主 → 插件：
  - `{ type: 'quickagent:theme', theme: 'light' | 'dark' }`：实时切换亮暗主题，不重载 iframe；
  - `{ type: 'quickagent:reload-file', file: string, name: string }`：AI 改写当前预览文件时静默重载，尽量保留页码/Sheet；
  - `{ type: 'quickagent:open-file', file: string, name: string }`：用于在完整办公创作台（`OfficeStudioView`）中承接“一键在工作台编辑”，让已保活或新开的编辑器直接载入该文件。
- 插件 → 宿主：
  - `{ type: 'quickagent:preview-ready', kind: string, meta?: { pages?: number; sheets?: number } }`
  - `{ type: 'quickagent:preview-error', message: string }`

---

## 3. Part 1：QuickAgent 宿主侧实现（`codex-workspace-mcp`，分支 `feat/context-as-code-v2`）

### 3.1 后端安全只读文件流接口（支持 HTTP `Range`）
- **新增路由**（`src/server/ai_proxy.rs` + `src/server/agent_service/routes.rs`）：
  - `GET /agent/tasks/{task_id}/workspace-file?path=<path>`（同时支持 `HEAD`）
- **安全与能力要求**：
  1. 根据 `task_id` 解析该任务绑定的工作区根目录 `task_workspace`（不存在返回 `404`）；
  2. 支持相对路径或位于 `task_workspace` 内的绝对路径（规范化 `file://` 前缀），使用 `cap_std::fs::Dir` / `open_dir_nofollow` / `FollowSymlinks::No` 严格阻断 `..` 穿越、工作区外路径、符号链接逃逸与非普通文件；
  3. 完整支持单区间 HTTP `Range: bytes=start-end`：
     - 无 `Range`：`200 OK` + `Accept-Ranges: bytes` + `Content-Length` + `ETag` + `Cache-Control: no-store` + 准确 `Content-Type` + 流式 `ReaderStream`（零全内存缓冲）；
     - 合法 `Range`：`206 Partial Content` + `Content-Range: bytes start-end/total` + 精确字节流（供大 PDF 按 256 KiB 分块拉取）；
     - 非法 `Range`：`416 Range Not Satisfiable` + `Content-Range: bytes */total`；
  4. 检查 `/agent/tasks/{task_id}/attachments/{receipt}`：若尚不支持 `Range` / `HEAD`，一并补齐以支持大 PDF 附件按需预览。

### 3.2 前端右侧多标签预览抽屉升级
- **新增分派模块 `frontend/src/filePreview.ts`**：
  - 统一根据文件扩展名与已启用插件列表决定渲染器类型：
    - `markdown`（`.md/.markdown`）：宿主内置 `MarkdownText` + KaTeX + Mermaid 只读视图（秒开零 iframe 开销）；
    - `image`（`.png/.jpg/.jpeg/.webp/.gif/.svg`）：内置居中缩放查看器；
    - `plugin`（`.pdf` → `pdf`、`.xlsx` → `excel`、`.docx` → `word`、`.pptx` → `ppt`）：若对应随包插件已启用，渲染 `?mode=preview&file=...` iframe；
    - `code`（常见源码/配置/纯文本，限制前 512 KiB）：内置带行号的只读代码视图，支持跳转到 `line` 并高亮该行；
    - `unsupported`：展示清晰元信息卡片 + 下载 / 复制路径按钮。
- **升级 `frontend/src/components/CodexRightPanel.tsx` 与 `frontend/src/App.tsx`**：
  1. 右侧面板顶栏支持 **「预览」与「变更」切换**，预览模式自带多文件 Tab 栏（图标、文件名、关闭 `×`）、路径提示、刷新、复制路径、以及**“在办公创作台编辑”**按钮；
  2. 放宽 `App.tsx` 右侧拖拽上限至 `Math.min(Math.floor(window.innerWidth * 0.65), 1100)`；打开文档预览且当前宽度 `< 520px` 时自动展开至舒适宽度（如 `560px`）；
  3. 切换宿主亮暗主题时向当前打开的预览 iframe 广播 `quickagent:theme`；
  4. 监听会话变更事件（`turn/end` / `workspace/changes`），自动通知已打开的对应预览标签静默刷新。
- **接入全部点击入口**：
  1. `frontend/src/components/ChatView.tsx` 的 `openFile(path, { line })`（同时覆盖 AI 回复 Markdown 链接与用户 `@文件` 引用）：点击直接在右侧抽屉展开对应文件预览（不再仅复制路径）；
  2. `frontend/src/components/AttachmentCard.tsx`：对可预览非图片附件（`.pdf/.docx/.xlsx/.pptx/.md` 等），点击卡片直接用 `file.url` 在右侧只读预览，保留独立小下载按钮；
  3. `frontend/src/components/ChangesCard.tsx`：变更文件项增加“预览”入口按钮；
  4. `frontend/src/components/OfficeStudioView.tsx`：支持接收 `pendingOpen?: { slotId, fileUrl, filename, seq }`，响应“在办公创作台编辑”自动切换并把文件传入对应模块（URL 参数 + `quickagent:open-file`）。

---

## 4. Part 2：办公五件套插件适配清单（各独立仓库可并行）

1. **`/home/goodtime/project/pdf-editor-engine` (`pdf`)**：
   - 响应 `?mode=preview&file=...&name=...&theme=...`：隐藏打开/示例/导出与批注/页修改按钮，保留页码、缩放、目录、搜索；
   - `file` 参数经同源校验后，直接交给 `pdf.js` `getDocument({ url, rangeChunkSize: 256 * 1024, disableAutoFetch: true, disableStream: true })`（走宿主 `206 Range`，大 PDF 零拷贝按需秒开）；
   - 同时支持 `mode=edit`（或无 `mode`）带 `file` 参数与 `postMessage('quickagent:open-file')` 直接载入编辑，支持 `quickagent:theme` 与 `quickagent:reload-file`。
2. **`/home/goodtime/project/excel-editor-engine` (`excel`)**：
   - 响应 `mode=preview`：隐藏写操作工具栏与导出按钮，禁止单元格进入编辑态；保留只读公式栏、多工作表切换、滚览、查找与选区聚合状态栏；
   - 校验同源 `file` 后 `fetch(file)` 并交给现有 `api.parse()` 渲染；支持 `quickagent:theme` / `quickagent:reload-file` / `quickagent:open-file`。
3. **`/home/goodtime/project/docx-editor-engine` (`word-editor` & `markdown-editor`)**：
   - `word-editor`：响应 `mode=preview` 隐藏格式/保存工具栏、设只读阅读态、保留大纲与 A4 页面，校验同源 `file` 后 `fetch(file)` 经 `word-server` 解析渲染；
   - `markdown-editor`：支持 `file` 参数与 `postMessage('quickagent:open-file')` 拉取纯文本载入双栏编辑器（用于从宿主只读预览一键转编辑）。
4. **`/home/goodtime/project/pptx-editor-engine` (`ppt`)**：
   - 响应 `mode=preview`：隐藏属性编辑栏与新建/导出按钮，保留缩略图导航、WASM 主画布渲染、上下翻页与缩放；校验同源 `file` 后拉取 `.pptx` 载入渲染；支持 `quickagent:theme` / `quickagent:reload-file` / `quickagent:open-file`。

---

## 5. 验收标准

### Part 1（`codex-workspace-mcp`）
- **Rust 测试**：`workspace-file`（及 `attachments/download`）覆盖 `200` 全量流、`206` 单区间 Range、`416` 非法 Range、`HEAD`，以及 `..` 穿越、工作区外路径、符号链接逃逸、目录拒绝、未知 `task_id` 404；编译 0 警告。
- **前端测试**：`npm --prefix frontend run typecheck` 0 错误；新增 `filePreview` 单测与 Playwright 真浏览器测试（点击对话文件链接/附件卡片展开右侧抽屉、多 Tab 切换与关闭、行号定位高亮、切回变更检查器、宽抽屉拖拽）。

### Part 2（各插件仓库）
- 各仓库原有 Rust / TS / 浏览器测试 100% 通过，新增 `mode=preview` 自动载入同源 `file` 与拒绝跨域 `file` 校验，重新打包至 `bundled_plugins/` 后在桌面端主会话点击 5 类文档实测流畅渲染与一键转编辑。

---

## 6. 执行约束
- `codex-workspace-mcp` 严格保持在 `feat/context-as-code-v2` 分支，**禁止碰 `master`**；
- 编译前用 `df -h .` 检查可用空间 > 20 GB；
- **不要终止正在运行的 `grow-desktop` 进程**，完成代码与测试后统一由宿主构建同步与重启验收。
