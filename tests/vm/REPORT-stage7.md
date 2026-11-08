# 阶段 7：异步迭代、异步生成器与存盘恢复

2026-11-08。按用户本轮要求，把异步迭代放在同步生成器之后；原计划的通用标签模板等内容顺延。先补失败测试，再实现、修正和逐项比较。新增 **52 项回归全部通过**：51 项 VM 测试（含 11 项存盘恢复）和 1 项覆盖三个源代码入口的宿主集成测试。

## 实现

支持 async function* 声明/表达式、对象及类的异步生成器方法、for await...of、异步 yield/yield*。参数在调用时初始化，首次 next 执行函数体；next/return/throw 返回 Promise，执行或等待期间的请求按队列处理，重入请求也排队。同步与异步生成器方法分别校验接收者；异步方法的品牌错误通过 Promise 拒绝报告。

yield 和显式 return 表达式等待其值；return; 和自然结束不额外等待。return(value) 等待参数，拒绝能进入 yield 周围的 catch/finally；next 和 throw 不自动拆开输入 Promise。异步 yield* 等待协议方法结果，但保留自定义异步迭代器的 value，包括 Promise；同步回退则等待 value。

for-await 优先 Symbol.asyncIterator，否则使用同步协议适配器。缓存 next、校验结果对象，在 break/return/循环体异常时等待 return 清理。正常耗尽和读取 next/done/value 失败不会再关闭；原始异常优先于清理异常。循环直接编译协议等待，避免额外的 async 包装改变微任务顺序；同步适配器按 Promise reaction 构造结果，也没有额外的 async 返回采用轮次。

生成器内部槽保存请求队列、等待状态和暂停帧；普通 Await continuation 保存正在等待的生成器帧。队列参数、请求 Promise、闭包、this/home object、异常处理器和 finally 待完成值都参与 GC 与 Runtime 序列化。恢复不会重建实例、重新读取 next 或重放完成的副作用。队列存盘测试明确检查快照中有三个尚未完成的请求，避免把“首个同步宿主调用阻塞、后续请求尚未发出”误算成队列恢复覆盖。

Await 的 PromiseResolve 边界通过正常 VM 帧读取 Promise.constructor、执行 then getter，并调度带单次生效保护的 thenable 回调；这些 getter/回调可以在宿主调用中保存恢复。这里只补 Await 所需的同化；一般 Promise.resolve 和构造器 resolve 的普通 thenable 同化仍不完整。JSON/structuredClone 不保存生成器执行状态；动态 Function/eval/GeneratorFunction/AsyncGeneratorFunction 仍禁用。

实现位置：[编译](../../src/agent_program/compiler_generators.rs)、[循环协议](../../src/agent_program/compiler_iteration.rs)、[生成器状态与队列](../../src/agent_program/runtime_generators.rs)、[内部协议方法](../../src/agent_program/prelude.js)、[回归测试](../../src/agent_program/tests_stage7.rs)。程序指南、能力提示、库契约及 task-program 技能已同步。

## 验证

- VM 全套：**535 通过、0 失败、2 忽略**，21.84 秒。
- 主程序全量：**863 通过、29 失败、23 忽略**，98.00 秒。相对阶段 6，**新增失败 0、消失失败 0、旧通过项退步 0**；逐项失败名称保存在 [机器记录](stage7-validation.json) 的 full_suite 字段。原始日志：runs/codex-vm-stage7-full-20261108.txt。
- 新异步专项固定 **2267 项**，同一 revision、选集和源码哈希。阶段 6 二进制为 191 通过、2065 编译拒绝、11 跳过；现在 **1555 通过、693 编译拒绝、8 运行失败、11 跳过**。8 个运行失败全部依赖禁用的动态 AsyncGeneratorFunction 构造，保留真实失败状态，没有改成跳过。[改动前](test262-stage7-async-iteration-before.json)、[改动后](test262-stage7-async-iteration.json)、[选集](test262-selection-stage7-async-iteration.json)。
- 原有八组 test262 逐项比较 revision、选集哈希、用例 ID、源码哈希和原通过状态，**旧通过项均无退步**。固定选集 980 通过，函数 371，原型 920，描述符 2146，同步迭代 454，类 1994，toExponential 12，同步生成器 516。范围存在重叠，不能相加。
- 旧描述符选集的 Object/seal/seal-asyncgeneratorfunction.js 从编译拒绝变成运行失败。固定源码明确调用动态 AsyncGeneratorFunction 构造器；按既定限制保留，不算旧通过项退步，也不改测量器绕过。
- 冻结模型源码继续使用同一文件及源码哈希；逐项编译状态无退步。[语料报告](corpus-stage7.json)。没有调用真实模型。
- [Node 对照脚本](async-iteration-oracle.cjs) **39 项全部一致**，Node v22.5.1；11 个 VM 存盘测试与 1 个 VM 诊断测试不伪装成 Node 可比用例。
- 契约 TypeScript 语法检查和 git diff --check 通过。

## 测试暴露并修正的问题

1. 委托结果中的 Promise 不能当普通 yield 值再次等待；return; 也不能和显式 return undefined 混成相同等待路径。
2. for-await 与同步回退的额外 async 辅助层改变了微任务次数；改为直接协议等待和正常 reaction。
3. Promise.constructor/then getter 异常必须作为 Await 的真实异常处理，不能直接把普通 thenable 当数据交出去。解析回调也保留不可构造、length 1、空 name 和单次生效语义。
4. 异步生成器回调返回迭代器，不能被当成普通 async 回调拒绝；编译器和运行时两道检查都已区分。
5. 未捕获的生成器拒绝原先变成第 0 行、空栈。现在在展开生成器帧之前保存原始 throw 位置与栈；测试核对真实第 4 行。
6. 新文档示例引用未定义变量，发布示例编译测试发现后已补成可独立编译的代码，没有放宽测试。

原先禁止异步生成器的旧测试改为检查仍未开放的语法或 AI 生成器声明；新增正向测试覆盖主程序、模块实现和私有 compiled 入口。最初 27 项新增测试全部失败，后续边界、适配器、回调与诊断的失败日志保存在机器记录的 test_first_logs 中。

最后复核清除了能力提示中一条仍禁止生成器和所有 thenable 同化的旧句子，随后重跑 VM 与主程序全量，失败集合不变。

一次中间测量因旧 toExponential 选集文件名写错而失败；已改回原有冻结选集并重跑全部最终测量。执行器使用独立 exe 副本，避免 Windows 测量进程占用编译输出。最终各个测量及全量进程均正常结束，没有访问异常退出。

## 运行条件和剩余范围

编译及最终验证避开 Windows 逻辑处理器 **8、9**，亲和性掩码 **0xffffcff**；标准选集和主程序全量还查询确认了子进程实际掩码。只调整本次开发子进程，没有修改系统级 CPU 设置，也不宣称修复硬件。

动态代码构造、一般 Promise.resolve/executor thenable 同化、arguments、类字段/私有字段/static 块及其他既有兼容性缺口仍保留。这是已列范围内的阶段验证，不是完整 JavaScript/test262 一致性声明。Await/AsyncGenerator 与同步回退语义参照 [ECMAScript 控制抽象规范](https://tc39.es/ecma262/multipage/control-abstraction-objects.html)。
