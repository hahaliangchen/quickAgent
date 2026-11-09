阶段 0 状态（2026-11-07）

阶段 0 的量尺、诊断和预算实现已落地并完成本阶段验证。用户释放系统内存后，主程序全量测试成功构建并运行；失败名称与改动前完全一致。本报告保留阶段 0 基线；后续集合实现见 [阶段 1 报告](REPORT-stage1.md)，仍不代表完整 ES2015 兼容。

当前结果：

- 生产入口 cargo check 成功，既有两条 dead_code 警告未变。[日志](../../runs/codex-vm-stage0-final-check.txt)。
- 主程序全量：531 通过、29 失败、23 忽略，执行用时 51.27 秒。与改动前逐个失败名称比较，新增为零、消失为零；本阶段 8 个新增测试在完整宿主入口也全部通过。新增 2 个忽略项是需显式运行的测量器。[全量日志](../../runs/codex-vm-stage0-full-20261107.txt)、[失败集合比较](../../runs/codex-vm-stage0-failure-comparison-20261107.json)。
- 轻量入口直接编译真实 VM 源码：208 通过、0 失败、2 项测量忽略。包括 8 个本阶段新增测试，覆盖预算刷新、存盘恢复、墙钟期限、热点行、特性诊断、assertion harness、负面测试分类、异步完成及基线退化检测。[日志](../../runs/codex-vm-stage0-vm-suite.txt)。
- 固定 test262 提交 e4fbf57745c6aae3d1f12f181ecef78b1665d812；归档和原始用例保存在仓库外。选集共 1756 文件：310 通过、39 运行失败、1091 编译拒绝、316 按明确条件跳过。这只是阶段选集、单一严格模式和最小 harness 的基线，不能解释为完整 ES2015 通过率。[逐项及分特性基线](test262-baseline.json)。
- 冻结模型源码 1461 份，未改写、未执行。应用内 27 份：24 编译通过、3 拒绝（两份 require("host:attachments")，一份 new Map）。Codex 1434 份均在不同宿主 API（tools/text 等）处被拒，不能据此判定标准 JS 能力，也不能与之前不同来源的 2172 份语料直接比较。[编译基线](corpus-baseline.json)。
- 两项测量再次对冻结基线运行，未发现旧通过项退化；校验还检查了样本 ID、源码哈希、固定版本及选择范围，不允许改分母。[test262 复跑](../../runs/codex-vm-stage0-test262-compare.txt)、[语料复跑](../../runs/codex-vm-stage0-corpus-compare.txt)。
- 全程没有调用真实模型，没有创建分支或提交；没有关闭用户其他程序。

预算变更：每两次已完成宿主交互之间默认 1,000,000 条指令，整段程序另有默认 30 分钟墙钟期限（含宿主等待与暂停）。序列化恢复保留进度和期限；next()、纯 Promise 微任务、重复读取 pending 不刷新预算。资源错误列明 limit、used、total_instructions、elapsed_ms、limit_ms、location、hot_lines，停止并归档，交给调用方调整编排，不作为普通模块错误自动修复重放。

新增的 harness 只存在测试构建。生产版没有的标准 Error 构造器“作为值”等能力没有被测试版悄悄补齐，所以相关 test262 项仍真实记编译拒绝。原版 harness includes 若依赖尚未支持的描述符或原型能力，也保留失败，不把断言变成空函数。后续根据这些结果推进 Map/Set、基础对象语义与其他阶段。

此前的环境阻塞已解除：完整主程序测试构建曾多次遭遇 Windows 页面文件/系统提交内存不足，错误 1455，Rust 随后分配失败。关闭增量编译、串行构建、降低当前 crate 的调试信息时也未完成。[历史编译日志](../../runs/codex-vm-stage0-low-memory-build.txt)。用户关闭游戏后，本次串行重新构建及全量运行完成；上述构建失败没有被计为测试失败，也没有覆盖原来的 29 个失败基线。

已补跑 cargo test --bin codex-workspace-mcp -j 1 -- --test-threads=8，并与上轮 [完整失败集合](../../runs/codex-output-capacity-failure-comparison-20261107.json) 完成逐项比较。阶段 0 验证完成，按分阶段计划等待用户决定是否开始 Map/Set。

测量复现命令见 [README](README.md)，阶段顺序与预算约定见 [设计说明](../../docs/vm-compatibility.md)。本机冻结语料在 C:/Users/Admin/AppData/Local/GrowVmTests/model-corpus/20261107-stage0.jsonl；内容哈希为 1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1，下一阶段直接复用，避免导出新增日志改变样本。
