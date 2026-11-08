# 阶段 6：同步生成器与存盘恢复

2026-11-08。先补失败测试，再实现和修正，最后逐项比较失败名称。新增 **47 项回归全部通过**：46 项 VM 测试（含 10 项存盘恢复）和 1 项覆盖三种宿主适配入口的集成测试。

## 实现

支持 function* 声明/表达式、对象和类生成器方法、yield/yield*、next/return/throw。参数初始化发生在调用时，生成器实例在初始化之后创建，首次 next 才执行函数体。原型、name/length、描述符、this/home object、异常值身份及不可构造、不可重入规则均有测试。

生成器在堆上拥有内部状态与独立的可序列化调用帧；开始前、yield 暂停、宿主调用进行中、finally 内待完成的 return/throw、完成状态都会进入 Runtime 快照。保存和恢复不会重建实例或重放已完成的副作用。GC 从生成器内部帧追踪局部变量、闭包、处理器作用域和待完成值，不把不可达生成器当永久根。

return 在编译时保留同源的 finally 控制流；yield* 的协议转发用正常可挂起 VM 调用实现，保留缓存 next、方法接收者、结果对象身份和访问器顺序。for-of、展开、解构、Array.from 等既有同步消费者可以直接使用生成器。JSON 只保存可枚举数据，structuredClone 拒绝生成器；只有 Runtime 快照保存执行状态。

主要代码：[生成器编译](../../src/agent_program/compiler_generators.rs)、[运行状态](../../src/agent_program/runtime_generators.rs)、[回归测试](../../src/agent_program/tests_stage6.rs)。程序指南、能力提示、库声明和 task-program 技能已同步。

## 验证结果

- VM：**484 通过、0 失败、2 忽略**，19.78 秒。
- 主程序：**811 通过、29 失败、23 忽略**，95.39 秒。相对阶段 5 补充修复，失败名称完全相同：**新增失败 0、消失失败 0、旧通过项退步 0**。完整名称保存在 [验证记录](stage6-validation.json) 的 full_suite 字段；原始日志在 runs/codex-vm-stage6-full-20261108.txt。
- 七组既有 test262 选集逐项核对了 revision、选集哈希、用例 ID、源码哈希和旧通过项，**旧通过项零退步**。固定选集 924 → **980 通过**；函数选集 371 通过，原型 920，描述符 2146，同步迭代 454，类 1994，toExponential 12。各组选集有重叠，不能相加。
- 新生成器选集固定 **703 项**：改动前 73 通过、621 编译拒绝、9 跳过；改动后 **516 通过、170 编译拒绝、8 运行失败、9 跳过**。8 项运行失败均依赖尚未开放的动态 GeneratorFunction 构造，原始状态与诊断全部保留。[改动前](test262-stage6-generators-before.json)、[改动后](test262-stage6-generators.json)、[选集](test262-selection-stage6-generators.json)。
- 开放生成器语法后，旧描述符选集中 1 项、类选集中 2 项从编译拒绝转为运行失败，源码均明确构造或继承动态 GeneratorFunction。这是既定动态代码限制，不是旧通过项退步；没有将这些测试改为跳过或通过。具体 ID 与原始诊断保存在验证记录各选集的 newly_executable_runtime_failures 字段。
- 冻结的真实模型源码语料仍使用同一文件和源码哈希，编译通过项无退步。[语料报告](corpus-stage6.json)。未调用真实模型。
- 库契约 TypeScript 语法检查和 git diff --check 通过。

## 测试发现并修正的问题

1. 生成器实例创建时机：默认参数可以改变函数的 prototype，必须在参数求值后读取。新增普通调用和参数初始化中途存盘测试。
2. yield 的值必须经过正常的访问器和类型转换边界。补齐 yield getter、对象转换及 getter 中宿主调用的存盘测试。
3. SWC 39.1.0 的表达式转数组模式路径丢掉末尾空位，导致箭头函数参数中的解构空位不推进迭代器。通过原始 span 调用其直接绑定解析器恢复空位长度，仅补回末尾空位，保留原节点、表达式和源码位置；补测注释、默认值、尾逗号、super。固定选集中原先被生成器语法拒绝遮住的 6 个运行失败转为通过。
4. 原先“所有生成器都拒绝”的旧测试改为拒绝异步生成器，保留其他限制与断言；新增主程序、模块实现和私有已编译调用三个入口的正向测试。

## Node 对照与规范差异

[Node 对照脚本](generators-oracle.cjs) 覆盖同一批纯 JS 回归 **34 项，33 项一致，1 项不一致**。10 个 VM 存盘测试及宿主入口、异步生成器拒绝检查不伪装成 Node 可比测试。

不一致项是 vm_stage6_instance_prototype_read_after_parameter_initialization。本机 Node v22.5.1 在默认参数改变 g.prototype 的情况下，生成器仍持有旧原型；VM 返回新原型。固定 test262 的两项 generator-created-after-decl-inst.js 与 [ECMAScript 2025 §15.5.2 EvaluateGeneratorBody](https://tc39.es/ecma262/2025/multipage/ecmascript-language-functions-and-classes.html) 都要求先 FunctionDeclarationInstantiation，再创建实例，因此保留 VM 的规范行为，没有改断言迎合 Node。原始 mismatch、Node 版本和非零退出保存在验证记录及 runs/codex-vm-stage6-node-oracle.json。

## 验证过程的保留记录

第一轮为 1 项通过、27 项失败；之后针对描述符/克隆、创建顺序、yield 属性访问、入口和解构空位分别先保存失败日志，再修正。参数错误样例从编译期不允许的未知标识符改为显式抛错初始化器，以实际验证调用时异常。新宿主适配器测试最初使用了旧入口约定，已改为当前顶层执行和 compiled 私有 ABI；其失败日志保留。

一次 LNK1104 是重编译时测量进程仍占用目标 exe。最终验证改用独立可执行文件副本，避免链接冲突。Node 对照提取器已支持 rustfmt 换行，并断言没有遗漏宏用例。

## 运行条件与剩余范围

编译及验证均通过本机临时启动器避开 Windows 逻辑处理器 **8、9**，掩码 **0xffffcff**；标准测量和主程序全量子进程另外查询并验证了实际亲和性。未改变系统级 CPU 设置，硬件故障没有因此被宣称修复。最终 VM 与主程序测试均完整结束，未出现访问异常退出；Node 对照的非零退出来自上述已保留的语义差异。

异步生成器、for-await、类字段/私有字段/static 块、arguments、动态 Function/eval/GeneratorFunction 和其余既有兼容性缺口仍未开放。这不是完整 ES2015/JavaScript 一致性声明。
