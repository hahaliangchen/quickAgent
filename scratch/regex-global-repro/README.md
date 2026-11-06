# 全局正则循环复现记录

原始脚本：`original-turn-2-6.js`，来自任务 `task_6c929d8e120341613ab934d03359ad08` 的 `turn-2-6.js`。脚本包含对 PPT 的实际写入和读取，**不要直接重放**。

2026-11-05 16:12:24 左右，脚本读取第 4 页 HTML 后，用全局正则的 `while ((m = re.exec(html)))` 遍历元素；16:13:04 触发 `program instruction budget exhausted`。原虚拟机的 `exec` 不推进全局正则的 `lastIndex`，反复返回同一匹配。独立、安全的最小复现由 `src/agent_program/tests.rs` 中的回归测试覆盖。
