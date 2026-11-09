VM 阶段 4：Symbol、同步迭代协议与迭代中存盘恢复（2026-11-07）

本轮约定范围已实现并完成离线验证。新增 37 项 VM 测试全部通过，主程序 681 通过，原有 29 项失败的名称完全相同，没有新增或消失；四组旧 test262 选集的原通过项也均无退步。没有调用真实模型，没有创建分支、提交或 worktree。仍是 JS 子集，不能将本报告当作完整 ES2015 一致性声明。

实现范围与取舍：

- 新增真正的 Symbol 值和身份表，支持 description、Symbol.for/keyFor、typeof symbol、包装对象、集合键和 JSON 规则。description 相同不等于身份相同；注册表、Symbol 键与集合中的身份随 Runtime 序列化。符号不是用 JSON 字符串冒充的值，工具/AI 的 JSON 边界不能传递其身份。String(symbol) 显式产生描述文本，隐式字符串/数字转换抛 TypeError。
- 属性键选用不会碰撞的内部编码，沿用已有有序属性表、原型与描述符结构，减少快照及 heap API 的改动。Symbol 使用独立存储前缀，所有零字符开头的用户字符串另行转义；列举/导出时再解码。测试实际以 0–31 的候选键尝试覆盖一个已存在的内置 Symbol 键，另验证注册符号、转义前缀、单独零字符和 JSON 往返。普通字符串、数字下标与 Symbol 键分别按规则保持顺序。
- Symbol 键接通字面量、访问器/描述符、继承、in/delete、Reflect、Object.getOwnPropertySymbols。Object.keys/values/entries、for-in 和 JSON 忽略 Symbol 键；assign、展开、对象 rest 及 getOwnPropertyDescriptors 包含相应 Symbol 键。对象 rest 在排除键之后才读取值，避免多执行已经绑定的 getter。JSON.stringify 对 Symbol 值省略对象字段、在数组中写 null，顶层返回 undefined。
- 提供 Symbol.iterator、asyncIterator、hasInstance、toPrimitive、toStringTag。instanceof、Object.prototype.toString 和属性键/运算符/模板、String/Number、单对象 Date 转换接入相应钩子及普通转换回退。new Symbol() 抛错，但 Symbol 有构造器身份，可以作为 Reflect.construct 的 newTarget；这是实际 test262 暴露并纠正的细节，没有把“调用 new 会抛错”等同于“不是构造器”。
- for-of、数组和参数展开、数组解构、Array.from、Map/Set/WeakMap/WeakSet 构造、Object.fromEntries、Promise.all/allSettled/race/any 统一走 Symbol.iterator。覆盖内置对象的迭代方法会生效；数组 keys/values/entries 从原来返回数组改为真正的实时迭代器，字符串按 Unicode 码点迭代，内置迭代器的 Symbol.iterator 返回自身。集合构造读取实际 adder，Promise 组合方法逐项迭代并登记反应，不再先把输入全部物化。
- 迭代记录保存 iterator、只取一次的 next、done 标记；用户调用和 getter 保留原始 receiver。结果必须是对象；done/value 和 next 本身的异常传播，不重复调用 return。消费步骤的异常、break、return、跨层 continue、解构提前完成和 mapper/adder 异常会关闭迭代器；正常耗尽不关闭，已有原始异常优先于清理异常。省略解构项不读取 value，rest 则耗尽迭代。实现采用普通 VM 帧和既有 try/finally 展开，保留 for-of 的 RHS TDZ、每轮新绑定及嵌套标签控制流。规则参照 [GetIterator 与 IteratorClose](https://tc39.es/ecma262/2025/multipage/abstract-operations.html#sec-iteratorclose)。
- Symbol 身份、键、缓存 next、游标、done、闭包、部分执行的 next/return 和循环体 await 都进入普通堆/帧快照并参与 GC。五项宿主测试实际在调用未完成时序列化 Runtime、恢复、继续调用，核对没有重复取值或重复关闭；没有用外部 JS 引擎替代 VM 来实现存盘。
- 程序指南、能力提示和 builtin_contracts 已同步。原有“纯算术代码不产生隐式外部效果”的测试只允许新增的本地可恢复 coercion 指令，仍禁止其他 Effect，并实际验证直接返回结果、不会产生工具/AI Action。没有删掉测试或放开任意工具调用来获取通过。

验证结果：

- 37 项新测试全过；VM 全量 355 通过、0 失败、2 忽略。[源码](../../src/agent_program/tests_stage4.rs)、[日志](../../runs/codex-vm-stage4-vm-suite.txt)。初始 23 项在实现前全部失败，保留了 [实现前日志](../../runs/codex-vm-stage4-before.txt)。后续用例来自实际发现的 Symbol 构造身份、转换、rest、TDZ、关闭规则、字符串迭代器借用和快照问题。
- 主程序全量 681 通过、29 失败、23 忽略，72.71 秒。相对阶段 3c 的 644 通过、29 失败逐项比较，新增和消失均为零。[全量日志](../../runs/codex-vm-stage4-full-20261107.txt)、[完整失败集合比较](../../runs/codex-vm-stage4-failure-comparison-20261107.json)。早一轮 680 通过、29 失败也单独保存，最终结果包含新增的字符串借用/标签删除用例和加强后的碰撞测试。
- 固定 1756 项 test262：770 → 893 通过，526 → 403 编译拒绝，运行失败仍为 144，跳过仍为 316。111 项从编译拒绝转为通过、12 项从运行失败转为通过；另 12 项解除编译阻碍后暴露运行缺口。原通过项无退步，revision、选集哈希和逐项源码哈希相同。[结果](test262-stage4.json)、[比较日志](../../runs/codex-vm-stage4-test262.txt)。
- 三组原有扩展选集也逐项与阶段 3c 比较：函数选集 283 → 311 通过，原型选集 653 → 688，通过数增加且原通过项无退步；描述符/Reflect 选集 1864 → 1915 通过，1387 编译拒绝、131 运行失败、371 跳过，原通过项无退步。后者新增的 5 项运行失败来自解除编译阻碍的用例，不是旧通过项变失败。阶段 3c 揭露的脚本 this 误通过记录没有改写；它在本轮基线和结果中仍是运行失败。[函数](test262-stage4-functions.json)、[原型](test262-stage4-prototypes.json)、[描述符](test262-stage4-descriptors.json)。
- 新增 Symbol/迭代选集 957 项：348 通过、517 编译拒绝、13 运行失败、79 跳过。13 项运行失败为 12 项既有匿名函数名称推断缺口和 1 项尚未提供的 Symbol.species getter 元数据。生成器、class、arguments、Proxy 等依赖仍按实际缺口计入拒绝/跳过，没有测试版补丁。新选集与旧选集重叠，不能相加通过数。[选集](test262-selection-stage4-iteration.json)、[全部结果](test262-stage4-iteration.json)。
- 冻结模型语料的逐项源码哈希和编译状态未变：应用内 27 份为 25 通过、2 拒绝，Codex 1434 份仍因不同宿主 API 等原因拒绝。原始 JSONL SHA256 仍为 1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1；没有换分母、重导出或执行这些程序。[结果](corpus-stage4.json)。
- Node v22.5.1 在独立严格函数环境执行同一测试源码及预期值：32 段中 31 段相同，1 段保留规范差异，见下文。五项宿主挂起/恢复用例只在 VM 实测，没有假装 Node 能验证 VM 存盘。[可重跑脚本](symbols-iteration-oracle.cjs)、[原始结果](../../runs/codex-vm-stage4-node-oracle.json)。
- 生产 cargo check 通过，保留三条 dead_code 警告；TypeScript 契约语法诊断为 0，git diff --check 无内容错误。[check](../../runs/codex-vm-stage4-check.txt)、[契约检查](../../runs/codex-vm-stage4-contracts.txt)。[机器验证记录](stage4-validation.json) 保存源码 SHA256、新测试、完整失败集合、四组选集比较、扩展选集分组、语料和 Node 差异。

Node 对照的一处差异：

`vm_stage4_for_in_and_object_rest_exclude_symbol_keys_correctly` 先绑定一个 Symbol getter，再用对象 rest 排除它。本机 Node 22.5.1 实际调用 getter 两次，VM 调用一次；其余输出相同。[CopyDataProperties 的规范](https://tc39.es/ecma262/2025/multipage/abstract-operations.html#sec-copydataproperties) 明确先检查排除键，只有未排除的键才执行 Get。因此保留显式绑定的那一次读取，未让 VM 增加重复读取去匹配本机 Node。这里是依据规范作出的判断，未追溯 V8 内部根因。

原始 Node 脚本在出现差异时仍以非零码结束，JSON 完整保留实际值和预期值，机器记录的 comparison_passed 为 false。没有改测试源码或预期以消除差异，没有将此项记为一致通过。

异常退出记录：

验证期间有一次 rustc 和一次 VM 测试进程以 STATUS_ACCESS_VIOLATION 退出。已保留 [编译异常日志](../../runs/codex-vm-stage4-rustc-crash.txt) 与 [测试异常日志](../../runs/codex-vm-stage4-vm-crash.txt)。测试进程用同一二进制重跑完成，后续最终源码的 VM 全量、主程序全量和生产 check 均实际完成。没有排除用例、提高线程栈或修改断言来绕过退出；根因未确认，不将它解释成已证明的环境问题或已修复的软件问题。

当前边界与下一阶段：

Symbol.asyncIterator 只提供键，不提供 for-await、异步迭代器或异步生成器。Symbol.species/isConcatSpreadable 和正则相关 Symbol 钩子没有开放；匿名函数名称推断、部分内置方法/构造器的通用对象转换和原型泛型借用仍有缺口。WeakMap/WeakSet 继续采用 ES2015 范围的对象键与强引用实现；没有开放后续版本的 Symbol 弱键。注册表属于当前 Runtime，不通过 JSON 跨程序/工具/AI 传递身份。旧脚本全局 this、arguments 等已知一致性限制保留。

本轮 prelude_hash 已变化，沿用现有规则拒绝直接恢复旧 prelude 的 Runtime，保留历史源码和输出。本版五类迭代中的挂起/恢复已经实际验证，不代表所有旧快照可以跨版本直接恢复。下一阶段为 class/extends/super/new.target，等待用户继续指示。
