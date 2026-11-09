# 阶段 5：class、extends、super 与 new.target

2026-11-07。本轮使用真实 VM 的离线测试，没有调用模型，没有启动 Grow 或修改 frontend/dist。新增能力已写入程序指南、能力提示和内置契约；这不是完整 ES2015 一致性声明。

实现内容：

- class 声明/表达式、显式及默认构造器、extends、实例/static 方法、计算名称（含 Symbol）、getter/setter。类声明保留 TDZ，类内部名称是不可改写的独立绑定；变量声明和普通赋值可以推断匿名类名称。类构造器必须通过 new 调用，方法不可构造且不可枚举，类 prototype 属性不可改写。严格函数的 caller/arguments 读取、写入通过同一抛错访问器处理。
- 派生 this 在 super() 成功返回前处于未初始化状态；重复 super() 会先执行父构造器，再在重复初始化时抛 ReferenceError。派生构造器返回对象可以不调用 super；返回其他非 undefined 原始值抛 TypeError，正常结束或 return undefined 必须已有 this。默认派生构造器直接转交参数，不额外读取数组迭代器。
- super() 保留父构造器的求值顺序；super 属性使用方法的定义对象作为 home object，以实际 receiver 调用方法、getter/setter。支持提取方法、箭头捕获、静态 super、动态改变 home object 的原型、计算访问/赋值/更新和复合赋值；对象字面量方法也接入 home object。计算 key 的表达式和类型转换分开处理：this 检查在前、基类查找先于 key 转换；普通赋值在 RHS 之后转换 key。
- new.target 在普通调用/方法中是 undefined，new、绑定构造和 Reflect.construct 保留有效构造目标；箭头函数捕获外围目标。Error、Array、Map/Set/弱集合、Date、RegExp、Promise、Object、Number/String/Boolean 子类保留内置槽位和原型关系。Number/String 对象初始化经可挂起的转换流程处理。
- 类构造器身份、方法 home object、只读类名、派生 this、super 返回后的初始化续体及 new.target 都在普通堆/帧中，参与 GC 和 Runtime 序列化。六项宿主用例在构造器、super getter、heritage.prototype/newTarget.prototype getter 等执行中存盘恢复；提取方法的测试还在创建者返回后通过 400 次分配触发 GC。自定义 Error 的 catch/instanceof 和未捕获异常源码行号单独验证。没有委托外部 JS 引擎执行这些代码。

主要代码：[编译器](../../src/agent_program/compiler_classes.rs)、[类与 home object 堆状态](../../src/agent_program/js_classes.rs)、[运行时](../../src/agent_program/runtime.rs)、[测试](../../src/agent_program/tests_stage5.rs)。已有两处把普通 class 列为不支持的测试已随能力更新：一处改为仍不支持的私有字段，另一处从禁止语法列表移除普通 class，其余禁止项保留。

验证结果：

- 先加入首批 28 项测试并实际确认全部失败，[实现前日志](../../runs/codex-vm-stage5-red.txt)。随后补充求值顺序、匿名类、只读绑定、原生子类、GC 和恢复等边界，共 51 项新测试，最终全部通过。
- VM 全量 **406 通过、0 失败、2 忽略**，8.70 秒。[日志](../../runs/codex-vm-stage5-vm-suite.txt)。
- 主程序全量 **732 通过、29 失败、23 忽略**，67.60 秒。相对阶段 4 的 681 通过、29 失败逐名比较，新增失败与消失失败均为零。[日志](../../runs/codex-vm-stage5-full-20261107.txt)、[完整失败集合](../../runs/codex-vm-stage5-failure-comparison-20261107.json)。较早一次完整运行有 729 通过，覆盖当时的 48 项新测试，同样只有原 29 项失败；[该日志](../../runs/codex-vm-stage5-full-intermediate-20261107.txt) 保留。
- 五组既有 test262 选集均以阶段 4 为基线，核对相同 revision、选集哈希、文件 ID 及逐项源码哈希，**没有原通过项变失败或被拒绝**。固定 1756 项为 893 → **900 通过**、389 拒绝、151 运行失败、316 跳过；函数选集为 311 → **329 通过**、278 拒绝、14 运行失败、17 跳过；原型选集为 688 → **693 通过**、588 拒绝、38 运行失败、334 跳过；描述符选集为 1915 → **1920 通过**、1374 拒绝、139 运行失败、371 跳过；迭代选集为 **348 通过**、510 拒绝、20 运行失败、79 跳过。[固定选集结果](test262-stage5.json)、[函数](test262-stage5-functions.json)、[原型](test262-stage5-prototypes.json)、[描述符](test262-stage5-descriptors.json)、[迭代](test262-stage5-iteration.json)。
- test262 的运行失败数量确有增加：例如固定选集有 8 项原编译拒绝转为运行失败，另 1 项旧运行失败转为通过，因此 144 → 151。它们暴露的是开放 class 后才执行到的已有缺口，不能写成“所有失败数量不变”，也没有从结果中排除。所有变化都在 [机器验证记录](stage5-validation.json) 中保留。
- 新类专项选集 **8636 项：1150 通过、1349 编译拒绝、107 运行失败、6030 跳过**。class 声明区域 606 通过、56 运行失败；表达式区域 468 通过、51 运行失败；super 区域 65 通过、28 拒绝、1 跳过；new.target 区域 11 通过、3 拒绝。107 项失败为：48 项解构/default 初始值的函数/类名称推断，50 项异步测试的 undefined 宿主 JSON 边界，4 项异步路径 unsupported built-in，4 项有意关闭动态 Function 导致的子类构造身份限制，1 项 Number.toExponential 未提供。其余依赖 fields、生成器、arguments、Proxy 或测试辅助代码等的用例按实际拒绝/跳过记录。没有测试专用语义补丁。[选集](test262-selection-stage5-classes.json)、[逐项结果](test262-stage5-classes.json)。各选集会重叠，不能相加通过数。
- 冻结语料逐项编译结果和源码哈希不变：应用内 27 份仍为 25 通过、2 拒绝，Codex 1434 份仍因不同宿主 API 等原因拒绝。原始 JSONL SHA256 仍为 `1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1`；没有重新采样或执行语料。[结果](corpus-stage5.json)。
- Node v22.5.1 严格环境运行同一源码/预期的 44 段：**41 相同，3 处差异保留**。六项 VM 宿主恢复和一项 VM 错误位置测试不冒充 Node 验证。[可复跑脚本](classes-oracle.cjs)、[原始结果](../../runs/codex-vm-stage5-node-oracle.json)、[非零退出码](../../runs/codex-vm-stage5-node-oracle-exit.txt)。
- 生产 cargo check 通过，仍有三条 dead_code 警告；TypeScript 契约语法诊断为零，git diff --check 退出码为零。[check](../../runs/codex-vm-stage5-check.txt)、[契约](../../runs/codex-vm-stage5-contracts.txt)。[验证脚本](../../runs/collect-stage5-validation.py) 可以重新检查失败集合、原通过项、哈希、Node 原始差异和全部结果文件。

Node 的三处差异：

- 参数求值期间改变子类的父构造器：规范预期使用参数求值前选中的 A，Node 实测使用 X。
- key.toString 改变 home object 的原型：规范预期从先前取得的基类读出 7，Node 实测读出 9。
- super 的基类是 null 时：规范预期先在 ToObject(base) 抛错、不转换 key，Node 实测转换一次 key 再抛错。

前两项顺序依据 [SuperCall 与 MakeSuperPropertyReference](https://tc39.es/ecma262/2025/multipage/ecmascript-language-expressions.html#sec-super-keyword-runtime-semantics-evaluation)，第三项依据 [GetValue](https://tc39.es/ecma262/2025/multipage/ecmascript-data-types-and-values.html#sec-getvalue)。VM 保留相应规范预期，没有修改断言迎合本机 Node。这里是根据规范算法作出的判断，没有追溯 V8 内部根因；Node 对照的 comparison_passed 为 false，实际值和退出码 1 都保留。

验证异常：

最终全量测试第一次以 `0xc0000005 STATUS_ACCESS_VIOLATION` 异常退出，未生成完整汇总。[原日志](../../runs/codex-vm-stage5-main-crash.txt) 已保留；同一二进制、同样的 8 个测试线程、完整用例重跑后得到上述 732/29/23 结果，没有删掉或过滤用例。二进制 SHA256 存在验证记录中。上一阶段也记录过同类退出，**根因仍未确认，本轮没有宣称修复**。

另有一次轻量测试的链接报 LNK1104：执行类专项测试时，该 exe 仍被占用，而编译任务尝试覆盖同一文件。这是本轮验证任务调度冲突；等占用者结束后重新链接成功，后续不再并行重写和运行同一个 exe。[原日志](../../runs/codex-vm-stage5-vm-first.txt)。

当前范围：

公有/私有字段、static 初始化块、生成器、异步迭代仍未开放；动态 Function/eval 和 species 算法保持限制。arguments、解构/default 的名称推断、脚本全局 this、内置构造器的所有通用转换及完整原型泛型借用等仍有缺口。本轮 prelude_hash 已改变，按既有机制拒绝直接恢复旧 prelude 的 Runtime，历史源码和输出保留。下一阶段等待用户决定。
