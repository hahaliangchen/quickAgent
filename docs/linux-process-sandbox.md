# 命令执行边界

Linux `bwrap` 进程沙箱与 `run_readonly_command` 已退场，不需要安装 Bubblewrap。Linux、Windows 和 macOS 的命令都以当前系统用户身份运行，由能力入口校验和会话审批控制。指定跨项目目录时，非完全权限模式仍按目录审批。这不是对已启动进程的文件系统或网络隔离。

现行规则见[设计指南：副作用边界](agent-design-guide.md#9-副作用边界)。
