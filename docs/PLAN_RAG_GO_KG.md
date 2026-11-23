# `rag-go` 知识图谱（LightRAG Sidecar + Draft/Prod 治理 + QuickAgent 远程插件与反向供模）施工计划

> 适用仓库：`rag-go`（**本任务只改 `rag-go` 仓，不改 `codex-workspace-mcp` 源码**；QuickAgent 侧控制反转能力已拆为独立任务 `codex-workspace-mcp/docs/PLAN_EXTERNAL_MODEL_IOC.md`，两仓通过第 3.2、3.3 节锁死的 HTTP 契约对接）。
>
> 角色分工：本计划作为 GPT 开发施工规格书（逐项实现、提交代码与自测）；完成后交由 Antigravity（验收方）逐项执行代码审查、契约核验、端到端实测与回归验收。
>
> 核心前提：201 服务器推理网关 `http://10.10.10.201:12888/v1` 已切换为 `qwen3.8-27b`（双 TP2 副本，支持原生 tool calling，支持顶层 `chat_template_kwargs: {"enable_thinking": false}` 关闭思考；开工前以 `GET /v1/models` 实际返回为准）。

---

## 0. 全局红线与约束（必须严格遵守）

1. **绝不阻塞或破坏主入库链路**：现有 `AnyDoc 解析 -> 切片 -> Embedding -> Milvus + ES -> 快照` 主流程保持原样；知识图谱抽取走异步旁路，LightRAG 故障或 `LIGHTRAG_ENABLED=false` 时不影响文档上传、检索与问答。
2. **双空间硬隔离（Draft / Prod）**：
   - 抽取结果只写 `draft`；经质检与人工后编辑确认后，增量合入 `prod`。
   - 所有问答与 Agent 工具严格只读 `prod`，绝不读 `draft`。
3. **不破坏现有 OpenAPI 校验与回归测试**：
   - `internal/server/server.go` 中 `validateNativeOperations` 硬校验 `openapi/openapi.json` 恰好 45 个操作；新增 `/api/v1/kg/*`、`/kg-plugin/*` 与 `/internal/v1/kg/llm/*` 路由只注册到 `mux`，不要改动原有 45 个 OpenAPI 契约，确保 `go test ./...` 100% 通过。
4. **多知识库（`library_name`）字段从 Day 1 预留**：
   - `rag-go` 全链路按 `library_name` 隔离（默认 `"default"`）。MySQL 表、Neo4j 节点/边属性、Go API、QuickAgent 工具入参必须保留 `library_name` 字段（默认 `"default"`）。
5. **vLLM 关闭思考的顶层 JSON 约束**：
   - 发往 vLLM `/v1/chat/completions` 的 HTTP JSON 请求体中，`chat_template_kwargs` 必须序列化在**最外层**（禁止错误嵌套进 `extra_body` JSON 键）：
     ```json
     {
       "model": "qwen3.8-27b",
       "messages": [...],
       "chat_template_kwargs": { "enable_thinking": false }
     }
     ```
   - `lightrag_service/` 直接用 `httpx.AsyncClient` 自定义 `llm_model_func` 与 `embedding_func`，发包结构透明且与 `internal/server/llm_client.go` 完全一致，并对返回正文兜底剥离 `<think>...</think>`。

---

## 1. 总体架构与跨仓契约边界

```text
文档上传 ──▶ rag-go 主入库 (AnyDoc -> Milvus + ES -> MySQL Manifest) ──▶ 立即可搜 (/api/v1/retriever)
                  │
                  │ (主入库成功且写入快照后，异步投递，绝不阻塞)
                  ▼
         MySQL: rag_kg_jobs ──▶ Go KG Worker Pool (并发=LIGHTRAG_EXTRACT_WORKERS, 默认2)
                                      │ (直接从 chunkStore/版本快照读取已入库文本，不碰临时目录)
                                      ▼
                            lightrag_service (:8021, FastAPI + lightrag-hku)
                            ├── 全局 asyncio.Semaphore 限制对 201:12888 总并发 (默认 3)
                            ├── draft 实例 (working_dir=data/lightrag/draft, Neo4j workspace=kg_draft, Milvus 前缀隔离)
                            └── prod  实例 (working_dir=data/lightrag/prod,  Neo4j workspace=kg_prod,  Milvus 前缀隔离)

人工治理闭环：
  rag-go Go API (/api/v1/kg/draft/inspect|merge|edit|publish|reject)
     ├── 直连 Neo4j (neo4j-go-driver/v5) 质检
     ├── 代理 lightrag_service 执行 merge / edit / publish(增量合并+精确清理draft) / delete
     └── MySQL 持久化 rag_kg_publish_batches / rag_kg_audit_logs / rag_kg_aliases

问答与编排驱动（控制反转：rag-go 仅暴露 HTTP 契约，不改 QuickAgent 源码）：
  QuickAgent (:3001)
     ├── 反向供模请求 (Bearer + x-agent-affinity + SSE)
     │     ──▶ rag-go (:9004) /internal/v1/kg/llm/{models,chat/completions} ──▶ 201:12888 (qwen3.8-27b)
     └── Remote Plugin 协议 (POST /kg-plugin/api/plugin/{handshake,confirm,call}, GET /kg-plugin/resource)
           ──▶ rag-go (:9004) 4 个只读 prod 原子工具 (library = "kg")
                ├── kg.search_entities    (Neo4j prod + rag_kg_aliases + Milvus prod)
                ├── kg.entity_events      (Neo4j prod 双向邻接 + 溯源)
                ├── kg.reverse_mentions   (Neo4j prod 双向邻接 + 提及/评价过滤 + 溯源)
                └── kg.search_chunks      (复用 rag-go 现有 Milvus+ES 混合检索)
```

---

## 2. 分阶段施工清单（严格按顺序交付，每阶段自测通过再进入下一阶段）

### 阶段 1：独立 Python Sidecar `lightrag_service/`

参考 `anydoc_service/` 和 `hermit_crab/` 结构新建 `lightrag_service/`（含 `main.py`、`config.py`、`kg_engine.py`、`requirements.txt`、`README.md`）。

#### 1.1 配置与存储初始化
- 环境变量：
  - `LIGHTRAG_HOST`（默认 `127.0.0.1`）、`LIGHTRAG_PORT`（默认 `8021`）
  - `LIGHTRAG_INTERNAL_API_KEY`（可选，配置时校验 `X-LightRAG-Internal-Key`）
  - `LIGHTRAG_DATA_DIR`（默认 `data/lightrag`，下分 `draft/`、`prod/` 两个持久化 `working_dir`，保存 KV 与 DocStatus 存储）
  - `NEO4J_URI`（默认 `bolt://10.10.10.201:7687`）、`NEO4J_USERNAME`、`NEO4J_PASSWORD`
  - `MILVUS_URI`（默认 `http://10.10.10.201:19530`）、`MILVUS_TOKEN`、`MILVUS_DB_NAME`（默认 `default`）
  - `LIGHTRAG_LLM_BASE_URL`（默认 `http://10.10.10.201:12888/v1`）、`LIGHTRAG_LLM_API_KEY`、`LIGHTRAG_LLM_MODEL`（默认 `qwen3.8-27b`）、`LIGHTRAG_ENABLE_THINKING`（默认 `false`）
  - `LIGHTRAG_EMBEDDING_BASE_URL`（默认 `http://10.10.10.201:9101/mass/v1`）、`LIGHTRAG_EMBEDDING_API_KEY`、`LIGHTRAG_EMBEDDING_MODEL`、`LIGHTRAG_EMBEDDING_DIM`（默认 `1024`）
  - `LIGHTRAG_LLM_MAX_ASYNC`（默认 `3`）、`LIGHTRAG_EMBEDDING_MAX_ASYNC`（默认 `4`）、`LIGHTRAG_GLOBAL_LLM_CONCURRENCY`（默认 `3`，进程级 `asyncio.Semaphore`）
  - `LIGHTRAG_ENTITY_TYPES`（默认 `Person,Event,Time,Place,Work`）
- LightRAG 初始化关键要求：
  - 同时初始化并持有两个实例：`draft`（`workspace="kg_draft"`，`working_dir=<data_dir>/draft`）与 `prod`（`workspace="kg_prod"`，`working_dir=<data_dir>/prod`）。
  - 后端固定 `graph_storage="Neo4JStorage"`、`vector_storage="MilvusVectorDBStorage"`。
  - Milvus 集合带 `kg_draft` / `kg_prod` workspace 前缀，绝不触碰 `rag-go` 现有 `<library>_text`、`<library>_image`、`rag_category_tags`。
  - `addon_params` 显式配置：`"language": "Simplified Chinese"`、`"entity_types": entity_types`。
  - 自定义 `llm_model_func`：用 `httpx.AsyncClient` 直接 `POST {base}/chat/completions`，顶层传 `"chat_template_kwargs": {"enable_thinking": LIGHTRAG_ENABLE_THINKING}`，受进程级 `asyncio.Semaphore` 约束，对响应剥离 `<think>...</think>`。
  - 自定义 `embedding_func`：用 `httpx.AsyncClient` `POST {base}/embeddings`，返回维度为 `LIGHTRAG_EMBEDDING_DIM` 的 numpy array。

#### 1.2 HTTP 接口契约
| 方法与路径 | 说明 |
|---|---|
| `GET /health` | 返回 `{"status":"ok","neo4j_ok":true,"milvus_ok":true,"workspaces":["kg_draft","kg_prod"]}`；依赖异常返回 503 与具体错误。 |
| `POST /internal/v1/shutdown` | 供 Go Sidecar 优雅退出（参照 `anydoc_service`）。 |
| `POST /internal/v1/kg/extract` | 入参：`{doc_id, doc_version, library_name, file_name, text, chunks?, entity_types?}`。以幂等 ID（`f"{library_name}:{doc_id}:{doc_version}"`）写入 `draft`，并将 `doc_id`/`doc_version`/`library_name`/`file_name` 写入文档元信息及 Neo4j 节点/边溯源属性。 |
| `GET /internal/v1/kg/graphs?workspace=draft\|prod&library_name=default&limit=200` | 返回 `{nodes: [...], edges: [...]}` 供预览与审核。 |
| `POST /internal/v1/kg/merge` | 入参：`{workspace, source_entities: ["沛公","汉高祖"], target_entity: "刘邦", target_entity_data?}`。调 LightRAG `amerge_entities`，同步更新 Neo4j 与 Milvus 向量。 |
| `POST /internal/v1/kg/edit` | 入参：`{workspace, action: "update"\|"delete", target_type: "entity"\|"relation", entity_name?, source_id?, target_id?, updates?}`。同步更新 Neo4j 与 Milvus。 |
| `POST /internal/v1/kg/publish` | 入参：`{batch_id, doc_ids?, entities?, relationships?, chunks?, library_name}`。要求：<br>1. 按 `doc_ids`（或全部待发布）从 `draft` 提取实体/关系/chunks；<br>2. **增量合并防覆盖**：写 `prod` 前查同名实体/同源目标边，若存在则将 `description`/`keywords`/`source_id`/`file_path` 去重合并（`<SEP>` 拼接）再调 `prod.ainsert_custom_kg` 同步落 Neo4j 与 Milvus；<br>3. **精确清理 draft**：仅清本次已发布的 `doc_ids`/节点与边，不误删未审核文档。 |
| `DELETE /internal/v1/kg/documents/{doc_id}?workspace=draft\|prod&doc_version=...` | 调 `adelete_by_doc_id` 清理指定文档的图谱与向量数据。 |
| `POST /internal/v1/kg/query` | 入参：`{query, mode: "local"\|"global"\|"hybrid"\|"mix", top_k?, only_need_context?}`，强制只查 `prod`。 |

---

### 阶段 2：`rag-go` 配置、Sidecar 托管、MySQL 治理表与并行入库

#### 2.1 配置与 Sidecar 托管
- 修改 `internal/config/config.go` 与 `.env.example`，新增：
  - `LIGHTRAG_ENABLED`（默认 `false`）、`LIGHTRAG_REQUIRED`（默认 `false`）、`LIGHTRAG_BASE_URL`（默认 `http://127.0.0.1:8021`）、`LIGHTRAG_MANAGED`（默认 `true`）
  - `LIGHTRAG_EXECUTABLE`、`LIGHTRAG_PYTHON`、`LIGHTRAG_INTERNAL_API_KEY`、`LIGHTRAG_DATA_DIR`、`LIGHTRAG_LOG_DIR`
  - `LIGHTRAG_STARTUP_TIMEOUT_SECONDS`（默认 `90`）、`LIGHTRAG_SHUTDOWN_TIMEOUT_SECONDS`（默认 `15`）、`LIGHTRAG_RESTART_LIMIT`（默认 `3`）
  - `LIGHTRAG_EXTRACT_WORKERS`（默认 `2`）、`LIGHTRAG_TIMEOUT_SECONDS`（默认 `600`）
  - `NEO4J_URI`、`NEO4J_USERNAME`、`NEO4J_PASSWORD`、`NEO4J_DATABASE`（默认 `neo4j`）
  - `KG_PLUGIN_TOKEN`（QuickAgent 远程插件与反向供模鉴权 Bearer Token，可选）
  - `QUICKAGENT_BASE_URL`（可选，用于 `/api/v1/kg/agent-chat` 或启动时可选自动注册）
- 参考 `internal/sidecar/anydoc.go` 新增 `internal/sidecar/lightrag.go`（`LightRAGManager`），并在 `cmd/rag-go/main.go` 接入启动与优雅关闭。

#### 2.2 MySQL 新增 4 张治理表（`internal/server/kg_store.go`）
自动建表（`CREATE TABLE IF NOT EXISTS`，`ENGINE=InnoDB DEFAULT CHARSET=utf8mb4`）：
1. `rag_kg_jobs`：`job_id` PK、`library_name`、`doc_id`、`doc_version`、`file_name`、`status`（`queued`/`running`/`pending_review`/`published`/`rejected`/`failed`）、`attempts`、`error_text`、`created_at`、`updated_at`；`UNIQUE KEY uk_kg_job_doc_ver (library_name, doc_id, doc_version)`，`KEY idx_kg_jobs_status (status, created_at)`。
2. `rag_kg_publish_batches`：`batch_id` PK、`library_name`、`version`、`operator`、`summary`、`doc_ids_json`、`entity_count`、`relation_count`、`created_at`。
3. `rag_kg_audit_logs`：`id` AUTO_INCREMENT PK、`library_name`、`batch_id`、`action`（`extract`/`merge`/`edit`/`reject`/`publish`/`delete`）、`target_type`、`target_id`、`detail_json`、`operator`、`created_at`。
4. `rag_kg_aliases`：`id` AUTO_INCREMENT PK、`library_name`、`alias_name`、`canonical_name`、`operator`、`created_at`；`UNIQUE KEY uk_kg_alias (library_name, alias_name)`。

#### 2.3 并行异步入库 Worker
- 挂载点：在 `internal/server/document_mutations.go` 主入库完成并写入 `ingest_ready` 快照后，当 `cfg.LightRAGEnabled == true` 时非阻塞入队到固定 Worker Pool（并发 `LIGHTRAG_EXTRACT_WORKERS`）。
- 幂等性：同一 `(library_name, doc_id, doc_version)` 若已有 `queued`/`running`/`pending_review`/`published` 任务则跳过；`doc_version` 复用 manifest 的 `revision` 字符串。
- 文本来源：严禁读取会被 `cleanupIngestOutput` 删除的临时目录；直接从 `server.chunks` 或 `server.versions` 快照按序拉取 text chunks 拼接发送。
- 重启恢复与重试：启动时将残留 `running` 复位为 `queued`；失败指数退避最多重试 3 次，超限置 `failed` 并支持手动重试。

#### 2.4 Go 侧治理、质检与审核 API（引入 `github.com/neo4j/neo4j-go-driver/v5`）
- `GET /api/v1/kg/jobs`：分页/按状态与 `library_name` 查询任务。
- `POST /api/v1/kg/jobs/{job_id}/retry`：重置失败/驳回任务并重入队。
- `GET /api/v1/kg/draft/graphs`、`GET /api/v1/kg/prod/graphs`：代理查询节点与边。
- `GET /api/v1/kg/draft/inspect`：直连 Neo4j 运行 Cypher 质检，返回 `duplicate_candidates`（draft 内与 draft/prod 间同名、子串及命中 `rag_kg_aliases` 的候选对）、`orphan_nodes`（度为 0 孤立点）、`missing_provenance`（缺 `source_id`/`file_path` 异常项）。
- `POST /api/v1/kg/draft/merge`：代理 merge，写 `rag_kg_aliases` 与 `rag_kg_audit_logs`。
- `POST /api/v1/kg/draft/edit`：代理 edit，写 `rag_kg_audit_logs`。
- `POST /api/v1/kg/draft/publish`：合入 `prod` 并精确清理 `draft`，更新 `rag_kg_jobs` 为 `published`，写 `rag_kg_publish_batches` 与 `rag_kg_audit_logs`。
- `POST /api/v1/kg/draft/reject`：清理指定 `doc_ids` 的 draft 数据，更新 job 为 `rejected`，写 `rag_kg_audit_logs`。

---

### 阶段 3：4 个只读 `prod` 原子工具 + QuickAgent 远程插件与反向供模代理

#### 3.1 4 个只读 `prod` 原子工具（同时暴露标准 REST 供单测与插件共用）
1. `search_entities`（`POST /api/v1/kg/tools/search-entities`）：入参 `{query, library_name?, limit?}`；结合 Neo4j `kg_prod` 模糊匹配 + `rag_kg_aliases` 别名反查 + 可选 LightRAG prod 向量召回。
2. `entity_events`（`POST /api/v1/kg/tools/entity-events`）：入参 `{entity_name, library_name?, limit?}`；先归一实体名，直连 Neo4j `kg_prod` 查一跳**双向邻接**关系，返回关联实体、边 `keywords`、`description`、`weight`、`source_id`、`file_path`。
3. `reverse_mentions`（`POST /api/v1/kg/tools/reverse-mentions`）：入参 `{entity_name, relation_keywords?, library_name?, limit?}`；先归一实体名，查双向邻接边（优先入边）并按 `relation_keywords`（默认含 `提及,评价,评论,引用,赞誉,批评,史评,记载,论述`）筛选，聚合返回提及者、关系描述与出处。
4. `search_chunks`（`POST /api/v1/kg/tools/search-chunks`）：入参 `{query, library_name?, top_k?, doc_ids?}`；直接复用 `rag-go` 现有 Milvus + ES 混合检索 + Rerank。

#### 3.2 QuickAgent 远程插件适配器（前缀 `/kg-plugin`）
严格遵循 QuickAgent `remote.rs` 契约：
- 认证：若配置 `KG_PLUGIN_TOKEN`，校验 `Authorization: Bearer <token>`；`confirm` 后校验 `X-Plugin-Connection`。
- 统一 JSON-RPC Envelope：
  - 请求：`{"type":"client-request","rpcId":"<id>","method":"<method>","payload":{"args":{...}}}`
  - 成功：`{"type":"server-response","rpcId":"<同请求>","result":{"ok":true,"value":{...}}}`
  - 失败：`{"type":"server-response","rpcId":"<同请求>","result":{"ok":false,"error":{"message":"..."}}}`
- 4 个端点：
  1. `POST /kg-plugin/api/plugin/handshake`：返回 `version: 1`、`state: "awaiting_confirmation"`、`connection_id`、`subject: "rag-kg-service"`、`plugin: { id: "rag-kg", library: "kg", title: "知识图谱问答", description: "...", skill: "skill:SKILL.md", capabilities: [], pages: [], tools: [4 个 executor="service" 工具] }`（⚠️ `pages` 必须为 `[]`，每个工具带完整 `inputSchema`，QuickAgent 已支持自动将其渲染为完整 TS 签名）。
  2. `POST /kg-plugin/api/plugin/confirm`：接收 `payload.args.connection_id`，返回 `{"state": "ready"}`。
  3. `GET /kg-plugin/resource?path=skill:SKILL.md`：返回 `text/markdown; charset=utf-8`，带标准 YAML frontmatter（`name: rag-kg`、`description: ...`）及并发检索策略说明（`kg.search_entities` → 并发 `kg.entity_events` / `kg.reverse_mentions` → `kg.search_chunks` 佐证）。
  4. `POST /kg-plugin/api/plugin/call`：路由 `payload.args.tool` 到对应原子工具，返回 `result.value = {"value": <工具结果 JSON>}`（⚠️ 必须双层 `"value"` 包裹）。

#### 3.3 反向供模内部代理（`/internal/v1/kg/llm/*`）
供 QuickAgent 外部供模能力调用（鉴权复用 `KG_PLUGIN_TOKEN` 或 `GO_RAG_INTERNAL_API_KEY`）：
- `GET /internal/v1/kg/llm/models`：返回 `{"object":"list","data":[{"id":"rag-default","object":"model"}]}`。
- `POST /internal/v1/kg/llm/chat/completions`：
  - 转发至 `rag-go` 配置的 `LLM_BASE_URL` + `LLM_API_KEY`，将请求体 `model` 覆写为真实上游模型（如 `qwen3.8-27b`）；
  - 在请求体**顶层**自动补齐 `"chat_template_kwargs": {"enable_thinking": false}`（由配置控制）；
  - 透传 `x-agent-affinity` 请求头与上游响应头 `X-Qwen-Backend`；
  - `stream: true` 时**零缓冲逐 chunk `Flush()` 透传 SSE**，不破坏 201 双副本 prefix caching 与 QuickAgent 流式解析。

#### 3.4 兼容无 QuickAgent 场景的轻量直调入口
- 提供 `POST /api/v1/kg/agent-chat`：若配置了 `QUICKAGENT_BASE_URL` 则走 QuickAgent 会话接口；否则退回极简 Go tool-calling 循环（最多 6 步，顶层传 `chat_template_kwargs: {"enable_thinking": false}`，复用同一套 4 个原子工具），返回 `{answer, citations, tool_trace}`。

---

## 3. 实现自测与验收门禁（不依赖内置真实语料，PDF 语料后续灌入）

> 说明：真实《史记》《资治通鉴》PDF 语料将在下一阶段由用户提供并经主入库链路（AnyDoc 解析）灌入，**本次施工不内置真实历史语料，不强求跑真实书目问答效果验证**，专注把全套代码实现、接口契约与工程自动化测试做扎实。

交付要求与验收门禁（交由 Antigravity 逐项核验）：
1. **门禁 1（回归零破坏）**：`go test ./...` 全量通过；未破坏 `validateNativeOperations`（45 个 OpenAPI 操作校验）；`LIGHTRAG_ENABLED=false` 时服务启动和文档主链路行为零变化。
2. **门禁 2（Sidecar 协议与连通性）**：`lightrag_service` 的 `GET /health`、`/internal/v1/kg/*` 完整实现并通过单元/接口测试；抓包或测试断言确认发往 LLM `/chat/completions` 的 `chat_template_kwargs` 在 JSON 最外层；存在进程级全局 LLM 并发 `asyncio.Semaphore`；`draft`/`prod` 拥有独立持久化 `working_dir` 与 Milvus 集合前缀。
3. **门禁 3（数据隔离、增量发布与 MySQL 治理）**：Go 侧与 Python 侧覆盖最小合成测试文本（短文本即可，覆盖同名合并、增量合入防覆盖、精确清理 draft 与 `rag_kg_aliases` 命中），确保 `draft` 未发布前问答只读 `prod` 查不到、MySQL 4 张表（`rag_kg_jobs` / `rag_kg_publish_batches` / `rag_kg_audit_logs` / `rag_kg_aliases`）读写与审计日志完整。
4. **门禁 4（QuickAgent 远程插件契约 & 反向供模）**：
   - `/kg-plugin/api/plugin/{handshake,confirm,call}` 与 `/kg-plugin/resource?path=skill:SKILL.md` 严格通过契约单测（含 `pages: []`、`result.value.value` 双层包裹、Token 与 `X-Plugin-Connection` 鉴权）；
   - `/internal/v1/kg/llm/{models,chat/completions}` 通过单测验证：覆写 `model`、顶层注入 `chat_template_kwargs`、透传 `x-agent-affinity` 与 `X-Qwen-Backend`、SSE 逐 chunk 零缓冲 `Flush()`；
   - 可顺手提供参数化的 `scripts/kg_ingest_check.py --files <pdf1> <pdf2> ...` 辅助脚本骨架，方便明天拿到真实 PDF 后一键触发入库、质检、合并与发布。
