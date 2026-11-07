VM 阶段 3 第二部分：原型链与普通构造（2026-11-07）

本部分补齐对象原型链接、继承访问和普通函数构造，并完成离线验证。没有调用真实模型，没有创建分支、提交或 worktree。属性描述符、用户 getter/setter 和 Reflect 留在下一部分；当前不能称为完整阶段 3 或完整 ES2015。

实现范围：

- 对象的原型链接进入堆，Object.create、Object.getPrototypeOf、Object.setPrototypeOf 和 Object.prototype.isPrototypeOf 使用实际链接。读取、in、方法调用沿原型链查找；继承方法的 this 保留最初接收者。自有属性覆盖继承属性，删除自有属性后重新看到继承值。null 原型对象没有隐式 Object 方法。循环链接、冻结对象换原型、改变 Object.prototype 的原型抛 TypeError；设置为原来的原型仍允许。规则参照 [Object.setPrototypeOf](https://tc39.es/ecma262/multipage/fundamental-objects.html#sec-object.setprototypeof)。
- 每个 Runtime 建立自己的内置构造器和 prototype 对象，包括 Object、Function、Array、原始值包装、Date、RegExp、集合、Promise 和错误类型。Function.prototype 本身可调用并返回 undefined；普通内置方法通过非枚举属性暴露，覆盖或删除后会影响实际查找。继承 Map.prototype 等不会授予 Map/Date/Promise 的内部槽，内置方法仍检查真实接收者种类。公开属性描述符尚未实现，内部标志不能当作完整 descriptor 语义。
- 普通同步函数有自己的 prototype 和 constructor 链接，new 支持参数、方法访问、原始值返回时保留实例、对象返回时替换实例，以及绑定构造函数。箭头、async 函数和对象简写方法不能构造。instanceof 接受运行时右值，沿实际原型链判断；替换构造器的 prototype 会改变之后的判断，绑定函数转给目标函数。规则参照 [OrdinaryHasInstance](https://tc39.es/ecma262/multipage/ordinary-and-exotic-objects-behaviours.html#sec-ordinaryhasinstance)。支持内置构造器别名和绑定 Promise 构造；未实现 class/new.target。
- Function.prototype.call/apply/bind 与第一部分的接收者调用路径接通。原始值可以取得对应原型并经 Object 或构造器装箱；字面量的 __proto__: object/null 设置原型，而 JSON.parse 得到的同名键仍是自有数据。普通空函数现在按 JS 返回 undefined，不再要求填写占位语句；AI 函数的声明规则保持单独检查。
- for-in 包含可枚举继承属性，Object.keys/values/entries、对象 JSON 和对象复制仍取自有可枚举属性。数组空洞的读取、map、内置迭代、Array.from 和 JSON 按原型链取得数字下标。固定 test262 首次复测暴露 ArrayLike Iterator 缺少继承的 next，已补齐对应原型及回归测试，再重新执行所有最终测量。
- 原型链接、包装值、内置对象和普通构造期间的实例进入快照及 GC 引用图。只经原型引用的对象不会被提前回收。构造函数在宿主工具调用处挂起，恢复后继续修改原来的实例，不重放前半段。
- 程序指南、flow_capabilities 和 builtin_contracts 已同步。test262 的 assert.throws 从旧的错误类型字符串判定改为实际构造器身份判定，回调仍在 VM 帧里执行；没有用测试专有实现补生产环境缺失的原型能力。

验证结果：

- 新增 23 项测试全部通过，覆盖继承/覆盖/删除、接收者、null 原型、循环及冻结约束、内置原型、new 与绑定构造、实际 instanceof、数组继承下标、JSON、GC 和挂起恢复。VM 全量 284 通过、0 失败、2 忽略。[日志](../../runs/codex-vm-stage3b-vm-suite.txt)。
- 主程序全量 610 通过、29 失败、23 忽略。与阶段 3a 的 587 通过、29 失败逐个比较失败名称，新增和消失均为零。[日志](../../runs/codex-vm-stage3b-full-20261107.txt)、[失败集合比较](../../runs/codex-vm-stage3b-failure-comparison-20261107.json)。
- 固定 1756 项 test262：通过 431 → 713，编译拒绝 929 → 624，运行失败 80 → 103，跳过仍为 316。269 项从编译拒绝转为通过，13 项从运行失败转为通过；另 36 项解除编译阻碍后暴露运行缺口。原通过项没有退步，revision、选集哈希和逐项源码哈希相同。[结果](test262-stage3b.json)、[运行日志](../../runs/codex-vm-stage3b-test262.txt)。
- 原有 638 项函数扩展选集：171 → 251 通过，351 编译拒绝、19 运行失败、17 跳过，原通过项没有退步。其中 Function.prototype.call/apply/bind 分别通过 13/9/34 项；尚有属性描述符、arguments、函数名称推断及脚本环境等缺口。[结果](test262-stage3b-functions.json)。
- 新增 1653 项原型和构造目录测量：344 通过、783 编译拒绝、192 运行失败、334 跳过。其中 Object.create 为 35 通过、171 运行失败、113 拒绝、1 跳过，失败大量依赖尚未实现的第二参数属性描述符；Object.getPrototypeOf 为 34 通过、1 运行失败、4 拒绝。该选集首次测量没有旧基线，与其他选集有重叠，不能相加通过数。[选集](test262-selection-stage3b-prototypes.json)、[结果](test262-stage3b-prototypes.json)。
- 冻结模型语料逐项源码哈希和状态不变：应用内 27 份为 25 编译通过、2 拒绝，Codex 1434 份仍因不同宿主 API 等原因拒绝。原始 JSONL SHA256 仍为 1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1，没有换分母，没有执行这些程序。[结果](corpus-stage3b.json)。
- Node v22.5.1 实际运行与 VM 测试相同的普通/绑定构造及 instanceof 程序，输出均为 [7,7,true,true,true,true,true,false,false,[],["read"]]。[脚本](prototypes-oracle.cjs)、[输出](../../runs/codex-vm-stage3b-node-oracle.txt)。生产 cargo check 与 TypeScript 契约语法检查通过。[check 日志](../../runs/codex-vm-stage3b-check.txt)、[契约诊断](../../runs/codex-vm-stage3b-contracts.txt)。
- [机器验证记录](stage3b-validation.json) 保存 23 项新增测试、源码 SHA256、完整失败集合、test262 迁移和扩展目录分组、冻结语料对照及 Node 结果。

编译过程说明：最终全量编译有一次 rustc 以 STATUS_ACCESS_VIOLATION 退出，尚未进入测试。已保留该次日志，重试后编译及全量测试完成，没有通过跳过用例绕过。[异常编译日志](../../runs/codex-vm-stage3b-build-crash-20261107.txt)。当前新增 function_is_constructor 辅助方法未使用，会产生 dead_code 警告；不影响运行验证。

剩余边界：Object.create 的非空属性描述符参数明确报错；Object.defineProperty/getOwnPropertyDescriptor、用户 getter/setter、Reflect、Symbol 自定义迭代、class/new.target 和动态 Function 构造尚未提供。原型已接通不等于所有内置方法都支持泛型借用，例如数组方法仍要求数组接收者。Promise 静态方法限内置 Promise 构造器，不提供自定义 species/构造器及完整 thenable 同化；对象 __proto__ 访问器也没有实现，本部分只支持字面量设置原型。函数名称推断和部分 JS 脚本环境语义仍是已知缺口。

prelude_hash 有变化，沿用现有保护规则拒绝直接恢复旧 prelude 的运行时；历史源码和输出保留。本版的 GC/快照恢复已经验证，不代表旧版运行时可跨版本直接恢复。下一部分为属性描述符、getter/setter 与 Reflect，等待用户继续指示。
