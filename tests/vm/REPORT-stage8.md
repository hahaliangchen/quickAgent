# 阶段 8：JS 语法、类元素、正则与内置方法

2026-11-09。在已有未提交的类字段、私有字段、标签模板、arguments 等改动上继续实现，保留这些改动；没有创建分支、提交或启动真实模型。按用户后续更正，本任务只更新程序指南和验收文档，没有编辑 prompts 或 task-program 技能文件；工作区这些文件的并行改动保持原样。

## 实现与回归

新增 **59 项 VM 回归，全部通过，其中 7 项存盘恢复测试**。第一批 46 项测试在实现前为 2 通过、44 失败；随后先补正则消费者状态、重复私有声明和可挂起转换的失败测试，再修正实现。测试与逐项记录见 [tests_stage8.rs](../../src/agent_program/tests_stage8.rs)、[stage8-validation.json](stage8-validation.json)。原始日志保存在本机 `runs/vm-stage8/`，该目录沿用仓库的忽略规则。

- 逗号表达式从左到右执行并返回末项；for 更新和声明中的多个绑定可用。数组/对象解构赋值保留右值身份，支持成员、super、默认值、rest 和 for-of 目标；成员 object/key 在读取对应的迭代值/属性之前计算，const 目标拒绝赋值。数组字面量空位保留 length 并删除该索引，含展开时也不变成显式 undefined。内置构造器实参展开走普通 Construct 调用。
- 类字段不再拼接进构造函数。每个类在类作用域生成实例初始化器，home 为 prototype；基类在参数初始化之前调用，派生类在 super 返回后立即调用，包含默认构造、条件和箭头函数里的 super。公有字段使用 Define 自有数据属性，不触发父类 setter，匿名函数按字段名推断名称。计算键只在类定义时求值一次；static 字段和独立 var 作用域的 static 块在内部类绑定初始化后执行，this/home 均为类本身。
- 私有身份使用堆内持久化 Symbol 计数器及私有键集合，每次类求值产生新身份。私有字段/方法/访问器存在独立堆槽，不进入普通属性、反射、JSON 或工具参数；GC 跟踪槽值、方法及初始化器。支持实例/static 私有方法、getter/setter、async/生成器、#x in o、更新、复合赋值、可选链和解构目标。品牌仅检查自身，缺失品牌、重复初始化、写只读成员或读 setter-only 成员均抛 TypeError；freeze/non-extensible 不阻止私有槽的添加与更新。未声明或重复声明的私有名称在编译期拒绝。
- 正则统一封装为 JsRegex。普通模式保留 Rust regex 线性执行，只有其无法编译时转用 fancy-regex 0.19.2，回溯上限为 100,000。支持环视、数字/命名反向引用、u 下的 Unicode 属性转义；fancy 不接受 `(?-u:\b)`，因此转换成无捕获的 ASCII 词边界环视。g/y、lastIndex、match/matchAll、search、replace/替换函数和 split 统一传播引擎错误；超限明确抛 RangeError，保留模式、输入及结果上限。
- 所有数组回调接受 async 函数，Promise 作为普通返回值消费、不自动等待；reduce 可自行连接 Promise 链，Set/Map.forEach、生成器回调和未处理拒绝规则覆盖回归。新增 Object.is、encodeURI/decodeURI；URI 对象转换、locale 选项 getter 都可挂起恢复。
- localeCompare 使用 **ICU4X 2.2.0 compiled data**，支持 locales、numeric、sensitivity 和 caseFirst，中文拼音排序由 ICU 数据提供，返回 -1/0/1。依赖与原锁文件的 ICU 2.2 对齐，没有批量升级原有依赖。Number/Date 的 toLocale* 开放已列 en/zh 子集；不支持的 locale、时区或选项抛 RangeError。
- 补齐 String.raw 的普通可挂起调用路径，修复本环境基线中的标签模板失败；同步更新了已支持语法的旧拒绝断言和隐式 arguments 的作用域断言。没有用忽略测试或放宽 test262 比较器隐藏失败。

## 验证结果

| 验证 | 修改前 | 修改后 |
| --- | --- | --- |
| 主程序 `cargo test --bin codex-workspace-mcp` | 889 通过 / 23 失败 / 23 忽略 | **953 通过 / 18 失败 / 23 忽略** |
| 完整轻量 VM 测试 | 未单独记录修改前全套 | **600 通过 / 0 失败 / 2 忽略** |
| 新增回归 | 首批 46 项：2 通过 / 44 失败 | **59 通过 / 0 失败** |
| Node v22.22.1 对照 | — | **45 项全部一致** |

最终主程序测试耗时 66.57 秒。**新增失败名称 0、旧通过项退步 0、消失失败名称 5**。开工基线的失败数是本次实际运行的 23，不沿用旧文档中的 29。剩余 18 个失败名称与基线相同，完整列表保存在机器记录的 `full_suite.after.failure_names`，仍按失败保留；其中包括 shell 平台差异、技能作用域和既有单代理流程测试。消失的 5 个名称为：

- `agent_program::tests::compatibility_raw_templates_and_character_constructors`
- `agent_program::tests::jumps_and_throws_leave_exactly_the_scopes_they_cross`
- `agent_program::tests::regular_expressions_validate_limits_and_resume_with_saved_state`
- `agent_program::tests::version_eight_rejects_javascript_extensions_at_every_source_entry`
- `agent_program::tests::vm_stage0_unsupported_syntax_diagnostics_name_the_feature`

Node 对照脚本 [stage8-oracle.cjs](stage8-oracle.cjs) 直接读取新增测试中的源码与预期，独立执行 Node，不调用 VM。存盘、编译诊断、未处理拒绝和预算限制测试不冒充 Node 对照；故意受限的 locale 选项及引擎回溯预算也不计入可比样本。

九组既有 test262 选集均在同一固定 revision、选集哈希及源码哈希上，用本次修改前保留的二进制建立基线后逐项比较。**旧通过项全部保留**；以下范围有重叠，不可相加：

| 选集 | 通过：前 → 后 | 编译拒绝：前 → 后 | 运行失败：前 → 后 | 跳过 |
| --- | ---: | ---: | ---: | ---: |
| 固定 | 1124 → 1162 | 173 → 132 | 143 → 146 | 316 |
| 函数 | 433 → 440 | 187 → 180 | 1 → 1 | 17 |
| 原型 | 1027 → 1044 | 279 → 262 | 13 → 13 | 334 |
| 描述符 | 3004 → 3030 | 302 → 281 | 127 → 122 | 371 |
| 同步迭代 | 516 → 739 | 358 → 123 | 4 → 16 | 79 |
| 类 | 2109 → 2167 | 477 → 419 | 20 → 20 | 6030 |
| toExponential | 14 → 14 | 0 → 0 | 1 → 1 | 0 |
| 同步生成器 | 562 → 576 | 120 → 106 | 12 → 12 | 9 |
| 异步迭代 | 1642 → 1919 | 600 → 322 | 14 → 15 | 11 |

开放语法后，有旧编译拒绝项进入运行并失败，保留这些真实结果；没有把它们写成通过。逐项状态变化、零退步检查、原始报告路径及 SHA256 在机器记录的 `test262` 字段。这九组冻结 feature 列表未改，类字段/私有成员等未被选中的后续 feature 仍会跳过；本报告不把自定义回归的通过等同于这些 feature 的完整 test262 验证。本轮没有重跑冻结模型语料，也没有声称其通过。

## 剩余限制

1. JS 把未参与匹配的捕获组反向引用视为空串；当前 fancy 后端仍会匹配失败，例如 `/(a|(b))\2c/.test('ac')`。本轮明确记录该差异，没有伪装成 JS 一致。
2. 正则/字符串仍有既有 Unicode 标量表示与完整 UTF-16/u 语义差异。matchAll 仍一次性物化数组，没有实现原生惰性 RegExp String Iterator。
3. Date/Number 的 locale 格式仅为文档列出的 en/zh 子集；Date 支持 UTC 和本地时区、hour12 与年月日时分秒选项，不提供完整 Intl.DateTimeFormat/NumberFormat 或所有地区排版。Number 暂不支持格式选项；localeCompare 则使用真实 ICU collation 数据。
4. Proxy、BigInt、eval/动态 Function 仍不做；其余已有 VM 限制继续保留在实际测试结果中，不宣称完整 JavaScript 一致性。

指南更新后，发布示例编译测试单独重跑，1 项通过、0 失败；日志见 `runs/vm-stage8/documentation-check.log`。本任务修改的代码和文档通过 `git diff --check`。工作区其他任务的提示词/技能文件不属于本轮修改或清理范围。
