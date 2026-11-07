VM 阶段 3 第三部分：属性描述符、getter/setter 与 Reflect（2026-11-07）

本轮已接通数据/访问器描述符、对象完整性级别、可挂起的用户访问器和 Reflect 的 13 个方法。34 项新增 VM 测试全部通过，主程序失败集合与阶段 3b 完全相同。没有启动真实模型，没有创建分支、提交或 worktree。阶段 3 的约定范围完成，仍不能称完整 ES2015 或全量 test262 一致。

实现与语义：

- Object.defineProperty/defineProperties、Object.create 的第二参数、getOwnPropertyDescriptor(s)、getOwnPropertyNames/Symbols、Object.prototype.propertyIsEnumerable 可用。新属性缺失的标志默认为 false，修订保留未提供字段，拒绝混合数据/访问器描述符和不可调用的 get/set。不可配置属性按实际类型、枚举性、可写性和 SameValue 校验；NaN 等于自身，+0/-0 不相同。defineProperties 先转换全部描述符再定义，读取继承的描述符字段时执行相应 getter；内部描述符记录使用 null 原型，避免 Object.prototype 上的同名 getter 污染内部赋值。语义参照 [ValidateAndApplyPropertyDescriptor](https://tc39.es/ecma262/multipage/ordinary-and-exotic-objects-behaviours.html#sec-validateandapplypropertydescriptor)。
- preventExtensions/seal/freeze 及 isExtensible/isSealed/isFrozen 维护可序列化的状态。冻结访问器仍能执行 setter；冻结或密封不等于冻结集合内部条目。数组下标与 length 执行属性约束，缩短长度遇到不可配置下标时保留先前删除和回退后的长度，并应用请求的不可写标志。严格赋值/delete 抛 TypeError，Reflect 对合法但不能完成的修改返回 false；非法目标或描述符仍抛错。
- 字面量 getter/setter 支持计算名称，原有普通计算属性名也接通。编译器把成员读取提升为可恢复的调用，覆盖可选链、解构、复合赋值及自增、返回的方法引用和短路分支；getter 在方法参数求值之前执行且只执行一次。用户访问器保留原始 receiver，异常正常传播；访问器在宿主调用处挂起、经过 GC 和序列化恢复后继续执行，不重放前半段。
- Object.values/entries/assign、对象展开/解构、JSON.stringify 和数组回调/内置迭代/Array.from 的访问器读取使用普通 VM 帧。数组 push/pop/fill/reverse/shift/unshift/splice/copyWithin/sort 的修改走属性操作，保留遇到错误前的部分改动。apply 逐项读取参数列表且只读取一次 length；Reflect.apply 不受目标自有 apply 覆盖影响。bind 按顺序读取 length/name，包括访问器及无限 length，绑定状态仍可存盘。instanceof 的 prototype 读取也可执行访问器，原始值左操作数不触发该 getter。
- Reflect 支持 apply、construct、defineProperty、deleteProperty、get、getOwnPropertyDescriptor、getPrototypeOf、has、isExtensible、ownKeys、preventExtensions、set、setPrototypeOf。get/set 区分省略 receiver 和显式 undefined；set 对不同 receiver 检查其自有描述符。construct 支持普通函数、绑定构造以及另一个 newTarget 的原型。扩展 test262 找到 deleteProperty/getOwnPropertyDescriptor/has 三处漏掉的属性键转换，已补可执行用户 toString/valueOf 的路径及回归测试，再重跑最终验证。语义参照 [Reflect](https://tc39.es/ecma262/multipage/reflection.html#sec-reflect-object)。
- 属性元数据、删除标记、不可扩展状态和访问器闭包进入堆快照与 GC 引用图。函数 name/length 的真实自有属性、删除及重建顺序也接通。包装字符串的描述符、length 和自有键按 UTF-16 单元处理。宿主/AI JSON 导出遇到未物化的可枚举访问器会明确失败，提示读取字段或 JSON.stringify，不再把 getter 的占位 undefined 静默丢掉。
- 程序指南、能力提示和 builtin_contracts 已同步。内部 prelude 新增的普通函数超过原来 64 个顶层函数的用户代码上限，编译器现在只对用户源码执行该上限；没有提高用户源码或调用深度上限。旧测试中“必须拒绝计算属性名”的断言已移除，其他拒绝断言保留，新测试实际执行计算属性和访问器。

验证结果：

- 34 项新增测试全部通过；VM 全量 318 通过、0 失败、2 忽略。[测试源码](../../src/agent_program/tests_stage3c.rs)、[日志](../../runs/codex-vm-stage3c-vm-suite.txt)。最初 20 项在实现前全部失败，保留了 [实现前日志](../../runs/codex-vm-stage3c-before.txt)；后续补充了迭代、属性键、元数据、UTF-16 和 JSON 边界等实际发现的回归用例。
- 主程序全量 644 通过、29 失败、23 忽略，64.55 秒。与阶段 3b 的 610 通过、29 失败逐个比较失败名称，新增和消失均为零。[日志](../../runs/codex-vm-stage3c-full-20261107.txt)、[逐项比较](../../runs/codex-vm-stage3c-failure-comparison-20261107.json)。第一轮 642 通过、29 失败也单独保留，最终数据来自包含全部 34 个新测试的重跑。
- 固定 1756 项 test262：713 → 770 通过，624 → 526 编译拒绝，103 → 144 运行失败，跳过仍为 316。57 项从编译拒绝转为通过，另 41 项解除编译阻碍后暴露运行缺口；原通过项无退步。revision、选集哈希和逐项源码哈希一致。[报告](test262-stage3c.json)、[比较日志](../../runs/codex-vm-stage3c-test262.txt)。
- 638 项函数扩展选集：251 → 283 通过，320 编译拒绝、18 运行失败、17 跳过；原通过项无退步。[结果](test262-stage3c-functions.json)。
- 1653 项原型扩展选集：344 → 653 通过，636 编译拒绝、30 运行失败、334 跳过。163 项运行失败转为通过、147 项编译拒绝转为通过，另有下文说明的 1 项旧误通过。原始比较器仍判失败，没有跳过、豁免或改写旧报告。[结果](test262-stage3c-prototypes.json)、[保留失败的比较日志](../../runs/codex-vm-stage3c-prototypes-test262.txt)。
- 新增描述符/Reflect 选集 3804 项：1864 通过、1443 编译拒绝、126 运行失败、371 跳过。Reflect 目录为 68 通过、75 拒绝、10 跳过、0 运行失败；被拒用例包含尚未实现的 arguments、Symbol、Proxy 等依赖，不能当作 Reflect 完全一致。Object.defineProperty 为 658 通过、440 拒绝、29 运行失败、4 跳过。该选集与其他选集有重叠，不相加通过数。[选择文件](test262-selection-stage3c-descriptors.json)、[结果](test262-stage3c-descriptors.json)。
- 冻结语料逐项哈希和编译状态不变：应用内 27 份为 25 通过、2 拒绝；Codex 1434 份仍因不同宿主 API 等原因拒绝。原始 JSONL SHA256 不变，未重新采样或执行这些程序。[语料报告](corpus-stage3c.json)。
- 从 Rust 测试中抽出同一段源码和预期值，在 Node v22.5.1 的独立严格函数环境执行，32 段全部相同。访问器宿主挂起/恢复和 VM 的 JSON 导出限制属于宿主行为，另外两项只在 VM 测试，不冒充 Node 一致性验证。[可重跑脚本](descriptors-oracle.cjs)、[结果](../../runs/codex-vm-stage3c-node-oracle.json)。生产 cargo check 与 TypeScript 契约语法检查通过。[check 日志](../../runs/codex-vm-stage3c-check.txt)、[契约检查](../../runs/codex-vm-stage3c-contracts.txt)。
- [机器验证记录](stage3c-validation.json) 保存源码哈希、34 个新增测试、主程序失败名称、全部 test262 状态迁移、扩展目录分组、旧误通过的身份和 Node 证明、冻结语料及宿主行为例外。

原型扩展选集的一项旧误通过：

`test/built-ins/Object/create/15.2.3.5-4-300.js` 把脚本顶层 this 保存为 global，再用作 descriptor.set，预期它是一个不可调用的全局对象，因此抛 TypeError。VM 一直按严格模块环境编译这些脚本，顶层 this 是 undefined。阶段 3b 尚未支持 Object.create 的非空描述符，提前抛出的“不支持描述符”TypeError 恰好满足 assert.throws，掩盖了环境差异。本轮正确接受 set:undefined 后，那个断言不再成立。

Node 对照已证实：真实脚本环境抛 TypeError；同一源码放进严格函数、让 this 为 undefined 时不抛。VM 的现有模块语义与后者一致。因此它是旧环境限制被错误路径掩盖后的测量变化，不能算新实现让有效 set:undefined 失效，也不能把本次扩展比较写成全通过。旧基线、选集、源码和比较器均未改；报告明确保留 pass → run_failed 及失败日志，未做名单豁免。脚本模式/全局环境仍属于已知一致性缺口。

当前边界与后续：

Symbol 与自定义迭代协议留到阶段 4；目前 Reflect.ownKeys 仅有字符串键，getOwnPropertySymbols 返回空数组。class/new.target、Proxy、动态 Function/eval 仍不可用。普通 thenable 同化、自定义 Promise constructor/species、匿名函数名称推断和完整原型方法泛型借用仍有缺口。部分尚未迁入可挂起路径的原生数组读取遇到访问器会明确报错，可先 Array.from(a) 物化；JSON-only structuredClone 不执行未物化的访问器，需先读取或转换为普通数据。部分属性键及数值的通用对象强制转换、test262 脚本全局语义和 arguments 也仍影响一致性结果，126 个新选集运行失败全部保留。

本轮 prelude_hash 有变化，沿用既有规则拒绝直接恢复旧 prelude 的 Runtime，保留历史代码与结果。本版的访问器、描述符和构造快照恢复已实际验证；不代表旧快照可跨版本无条件恢复。下一阶段为 Symbol、可迭代协议和迭代中的存盘恢复，等待用户继续指示。
