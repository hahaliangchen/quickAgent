# 阶段 5 补充：异步回调、名称推断和诊断

2026-11-08。全部验证使用本地真实 VM，没有调用模型，也未启动 Grow 或修改 frontend/dist。主程序全量的失败集合与上一轮完全相同；已消除类专项中 103 项非预期运行失败。动态 Function 的既有禁用限制保留，偶发访问异常的根因仍未确认。

修复内容：

- Promise reaction 原先另行分发回调：普通 closure 直接创建帧，部分 native 直接调用 js_methods::builtin，绕过普通调用入口。因此 native 回调可能误入旧的 JSON 库分支，类构造检查、方法 home object、可挂起的转换也会不一致。现在本地回调统一由已有的 $$promiseReaction 帧进入正常调用入口；只对真实工具保留宿主调度和 JSON 参数检查。覆盖普通/异步/绑定/内置回调、类构造器拒绝普通调用、静态 super、异常传播、微任务顺序及转换中的宿主挂起恢复。
- test262 的 $DONE(undefined) 不再误入 JSON fallback。异步默认参数异常的四项失败还暴露出测试设施自身的 Test262Error 缺少 prototype.constructor 链，导致直接检查 error.constructor 得不到原构造器。本轮补齐这项测试辅助对象；生产运行时的语言规则没有为 test262 打补丁，测试设施的修正与生产回调修复分别记录。
- 匿名函数、箭头及类在变量绑定、标识符赋值、逻辑赋值、参数/解构默认值和对象属性中推断 name，支持计算名称及 Symbol 描述。显式名称、类的 static name 成员保留；推断名称不引入词法自绑定，算术复合赋值不推断名称。名称属性保留不可写、不可枚举、可配置属性及 UTF-16 码元，参与 GC/快照。新增测试发现字符串方法入口会错误地将已有 Utf16 值转成 Unicode 标量字符串，也一并修复，未用替换字符吞掉落单代理字符。
- 未识别的 native 在检查 JSON 参数之前直接报告具体函数名；旧 path/url JSON 分支只接收其明确声明的方法。PromiseReaction 和 callback 帧保存注册源码位置，内置回调/工具参数错误不再只指向内部 prelude 第 100 行或第 0 行；用户函数自己的错误仍保留用户源码位置。位置可以随 Runtime 保存恢复。动态 Function 仍给出既有 EvalError，真实工具仍要求对象参数。
- 补上 Number.prototype.toExponential，包含普通/包装 Number、Number 子类、0..100 位、最短指数形式、极小/极大值、正负零、NaN/Infinity、非法 receiver/参数及参数对象转换的挂起恢复。显式精度使用二进制数的精确十进制展开和较大有效数优先的舍入，避免 Rust 默认精度格式化的 ties-to-even 差异。顺序按 [toExponential 规范](https://tc39.es/ecma262/2025/multipage/numbers-and-dates.html#sec-number.prototype.toexponential)：先验证 receiver，再转换位数，非有限值和范围检查按规定处理。

主要源码：[回调与诊断](../../src/agent_program/runtime.rs)、[名称 lowering](../../src/agent_program/compiler_classes.rs)、[绑定与默认值](../../src/agent_program/compiler.rs)、[名称堆元数据](../../src/agent_program/js_classes.rs)、[数字方法](../../src/agent_program/js_methods.rs)、[测试](../../src/agent_program/tests_stage5_fixes.rs)、[测试设施](../../src/agent_program/conformance.rs)。程序指南、能力提示和 builtin 契约已同步。

先测试再实现的记录：

首批 21 项为 5 通过、16 失败，[原日志](../../runs/codex-vm-stage5fix-red.txt) 保留。其中两个控制用例随后按实测契约修正：工具拒绝的是非对象参数，而不是必须出现 JSON 字样；动态 Function 的既有异常是 EvalError，不是 TypeError。其余失败实际复现了回调、名称和未知 native 诊断问题。随后新增的指数格式化和回调错误位置六项，在实现前全部失败，[日志](../../runs/codex-vm-stage5fix-extra-red.txt)。最终共有 30 项生产 VM 回归测试及 2 项测试设施回归测试，全部通过。

最终验证：

- VM 全量 **438 通过、0 失败、2 忽略**，8.69 秒。[日志](../../runs/codex-vm-stage5fix-vm-suite.txt)。
- 主程序全量 **764 通过、29 失败、23 忽略**，68.77 秒。与阶段 5 的 732 通过、29 失败逐名比较，新增和消失失败均为零。此次完整运行未异常退出。[日志](../../runs/codex-vm-stage5fix-full-20261108.txt)、[完整失败集合比较](../../runs/codex-vm-stage5fix-failure-comparison-20261108.json)。没有把原有 29 项宿主测试失败算成已修复。
- 六组既有 test262 选集均逐项核对相同 revision、选集哈希、case ID 和源码哈希，**没有旧通过项退步**。固定选集 **900 → 924 通过**、运行失败 151 → 127；函数 **329 → 341 通过**、14 → 2 运行失败；原型 **693 → 720 通过**、38 → 11 运行失败；描述符 **1920 → 1948 通过**、139 → 111 运行失败；迭代 **348 → 366 通过**、20 → 2 运行失败；类 **1150 → 1253 通过**、107 → 4 运行失败。六组的编译拒绝和跳过数量不变。选集重叠，不累加通过数。[固定](test262-stage5fix.json)、[函数](test262-stage5fix-functions.json)、[原型](test262-stage5fix-prototypes.json)、[描述符](test262-stage5fix-descriptors.json)、[迭代](test262-stage5fix-iteration.json)、[类](test262-stage5fix-classes.json)。
- 类专项剩下的 4 项全部是 extends Function 相关测试，与既定动态代码限制有关；没有为了消除这四个红项而开放 Function/eval。其余 103 项运行失败已转为通过，包括 48 项名称推断、50 项 undefined 回调边界、4 项异步默认参数异常和 1 项 Number 格式化。
- 新增 toExponential 专项 **15 项：12 通过、3 编译拒绝、0 运行失败**。三项拒绝均来自属性描述符测试辅助代码使用尚不支持的 arguments；不将它们改成跳过或通过。[选集](test262-selection-stage5fix-exponential.json)、[逐项结果](test262-stage5fix-exponential.json)。
- 冻结模型语料结果及逐项源码哈希不变：应用内 27 份仍为 25 通过、2 拒绝；Codex 1434 份仍因不同宿主 API 等原因拒绝。原 JSONL SHA256 仍为 `1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1`，没有重新采样或执行语料。[结果](corpus-stage5fix.json)。
- Node v22.5.1 对照 **22 段中 21 段一致、1 段差异保留**。其余 8 段需要 VM 宿主调用、Runtime 快照、诊断字段或动态代码禁用策略，未冒充 Node 一致性验证；另两个测试设施用例在 VM 内验证。[脚本](stage5fix-oracle.cjs)、[原始结果](../../runs/codex-vm-stage5fix-node-oracle.json)、[退出码 1](../../runs/codex-vm-stage5fix-node-oracle-exit.txt)。
- 生产 cargo check 通过，仍有三条 dead_code 警告；TypeScript 契约语法诊断为零，git diff --check 为零。[check](../../runs/codex-vm-stage5fix-check.txt)、[契约](../../runs/codex-vm-stage5fix-contracts.txt)。[验证脚本](../../runs/collect-stage5fix-validation.py) 与 [机器记录](stage5fix-validation.json) 保存源码哈希、完整失败名单、六组选集变化、语料、Node 差异和异常调查证据。

Node 对照差异：

`{ [key]: class { static name() { return 9; } } }` 中，VM 保留类显式定义的 static name 方法，并只转换 key 一次；本机 Node 将 name 变成字符串，再调用它会抛 TypeError。[额外实测](../../runs/codex-vm-stage5fix-node-name-probe.json) 保留 name 的实际类型、值和 key 转换次数。[对象属性定义](https://tc39.es/ecma262/2025/multipage/ecmascript-language-expressions.html#sec-object-initializer-runtime-semantics-propertydefinitionevaluation) 应走 NamedEvaluation，而 [类定义](https://tc39.es/ecma262/2025/multipage/ecmascript-language-functions-and-classes.html#sec-runtime-semantics-classdefinitionevaluation) 在静态成员求值前建立构造器名称。本轮据此保留 VM 结果，未修改断言迎合本机 Node；未追溯 V8 内部根因。原始异常、comparison_passed=false 及非零退出码均保留。

访问异常调查：

已读取 [Windows 应用故障事件](../../runs/codex-vm-stage5fix-windows-fault-events.json)，找到前一轮异常对应的 dump。符号定位前核对了原二进制 SHA256，确认与阶段 5 记录相同，再核对 dump 的模块时间戳；没有用新编译的程序去解释旧地址。故障指令落在 `BTreeMap<String, serde_json::Value>` 底层的 `push_with_handle`，标准库 node.rs 第 674 行，非法访问地址 `0x7`。[解析记录](../../runs/codex-vm-stage5fix-crash-stack.json) 保留 dump SHA256、故障 RVA、符号、位置和寄存器地址信息。

后续自动展开的帧未可靠解析，不能把它当成已经成立的调用链；当前证据只能定位故障指令，不能确认内存从哪里损坏，也不能认定是 VM、编译器、硬件或外部环境导致。本轮全量运行完成不等于异常已经修复，机器记录仍标注 `root_cause_confirmed=false`、`fixed=false`。解析脚本保留严格的旧二进制哈希检查，防止将新符号误用于旧 dump。

仍保留的范围：

动态 Function/eval、类字段/static 块、生成器/异步迭代、species 算法仍按此前约定限制；arguments 和其他未覆盖的一致性缺口仍在。32 项新回归及六组旧选集不代表完整标准 JS 一致性。本轮 prelude_hash 变化，旧 Runtime 继续按既有机制拒绝直接恢复，历史源码与输出保留。
