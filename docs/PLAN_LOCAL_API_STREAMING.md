# Part A：QuickAgent 宿主 `local-api` 流式代理改造计划

> 目标仓库：`/home/goodtime/Documents/project/codex-workspace-mcp`（当前分支 `feat/context-as-code-v2`，**禁止操作 `master`**）
> 关联插件改造：`/home/goodtime/project/pdf-editor-engine/docs/PLAN_LARGE_PDF.md`（Part B）

---

## 1. 背景与现状问题

QuickAgent 宿主通过 `/agent/skill-packages/{id}/local-api/{*path}` 将插件页面（PDF / Word / Excel / PPT）的本地 API 请求代理到对应插件的回环进程 `127.0.0.1:{port}`。

当前 [src/server/ai_proxy.rs](../src/server/ai_proxy.rs)（约 L1201–L1310）中的 `skill_local_api_proxy` 存在三处大文件瓶颈：

1. **请求体 16 MiB 硬上限 + 全内存缓冲**：
   `axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024)` 直接拒收 >16 MiB 请求体，并把整份请求读入内存（且重试循环中每次 `.clone()`）。
2. **响应体整块读入内存**：
   `upstream.bytes().await` 在转发大文件导出（如数百 MB PDF 导出）时整份驻留宿主内存。
3. **总时长 30 秒硬超时**：
   `.timeout(Duration::from_secs(30))` 会中断大文件流式上传、合并、渲染或下载。

依赖现状已核对（`Cargo.toml` 无需新增 crate）：
- `axum = "0.8.9"`
- `reqwest = { version = "0.12.26", features = ["json", "stream", "multipart", "rustls-tls"] }`（已启用 `stream`）
- `futures = "0.3"`

---

## 2. 改造目标与设计契约

### 2.1 流式代理核心逻辑（`src/server/ai_proxy.rs`）

1. **冷启动端口探活与流式发送解耦**：
   - 将原本“把完整 body 缓存在内存里反复 `req.send()` 重试”改为：先对 `127.0.0.1:{port}` 执行轻量 TCP 探活（`tokio::net::TcpStream::connect` + 单次 200ms 超时，最多 10 次、间隔 150ms）。
   - 确认插件 `local_service` 已开始监听后，再发出一次性流式 HTTP 请求；若 10 次后仍连不上，返回 `502 Bad Gateway` JSON 错误。
2. **请求体零缓冲流式转发**：
   - 删除 `axum::body::to_bytes(..., 16 * 1024 * 1024)`；
   - 通过 `reqwest::Body::wrap_stream(request.into_body().into_data_stream())` 直接流式转发给上游（对于 `GET` / `HEAD` / `DELETE` 等无 body 或 `Content-Length: 0` 请求可直接不附带 body，避免某些上游对无体请求发送 `Transfer-Encoding: chunked`）；
   - 路由 `/agent/skill-packages/{id}/local-api/{*path}` 显式挂 `.layer(axum::extract::DefaultBodyLimit::disable())`，防止未来引入默认提取器上限。
3. **请求头与响应头安全透传**：
   - **上游请求头**（白名单透传，拒绝 `Host`、`Authorization`、`Cookie` 及 hop-by-hop 头如 `Connection`、`Transfer-Encoding`、`Keep-Alive`、`TE`、`Upgrade`）：
     - `Content-Type`
     - `Content-Length`
     - `Accept`
     - `Range`、`If-Range`
     - `X-Chunk-Offset`、`X-Upload-Id`、`X-File-Name`（供 PDF / 办公插件分块上传使用）
   - **下游响应头**透传：
     - `Content-Type`
     - `Cache-Control`
     - `Content-Length`
     - `Content-Disposition`
     - `Accept-Ranges`、`Content-Range`
     - `ETag`
4. **响应体零缓冲流式转发**：
   - 将 `upstream.bytes().await` 替换为 `axum::body::Body::from_stream(upstream.bytes_stream())`；
   - 保留上游 HTTP 状态码与上述安全响应头，以流式 `Response` 直接返回给浏览器/WebView。
5. **合理超时配置**：
   - 复用或构建 `reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(5)).read_timeout(Duration::from_secs(300))`，移除总时长 `timeout(30s)`，确保大文件慢速流式读写不被误杀，同时上游假死仍会在 300s 读空闲后释放。
6. **不弱化既有安全边界**：
   - 保持 `desktop_origin_guard`、`local_http_guard` 以及 `registry.packages.local_service_port(&id)` 校验完全不变，仅代理到宿主托管的 `127.0.0.1:{port}`。

---

## 3. 代码结构建议

为便于单元测试与维护，将核心流式转发逻辑抽为独立辅助函数（可位于 `src/server/ai_proxy.rs` 或同级独立子模块）：

```rust
async fn wait_for_local_service(port: u16) -> Result<(), std::io::Error>
async fn forward_local_api_request(
    port: u16,
    path: &str,
    request: axum::extract::Request,
) -> Response
```

`skill_local_api_proxy` 仅负责查询 `registry.packages.local_service_port(&id)` 并委托给 `forward_local_api_request`，测试可直接传入临时 `TcpListener` 绑定端口验证，无需启动整套插件注册表。

---

## 4. 验收测试（必须全部通过）

在 `cargo test` 中新增覆盖以下场景的自动化测试（建议放在 `src/server/ai_proxy.rs` 的测试模块或 `tests/` 下）：

1. **真流式大请求体 + 大响应体双向透传（打破 16 MiB 限制并证明非缓冲）**：
   - 起一个本地 mock Axum 服务，接收 `POST /api/echo` 流式读取请求体并边收边按流回写（同时校验 `X-Chunk-Offset`、`Content-Type`、`Content-Length`）；
   - 通过 `forward_local_api_request` 以异步分块流发送 **24 MiB** 请求体，并在测试中使用通道/栅栏显式断言：
     - **上行真流式**：上游在客户端**发送完所有块之前**就已经收到首块数据；
     - **下行真流式**：客户端在**上游结束响应流之前**就已经收到首块响应数据；
     - **完整性与头透传**：最终响应长度精确为 `24 MiB`、首尾及抽样字节与输入完全一致、`Content-Disposition` / `Cache-Control` 等响应头正确透传。
2. **冷启动重试验证**：
   - 先分配临时端口但不立即 `listen`，在后台延迟 300ms 后启动 mock 服务；
   - 调用 `forward_local_api_request` 发起 `GET /api/health` 与带 body 的 `POST /api/ping`，断言探活重试后均成功返回 `200 OK`（body 未因重试丢失）。
3. **头过滤与安全边界**：
   - 验证 `Authorization` / `Cookie` 不会泄露给插件本地服务；
   - 验证不可达端口在重试耗尽后返回 `502 Bad Gateway` JSON 错误。
4. **现有测试回归**：
   - 编译 0 警告，现有相关 `ai_proxy` / `skill_packages` 单元测试全绿。

---

## 5. 执行约束

- 先用 `df -h .` 确认可用磁盘空间 > 20 GB 再编译；
- 禁止修改 `master` 分支，全部改动保持在 `feat/context-as-code-v2`；
- **不要终止正在运行的 `grow-desktop` 进程**；完成代码与测试验证后汇报结果即可，是否构建 release 与重启桌面端由用户统一安排。
