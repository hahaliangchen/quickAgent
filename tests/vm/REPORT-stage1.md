VM 阶段 1：集合与内置迭代（2026-11-07）

本阶段实现与回归完成。没有调用真实模型，没有创建分支、提交或 worktree。未进入阶段 2。

实现范围：

- Map、Set、WeakMap、WeakSet 使用独立 Collection 堆节点，键按 SameValueZero 比较：NaN 相等，正负零合并且保存为正零，对象按 HeapId 身份。插入顺序与哈希查找分开；更新不移位，删除重插到末尾。
- Map/Set 的 size、get/set 或 add、has/delete/clear、keys/values/entries/forEach。Set.keys 与 Set.values 使用同一函数值。WeakMap/WeakSet 只接受对象键作为写入键，非对象查询不抛错；不暴露 size、clear、枚举和 forEach。按用户方案强引用保留弱集合对象键，不模拟 GC 回收。
- Iterator 堆节点保存来源、游标、迭代种类与完成状态。删除元素跳过，追加元素可见，clear 后重插也可继续；耗尽后不再重新激活。删除占位保留在快照，哈希缓存恢复后从元素重建。
- for…of、展开、数组解构、Array.from、Object.fromEntries、Promise all/allSettled/race/any 接受内置可迭代对象。解构按需推进，不先把整个迭代器吃完。Array.from 的 mapper 与读取逐项交错，能观察对源集合的追加。forEach 用普通可挂起 VM 帧，回调内工具调用和恢复不丢位置。
- 集合普通自有属性可读写、枚举；size 只读，Object.freeze 不冻结内部条目。JSON.stringify 只输出普通可枚举属性，集合条目中的环不会被当成 JSON 环；普通集合输出 {}。Runtime 整体快照则保留条目、引用身份及迭代状态。传给工具、模块或 AI 的 JSON 内容要显式转成 Array.from 等 JSON 材料。
- structuredClone 仍按此前的 JSON-only 范围工作；新增集合/迭代器（含嵌套）明确抛 TypeError，避免通过 JSON 偷偷变成空对象。没有宣称实现完整结构化克隆。
- 构造输入的键值对象可以使用 {0:key,1:value}；同时补了静态数字属性名。阶段 0 的“Map 构造必须被拒”测试已改成仍不支持的 WeakRef/Proxy/URL 等；Map 的正例由本阶段独立语义测试覆盖。
- 程序指南、builtin_contracts 定义和设计说明已同步。定义文件经本地 TypeScript 解析，无语法诊断。新 prelude_hash 按现有策略拒绝直接恢复旧 prelude 的 Runtime，历史源码和结果仍保存。

验证结果：

- 16 项新增集合测试均通过，覆盖对象键、NaN/正负零、构造输入、变更中的迭代、完成状态、JSON、自有属性、弱集合隔离、强引用 GC、只由迭代器保留来源、哈希缓存恢复、for…of/forEach 挂起与恢复、Promise 互操作及 JSON-only 克隆拒绝。
- 轻量 VM 全量：224 通过、0 失败、2 忽略。[日志](../../runs/codex-vm-stage1-vm-suite.txt)。
- 主程序全量：547 通过、29 失败、23 忽略，执行用时 50.04 秒。逐个失败名称与阶段 0 比较，新增、消失均为零。[日志](../../runs/codex-vm-stage1-full-20261107.txt)、[失败集合](../../runs/codex-vm-stage1-failure-comparison-20261107.json)。
- 生产 cargo check 成功，既有两条 dead_code 警告未变。[日志](../../runs/codex-vm-stage1-check.txt)。
- 同 revision、同 1756 项 test262 选集、同源码哈希：通过 310 → 407；运行失败 39 → 74；编译拒绝 1091 → 959；跳过仍为 316。旧通过项没有退化。92 项从编译拒绝转为通过，5 项从运行失败转为通过，另 40 项解除编译阻碍后暴露运行时缺口；不能仅按运行失败总数判断倒退。[阶段 0 基线](test262-baseline.json)、[阶段 1 结果](test262-stage1.json)、[比较日志](../../runs/codex-vm-stage1-test262.txt)。
- 按用例目录统计，Map 29 通过、Set 44、WeakMap 11、WeakSet 9；这些目录在阶段 0 均没有通过项。features 标签不是目录名，不能把仅按 Map 标签分组的数据误当成所有 Map API 用例。
- 冻结的 27 份应用内原始程序：24 → 25 编译通过。解除阻碍的是前次真实任务里使用 new Map() 的探路程序；另外两项仍是旧 require("host:attachments") 写法。这只证明解除编译障碍，没有执行该分析任务，不能据此声称其指令开销或最终答案已改善。Codex 1434 份不同宿主 API 拒绝未变。[语料结果](corpus-stage1.json)、[比较日志](../../runs/codex-vm-stage1-corpus.txt)。

后续边界：

当前只覆盖内置迭代对象。Symbol.iterator、自定义迭代协议、原型链、属性描述符、this/thisArg 绑定仍属于后续阶段；它们使一些集合 test262 用例继续编译拒绝或运行失败，未用空断言/测试版标准全局补丁掩盖。WeakMap/WeakSet 也不是弱引用回收的完整实现。不能把本阶段称为完整 ES2015 一致性通过。

本阶段依照 [TC39 集合规范](https://tc39.es/ecma262/multipage/keyed-collections.html) 核对实时迭代和顺序行为，并以固定版本测试数据作验证。下一阶段按用户计划是回合内定时器，等待用户继续指示。
