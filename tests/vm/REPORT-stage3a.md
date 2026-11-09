VM 阶段 3 第一部分：this 与函数调用（2026-11-07）

本部分已实现并验证，没有调用真实模型，没有创建分支、提交或 worktree。原型链、属性描述符、Reflect 尚未开始，不能把本报告当作整个阶段 3 或完整 ES2015 已完成。

实现范围：

- 普通函数采用严格模块 this：直接调用为 undefined，方法调用保留接收者，显式 call(null)/call(原始值) 不替换成全局对象或装箱。箭头函数从创建处的词法环境取得 this，包括默认参数里的箭头；普通函数参数默认值也能访问本次 this。规则依据 [OrdinaryCallBindThis](https://tc39.es/ecma262/multipage/ordinary-and-exotic-objects-behaviours.html#sec-ordinarycallbindthis)。内部绑定随环境进入堆快照，但不会混进 read_scope/AI 的命名变量摘要。
- f.call、f.apply、f.bind 可用于普通/async 函数、已有内置函数及工具函数值。apply 接受数组或按 length/数字下标读取的对象，null/undefined 为空参数，其他原始值抛 TypeError；沿用 VM 的百万项数组边界。BoundFunction 堆节点保存目标、接收者、前缀参数及 name/length，GC 追踪所有引用；重复绑定保留第一次 this，按顺序追加参数。规则依据 [Function.prototype.bind](https://tc39.es/ecma262/multipage/fundamental-objects.html#sec-function.prototype.bind)。
- 普通函数 length 按第一个默认参数或 rest 之前的参数数目计算，绑定函数相应扣除前缀参数。绑定函数仍满足 typeof function/instanceof Function，按函数规则被 JSON.stringify 忽略；普通自有函数属性可读写、删除及枚举。
- 编译器先取得本次实际调用的函数，再计算参数。参数中替换同名方法不会偷换本次函数；非函数报错发生在参数求值后，诊断仍点名方法。接收者覆盖计算属性、可选调用、括号和参数展开；括号包住可选成员时保留接收者，但不会把短路延伸到括号外的普通调用。f()()、(a || b)()、arr[i]() 走实际函数值。
- 去掉按 push 名字直接编成数组追加、以及按内置名字给自有方法强套参数数目的做法，尊重用户自己的同名方法。普通顶层函数调用使用当前函数值，支持重新赋值；AI 函数的静态/运行时精确参数检查、宿主实现入口不能直接递归当 helper 的原约束仍保留。
- Array 的 map/filter/forEach/some/every/find/findIndex/findLast/findLastIndex/flatMap、Map/Set.forEach、Array.from 的 mapper 支持 thisArg。普通 Promise 回调、没有 thisArg 的普通回调使用 undefined；箭头回调忽略 thisArg。绑定回调可作 Promise handler/executor 和定时器回调，回调内宿主调用、await、GC 和存盘恢复保留接收者，不重放前半段。
- 现有内置方法新增可提取的函数值，通过同一 receiver-aware 调用路径执行。过程中补回 hasOwnProperty 的已有能力，避免转换调用路径后丢失方法。调用指令的大型参数组合放在 Box 中，避免撑大整份指令枚举。程序指南、能力说明和 builtin_contracts 已同步；TypeScript 定义文件语法诊断为 0。

验证结果：

- 新增 19 项 VM 测试全部通过，包含严格/词法 this、默认参数、call/apply/bind、重复绑定、name/length、方法与参数求值顺序、可选/计算/括号/展开调用、同名方法覆盖、函数重新赋值、自有属性、普通/async 回调、工具调用、定时器及快照/GC。
- VM 全量：261 通过、0 失败、2 忽略。[日志](../../runs/codex-vm-stage3a-vm-suite-final.txt)。
- 主程序全量：587 通过、29 失败、23 忽略，执行用时 64.75 秒。阶段 2 为 568 通过、29 失败、23 忽略；逐个失败名称比较，新增和消失均为零。MCP 过期测试也在最终全量中通过，没有排除它。[日志](../../runs/codex-vm-stage3a-full-20261107.txt)、[失败集合比较](../../runs/codex-vm-stage3a-failure-comparison-20261107.json)。
- 生产 cargo check 通过，仍是原有两条 dead_code 警告。[日志](../../runs/codex-vm-stage3a-check.txt)。
- 固定 1756 项 test262：通过 407 → 431，编译拒绝 959 → 929，运行失败 74 → 80，跳过仍为 316。源码哈希、选集哈希和 revision 相同，旧通过项无退步。17 项从编译拒绝转为通过、7 项从运行失败转为通过；另 13 项解除编译阻碍后暴露运行缺口，不能只拿运行失败总数判断退化。[结果](test262-stage3a.json)、[比较日志](../../runs/codex-vm-stage3a-test262.txt)。
- 另跑 638 项函数目录选集：171 通过、441 编译拒绝、9 运行失败、17 跳过。它与原选集有重叠，不合并通过数。Function/apply 1 通过、44 拒绝、3 跳过；bind 3/94/3；call 1/48/0。大量标准用例依赖尚未实现的 Function.prototype、构造及属性描述符，这些拒绝没有用测试版垫片掩盖。9 个运行失败中，8 个是函数名称推断缺口，1 个是脚本顶层 this 与本 VM 严格模块环境的差异。[选择文件](test262-selection-stage3a-functions.json)、[全部结果](test262-stage3a-functions.json)。
- 冻结模型代码语料不变：应用内 27 份为 25 编译通过、2 拒绝；Codex 1434 份不同宿主 API 的拒绝未变。每项源码哈希和状态均与阶段 2 相同。恢复缓存时核对原始 JSONL SHA256，未重新采样或换分母；没有执行这些程序或真实代码分析任务。[语料结果](corpus-stage3a.json)。
- 本机 Node v22.5.1 在严格模式下实际执行同一段 call/apply/bind 程序，JSON 输出与 VM 测试一致。[可重复执行的脚本](this-call-oracle.cjs)、[Node 输出](../../runs/codex-vm-stage3a-node-oracle.txt)。另核对 String、Array.map、Math.max.bind、Array.from 的 length。定时器属于宿主接口：本 VM setTimeout.length 为声明的 2，本机 Node 22 实测为 5，没有把二者列为相同。
- [机器可读验证记录](stage3a-validation.json) 保存源码 SHA256、19 个新测试、完整失败集合、test262 状态迁移、额外选集目录分组及 Node 对照信息。

断电恢复与测试修正：

断电前的一份日志出现大段空字节，未用作成功依据。VM/MCP 源码检查没有空字节，后续结果均来自重新执行的测试。外部测量缓存实际位于商店版应用 LocalCache/Local/GrowVmTests，固定 JSONL 的 SHA256 仍为 1e73f5c9433e96739b9e41aa5711c74f9a367509c25cd4f68c605c4b7c430cd1。

全量进程曾因现有 MCP 过期测试异常退出：测试把 Windows Instant 减去两小时，刚启动不足两小时会下溢；持锁 panic 后，Drop 再次 unwrap 被污染的锁，导致进程终止。现改为给同一个过期实现注入零超时，验证立即过期和清理；生产入口仍固定使用两小时超时。未用跳过测试或增大线程栈绕过问题。

当前边界与下一步：

没有完整原型对象、属性描述符、Reflect、自定义构造和一般 thenable 同化；匿名函数名称推断也未在本部分补齐。现有内置方法仍按 VM 接收者种类分派，不能当作全部原型方法的泛型借用已经实现，例如借用 Array 方法仍要求数组接收者。定时器回调没有 Node Timeout 的 this 对象，普通回调是 undefined，bind/箭头捕获可以保留用户接收者。

prelude_hash 已变化，按现有保护规则拒绝直接恢复旧 prelude 的 Runtime；历史源码和输出仍保留。本版快照恢复已验证，不意味着旧版运行时可直接恢复。

接下来是阶段 3 的原型链部分，再接属性描述符与 Reflect；等待用户继续指示。
