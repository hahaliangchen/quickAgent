# 在线读取 GitHub 仓库

内置 Git 插件的 `git.read_remote_git` 可直接读取 GitHub 仓库，无需克隆或创建本地项目。新聊天可以直接提供 `https://github.com/owner/repo.git` 并要求分析代码。插件需要处于启用状态，服务端需要能访问 GitHub API；修改代码后需要重启服务加载新工具。

- `overview` 获取仓库信息、默认分支和解析后的 `commit_sha`。
- `tree` 获取递归目录树；若返回 `truncated: true`，使用 `directory` 分目录继续读取。
- `directory` 列出指定目录；`file` 读取 UTF-8 源文件；`commits` 获取近期提交。
- 后续读取将 `overview` 返回的 `commit_sha` 作为 `ref`，可确保整个分析使用同一版本。也可传分支或标签，响应会返回解析后的提交。

仅支持 GitHub.com，公开仓库无需凭据。私有仓库可在服务进程环境中配置 `GITHUB_TOKEN` 或 `GH_TOKEN`，权限需覆盖目标仓库；不要将令牌放进聊天或仓库 URL。GitHub 的权限与 API 请求限额仍然适用。单个文本文件限制为 1 MiB，二进制或过大的文件不会作为文本返回。工具不会自动执行仓库代码或把仓库写入磁盘。
