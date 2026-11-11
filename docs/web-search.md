# 联网搜索

`web` 是宿主内置库，普通工具循环和 JavaScript 编排共用。搜索服务独立于对话模型：切换聊天模型不会切换搜索凭据。首个后端使用 [DeepSeek 官方 Search/Fetch API](https://api-docs.deepseek.com/guides/search/)。

在“设置 → 专用模型 → 联网搜索 → 搜索凭据”选择一个已配置的官方 DeepSeek 提供商，例如“DeepSeek 自用”，然后保存。Key 继续保存在已有本机凭据存储中；工具参数与返回值不包含 Key。未选择时调用会明确返回 `WEB_NOT_CONFIGURED`，不会自动选择其他账号。

可用方法：

```js
const web = require("web");
const found = await web.search({ query: "site:api-docs.deepseek.com thinking mode", count: 5 });
const url = found.results[0].url;
let page = await web.fetch({ url, max_chars: 12000 });
// 每页都有来源、抓取时间、快照标识和完整性信息。
while (!page.complete) {
  page = await web.fetch({ url, cursor: page.next_cursor, max_chars: 12000 });
  // 在代码中处理这一页，或将与当前问题相关的完整证据传给 AI。
}
```

普通工具循环可以直接调用 `web__search`、`web__fetch`，无需先生成程序。通过 `read_tool_contracts` 浏览 `web` 库获取工具契约。编排代码中的外部调用按现有规则放在第一层实现中；`probe:true` 仅限核心文件读取工具。

- 搜索默认返回 5 条，最多 20 条；查询限 128 个 Unicode 字符。结果包含标题、网址、摘要和可用的发布日期。搜索摘要不等于已读取正文。
- 网页正文默认每页 12000 字符，最多 24000 字符。`complete:false` 表示仍有未返回的正文，`next_cursor` 可继续读取。
- 正文快照保存在当前工作区的 `.codex-workspace-mcp/web/` 中，按内容哈希标识。继续读取同一快照不访问网络，也不会再次发生抓取计费；重新抓取需省略游标。快照变更会报错，不把不同时间的页面拼成同一份证据。
- 上游响应最多 8 MiB；超限或错误直接报告，不返回伪装成完整结果的前缀。错误响应正文不透传给模型，避免凭据或请求内部信息被回显。
- DeepSeek 的成功搜索和成功抓取分别按请求计费，详情以官方文档为准。当前只实现这一种后端，未实现搜索引擎或 HTML 解析器。

设置 API 的 `web_search_provider` 字段接收提供商 ID；省略保留已有选择，空字符串表示停用。此字段随其他非机密设置保存到应用配置目录的 `agent-generation.yml`。
