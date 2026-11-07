VM 阶段 2：回合内定时器（2026-11-07）

本阶段实现与验证完成。没有调用真实模型，没有创建分支、提交或 worktree，未进入阶段 3。

实现范围：

- 增加 setTimeout、clearTimeout、setInterval、clearInterval、queueMicrotask。它们通过 Runtime 调用，能作为函数值使用，不走纯计算内置函数路径。两个 clear 可以互换，取消接受数字句柄或其字符串形式。
- Promise 与 queueMicrotask 共用 FIFO 作业队列；每个定时器回调前排空微任务。回调是独立任务帧，返回值不替换主程序结果；未捕获回调异常使程序失败并清理剩余计时器，async 回调返回的 Promise 按现有未处理拒绝规则处理。延迟换算及回调错误行为参考 [Node 定时器文档](https://nodejs.org/api/timers.html)、[queueMicrotask 文档](https://nodejs.org/api/globals.html#queuemicrotaskcallback)。
- Runtime 保存绝对到期时间、回调、参数和闭包引用，并把等待中的回调加入 GC 根。等待、暂停、宿主重启后的恢复沿用到期时间；过期 timeout 立即触发，interval 只触发一次，从实际派发时刻安排下一次，不补跑错过的次数。回调内工具调用可按原帧挂起、存盘和恢复，不重放回调前半段。
- 宿主同时等待并发子调用、最近定时器和整段程序期限。空 JoinSet 不会导致空转，也不会误把没有并发调用的定时器程序结束。停止/插话检查每 100 毫秒醒一次；没有状态进展时只查停止条件，不重复保存大 Runtime。中断的函数帧清理自身程序；回合结束时也更新所有保存作用域的程序，防止重启后复活旧回调。
- 到期定时器的实际派发开启新指令段；纯 Promise/queueMicrotask 作业、next()、存盘恢复不刷新指令预算。无限微任务仍命中指令上限，interval 也受原有整段 30 分钟墙钟期限限制。
- 程序指南、能力说明和 builtin_contracts 已同步，定义文件经本地 TypeScript 解析，语法诊断为 0。

验证结果：

- 新增 21 项测试全部通过（18 项 VM、3 项宿主）。覆盖保活、宏/微任务顺序、delay 换算、句柄取消、非法回调、未捕获异常、并发 AI 调用等待期间的定时器、GC 根、快照到期时间、interval 不补跑、回调挂起恢复、async 回调、预算以及保存作用域的终止清理。最终全量日志包含这 21 项的结果；早期定向日志包含其中 20 项，最后补充的 GC 根用例在最终全量中验证。
- 轻量 VM 全量：242 通过、0 失败、2 忽略。[日志](../../runs/codex-vm-stage2-vm-suite.txt)。
- 主程序全量：568 通过、29 失败、23 忽略，执行用时 48.18 秒。阶段 1 为 547 通过、29 失败、23 忽略；逐个失败名称比较，新增、消失均为零。[日志](../../runs/codex-vm-stage2-full-20261107.txt)、[失败集合比较](../../runs/codex-vm-stage2-failure-comparison-20261107.json)。首次全量中一个新测试缺少建表初始化，已修正测试准备并重跑，未把它排除在最终全量之外。
- 生产 cargo check 成功，保留原有两条 dead_code 警告。[日志](../../runs/codex-vm-stage2-check.txt)。
- 用本机 Node v22.5.1 实际执行混合 Promise/queueMicrotask/await/timeout 用例，与 VM 测试的九行顺序一致：[Node 输出](../../runs/codex-vm-stage2-node-order.txt)。这证明所测顺序一致，不代表实现了全部 Node 事件循环。
- 固定 test262 选集 1756 项：407 通过、959 编译拒绝、74 运行失败、316 跳过。与阶段 1 的每一项状态、源码哈希、选集哈希均相同。定时器不属于 ECMAScript，这份选集用于检查原有语言回归；本阶段宿主行为由新增测试验证。[结果](test262-stage2.json)。
- 冻结原始代码语料：应用内 27 份仍为 25 编译通过、2 编译拒绝；Codex 1434 份不同宿主 API 的拒绝未变。每项状态和源码哈希与阶段 1 相同，没有换分母，也没有执行这些程序或真实分析任务。[结果](corpus-stage2.json)。
- [机器可读验证记录](stage2-validation.json) 保存源码 SHA256、新测试名称、失败集合和测量对比。

兼容边界：

当前句柄是数字 ID，不是 Node Timeout 对象；没有 ref/unref/refresh、计时器回调 this 绑定、setImmediate、process.nextTick 或 node:timers/promises。this/原型相关能力仍留在后续阶段。这里提供回合内延时和轮询，不提供跨回合/跨天调度。

新增回调包装函数改变 prelude_hash，按已有宿主策略拒绝直接恢复旧 prelude 的 Runtime；历史源码和结果仍保留。本轮快照恢复测试验证的是本版程序的保存与恢复，不能据此声称旧版本快照可直接恢复。

下一阶段按用户计划是 this、函数调用绑定、原型链、属性描述符和 Reflect，等待用户继续指示。
