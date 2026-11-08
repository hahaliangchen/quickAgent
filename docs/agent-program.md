# Agent 流程伪代码与 Rust 执行器

> 本手册描述当前 Rust JS 子集，不代表最终目标必须使用自建 VM。设计理念与待验证的 Node 方向见 [设计与实现入口](agent-design-guide.md)。

当前语言版本 **14**。使用常见 JS 基础语法表达流程，由 SWC 解析、Rust 执行持久化指令。内部值使用虚拟机堆以保留对象引用，变量按 JS 的块级作用域和词法作用域存放；宿主工具和 AI 仍通过 JSON 交接。**普通表达式由宿主确定性求值，智能判断必须写成显式任务函数。** 相同程序、相同输入和相同工具／任务返回值产生相同控制流；AI 任务及外部操作本身不因此变成确定性的。

流程就是普通的 JavaScript。宿主实现其中的一个子集，本手册说明这个子集、尚未支持的功能和宿主提供的工具库。业务 skill 提供需求和流程，语言负责表达函数、数据依赖和控制流，宿主负责执行；学习语言不替代加载当前业务的 skill。

## 运行方式与工具导入

主任务的 AI 把完整流程写成一段 JS，放在回复的一个 ```js 代码块里；宿主收到后直接编译运行，不需要另外调用提交工具。用户请求就是主任务，由宿主建立。程序像 JS 模块一样从顶层第一条语句开始顺序执行，没有入口函数，也不需要返回值；需要的数据写在代码里，或用导入的工具读取。宿主自带的工具通过 `import * as io from "host:io"` 或 `import {read as load} from "host:io"` 引入；支持命名导入和命名空间导入，别名仅为本地绑定，不扩大权限。也可以像 Node 里 `require("fs")` 那样写 `const io = require("io")`，执行到这一行时拿到同一组方法，方法是否存在在调用处才检查。没有 import 的程序里，宿主自带的库按名字就能用；程序自己声明了同名的变量时，这个名字归程序自己。插件和项目技能的库是第三方库，在用到的地方用 `const ppt = require("ppt")` 加载：执行到这一行，宿主才链接这个库，返回它的方法对象；同一次运行里再 require 拿到同一个对象；找不到就在这一行抛出可以捕获的 `MODULE_NOT_FOUND`。参数必须是库名字符串。`host:` 只用于宿主自带的库，用它引入插件库会收到改用 require 的诊断。工具仍以一个 JSON 对象传参并经过原权限检查。默认导入、export 和其他模块导入不支持。顶层运行完毕表示流程结束，不代表业务验收自动通过。

`program_tool_headers` 是按用途组织的库目录。读代码时，用 `io.search` 在相关目录找定义或文本，再用 `io.read_lines` 查原文。用 `read_tool_contracts({libraries:["io"],overview:true})` 看该库的当前方法和用途，再用 `read_tool_contracts({methods:["io.read_lines"]})` 读单个方法的参数和返回字段。宿主也在目录下列出常用源码方法的短签名，字段来自工具契约。主 AI 可直接使用文件搜索和读取工具，模块作者的探路使用 `io` 的只读方法。

## 阅读路线

1. 从下面两个完整示例理解一次工作如何开始、执行和交付。
2. 查阅“固定语言范围”“确定性的值规则”，避免照搬完整 JS／TS 的能力。
3. 需要调用业务工具时先看本轮 `program_tool_headers` 目录，再用 `read_tool_contracts` 获取相关签名，需要把一块完整工作交给 AI 写成代码时读“模块：AI 写成的代码”。
4. 材料和并发问题查“函数作用域与工作空间预算”“循环与并发”，错误查“失败与恢复”。

宿主只提供简短的流程运行环境说明，不自动注入本手册、Skill 或完整示例。需要了解语言能力和差异时，可用 read_program_guide 按需查询；[task-program Skill](../skills/task-program/SKILL.md) 与详细手册保留供人查阅。示例都是完整源码文件，支持直接校验；它们是教学案例，不固定其他业务的页数、任务数量或步骤。

## 从最简单的串行执行开始

源码：[serial.js](../skills/task-program/examples/serial.js)。

这份程序在顶层顺序执行，不创建并发 Agent：

```text
顶层流程（request 写在代码里）
  → compose(request)         AI 设计，返回正文
  → io.write(...)             工具保存
  → io.read(...)              工具读回实际文件
  → inspect(request, content) AI 检查，返回 passed 和证据
  → assert(review.passed)     宿主消费检查结论，失败则停止
  → 流程结束
```

`compose` 和 `inspect` 带 `@module`：需要 AI 理解与处理，第一次调用时由 AI 各自写成代码，其中真正需要 AI 生成或判断的那一步，在模块代码里声明成虚函数；写文件、读文件及布尔判断有确定规则，直接由宿主执行。主程序里需要 AI 的地方都写成模块，主程序里声明虚函数，宿主在交出时就拒绝（[设计文档](agent-design-guide.md)第 3 节）。普通函数调用使用局部 Runtime 函数帧，不创建 AI 任务节点，也不启动另一个 Agent。此例用 HTML 说明串行数据流，只检查实际执行过的内容／结构，不声称已经完成浏览器视觉验收。

已有的真实结果通过参数交给需要它的函数，不因为进入虚函数就重读原材料。虚函数可在同一上下文连续观察、调用工具和处理；它只做一件事时最稳：只检查，或只处理，检查之后要不要处理由代码决定。调用方对函数契约中可以确定的输入与输出关系做代码核对，不能把不匹配的返回结果当作有效验收。处理实际状态后需要复检时才重新读回；多步的复杂处理写成代码或模块，不交给一个虚函数。

虚函数是流程中的一次 AI 能力调用，有明确输入、职责和返回值；不要把它当作省略整段流程实现的默认容器。能具体表达的读取、遍历、判断分支、写入和复查等步骤尽量由普通函数编排；需要 AI 理解、判断或生成的部分才交给职责明确的虚函数，调用方再用其返回值继续推进。划分应符合当前任务，虚函数内仍可按职责和授权逐步使用工具、处理与复查，无需为了形式强制拆分。

验收要覆盖当前任务的实际要求，禁止用表面特征冒充完整验收。基础文本、数量或存在性检查只能证明其实际检查到的事项，不能单独证明内容正确或资源可用。可确定的步骤尽量写成普通代码，需要理解与判断时调用职责明确的 AI 函数或适用工具；未检查或无法确认的事项如实报告。遇到语言限制或工具缺失时，不得删减必要检查后将结果当作全面通过，应该改用其他可行能力或说明未完成的原因。

用 `write_program` 交出这份源码（这一次回复里只调用它），宿主就会运行它。回复正文里的代码一律是回答，不会运行。

## 异步函数、工具和并发

宿主自带的工具从 `host:<library>` 导入，同步等待实际回执；需要异步工具时从 `host:<library>/promises` 导入。用 require 时，异步方法用 `require("<库名>/promises")` 加载。async 函数和 async 虚函数返回 Promise，调用方可在等待前继续执行。`Promise.all`、`allSettled`、`race`、`any`、`then`、`catch`、`finally` 提供组合与错误传播；all 和 allSettled 按输入顺序排列结果，race 和 any 按完成结果决定。

独立只读操作可由宿主合批，最多 32 项一批，逐项记录成功或失败。多个 async 虚函数运行在独立窗口，调用时固定参数和 `@uses` 点名的变量；模型请求共用并发队列，默认三个槽位。共享文件应由调用方统一写入；并发函数对同一文件的整文件写入会保留冲突候选并停止。

[五页示例](../skills/task-program/examples/parallel-five-pages.js)并发设计，统一写入并读回，再并发验收。[二十页示例](../skills/task-program/examples/parallel-twenty-html.js)用相同模式生成独立页面。示例不声称执行过浏览器或视觉检查。

## 四类调用和执行者

| 写法 | 谁处理 | 何时完成 |
| --- | --- | --- |
| 普通 `helper(args)` | 宿主进入函数局部作用域并执行其代码 | 函数 `return` 后恢复调用方；注释不会自动执行 |
| `@virtual` 的 `helper(args)`（写在模块代码里） | 一个 AI 读职责注释和实际参数，用工具循环完成这一件事 | 返回约定的 JSON 值 |
| `@module` 的 `helper(args)` | 第一次调用时 AI 按契约写成代码，之后对同一契约的调用直接运行这份代码 | 代码 `return` 后恢复调用方 |
| `library.method({...})` | 已发布的真实工具：import 的宿主库，或 require 得到的第三方库对象 | 得到工具原始 JSON；业务验收仍由流程定义 |

普通函数按词法作用域执行。虚函数只看到自己的参数和契约里用 `@uses` 点名的变量，也就是它的闭包，值在调用时读取；async 虚函数发起之后，父流程变量的变化不会同步过去。模块只通过参数拿输入。

## 模块：AI 写成的代码

`@module` 函数是一块完整的工作。第一次调用时，宿主把它的签名、契约注释和这次调用的实参交给一个 AI，由它回复这个函数的完整代码；编译通过后存成这个聊天的模块文件再运行。之后对同一契约（函数名、签名和注释原文完全一样）的调用直接运行这份代码，不再请求 AI，所以模块要写成通用代码，实参只是一个例子。同一契约的几个调用同时第一次运行时，只写一次。示例见 [module-notes.js](../skills/task-program/examples/module-notes.js)。

编排时可从 `chat_modules` 找已有模块的文件和版本，用历史工具打开源码。保持契约不变的实现改进可在提交主程序时加 `revise_modules: {模块名: {version, reason}}`。模块作者在实际调用时读取旧实现和修改目标，提交编译通过的新版本。签名、返回形状或承诺行为变化时使用独立契约。

模块代码里除了 import 只有函数声明，第三方库在用它的函数里 require。需要 AI 的部分声明成虚函数，完整的子流程可以再声明成模块。模块不捕获调用方的变量，在模块上写 `@uses` 会编译报错；写模块的 AI 也看不到调用方读过的技能，要用哪个技能，自己用 `read_skill` 读。模块出错时交给修复者，见“失败与恢复”。

## 失败与恢复

| 情况 | 意义 | 下一步 |
| --- | --- | --- |
| `PROGRAM_SOURCE_INVALID`、`executed:false` | 整份源码编译／校验未通过，什么都没执行 | 写这份代码的 AI（主 AI 或写模块的 AI）按行列诊断重写完整源码 |
| 模块里没有捕获的错误 | 之前的操作可能已发生 | 交给一个修复者：它拿到模块文件、错误和停止位置、这次调用的实参、打印的输出、已完成的写入，以及模块 require 的库的说明，用 `edit_file` 改模块文件；改好后存为新版本，从头重跑这次调用；修不了就回复 `$task_error`，错误抛给调用方 |
| 主程序里没有捕获的错误 | 执行中出现明确失败，之前完成的写入可能存在 | 错误连同这一轮已写入的文件交回主 AI，由它回复修正后的完整程序，或告诉用户做不成的原因；每轮最多重写 3 次 |
| 虚函数返回无效 JSON | 返回值无法交给调用方 | 同一个 AI 收到 `FUNCTION_RESULT_INVALID` 诊断并继续，直到返回有效 JSON 或达到执行限制 |
| 虚函数做不到 | 执行 AI 未取得要求的结果 | 它回复 `{"$task_error":{"message":"具体原因"}}`，作为错误抛给调用它的代码；代码可以捕获后拆细或换一种做法 |
| 虚函数的上下文放不下 | 下一次请求放不进上下文 | 以名为 `ContextOverflow` 的错误结束，带上已完成的写入；调用方可以把工作拆小重新调用 |
| 流程已返回 | 代码已结束 | 只有业务要求的检查实际完成，才能称作业务完成 |

错误捕获使用 `try/catch/finally`；finally 在返回、抛错和跨层跳转时执行。重写的程序和修好的模块都从第一条语句开始运行，不会从失败的指令接着跑，已经列出的写入不必重做。结果未知的外部写操作不自动重放：跨进程缺少外部回执时，副作用是否发生可能未知，不能承诺外部操作恰好一次。

## 固定语言范围（版本 14）

| 部分 | 允许范围与含义 |
| --- | --- |
| 文件 | 顶层顺序执行，没有入口函数，也不需要返回值。普通 function 定义辅助函数。模块代码只有函数声明和 import，没有顶层语句 |
| 数据 | JSON 字符串、有限数字、布尔、null、数组、静态键对象；成员／下标读取、对象简写、简单数组解构 |
| 局部值 | const/let/var、局部赋值、赋值表达式、局部 ++/-- 与 += -= *= /= %=。const 不重绑定。变量不共享可变别名 |
| 调用 | helper(args)、已发布的 library.method(args)（如 io.read，或 require 得到的库对象的方法）、函数值调用；普通函数按 JS 处理实参数量（缺少为 undefined，多余忽略），@virtual 函数严格核对参数 |
| 控制流 | if/else、for(init;test;update)、while、do/while、数组 for...of、简单绑定的 for...in、switch、带标签 break/continue、三元表达式、return、assert、try/catch/throw |
| 表达式 | + - * / %、一元 + - ! typeof、== != === !== < <= > >=、&& || ??、模板字符串；宿主直接计算，不创建模型任务 |
| 收集及数据方法 | 本地数组 push；确定性的 includes/indexOf/slice/at/concat、字符串 lastIndexOf/startsWith/endsWith/trim/toLowerCase/toUpperCase/split/replace/replaceAll/match、正则 test、数组 join，以及 JSON.stringify/JSON.parse、Array.isArray、Object.keys/values |
| 路径和 URL 计算 | `path.join/resolve/relative/normalize/dirname/basename/extname`、`url.parse`；由 Rust 直接计算，不访问文件系统 |
| 异步并发 | async 虚函数和异步工具返回 Promise；可用 Promise.all 等待，失败可由 try/catch 处理 |

常见语法归约为已有赋值、分支、跳转、调用和返回结构，不为每种写法新增调度协议。for 的 continue 进入 update 再检查条件；while 的 continue 进入条件；do/while 的 continue 进入末尾条件。break/continue 只退出其跨越的异常处理现场，保留外围 catch。

顶层和 async 函数可以 await Promise；`Promise.all` 等组合子支持 async 虚函数与异步工具。Promise 回调和 await 恢复按 JS 微任务顺序执行：async 函数返回 Promise、`then` 回调返回 Promise 或 resolve 传入 Promise 时，与 JS 一样多等两轮。直接把顶层 async 函数（含 async 虚函数）或 `host:<库>/promises` 工具的调用结果当作值使用时编译失败，例如 `inspect(page).passed`、`if (inspect(page))`、`assert(inspect(page))`、`` `${work()}` `` 或参与运算；赋值保存、传给 `Promise.all`、调用 `then`/`catch`/`finally` 和 `void` 不受影响。局部函数值或变量保存的 Promise 在编译期无法识别，运行时读取 Promise 属性时报“this is a Promise; the value may need await”。运行时实现的是 JS 子集，不是完整 Node.js。支持对象和数组成员的赋值、复合赋值及自增自减；声明、普通函数参数和 catch 支持嵌套解构、默认值及解构剩余项，对象和数组字面量支持展开；支持可选函数调用、剩余参数及调用参数展开。非简单参数先在独立的参数作用域中初始化；默认值及其创建的闭包不会看到函数体内的同名绑定。其余仍禁止 call()/ai() 包装、除 host: 库之外的 import（第三方库用 require）和 export、TS 类型／断言、BigInt，以及 JS 全局环境。`new RegExp`、`new Promise`、`new Array`、`new Date`、`new Map/Set/WeakMap/WeakSet` 和内置 Error 类是构造例外。源码字符串里的这些词语只是数据，不按词语拦截。需要智能处理时声明注释任务；没有“计算失败后自动交给 AI”的路径。

函数是值：嵌套函数声明、函数表达式、箭头函数和对象方法简写创建闭包，可保存、传递和调用。数组回调支持同步函数值、多语句和嵌套调用，包括在回调中调用工具并跨存盘恢复；字符串 replace/replaceAll 的替换函数也支持此方式。数组 map 支持 async 回调；其他数组回调当前仍要求同步。支持通过索引赋值和 length 产生稀疏数组；数组字面量空位尚不支持；数组可保存非索引属性（包括正则结果的 groups/index/input），这些属性不改变 length，也不作为数组元素参与 JSON.stringify。

## 确定性的值规则

### 常见 JS 内置能力

`Array(n)` / `new Array(n)` 的单个数字参数表示长度，创建空位；负数、非整数和非有限数抛 `RangeError`，当前数组上限为 1,000,000。其他参数按元素处理，例如 `new Array(1,2)` 为 `[1,2]`。空位不参加 `map` 等回调，`JSON.stringify` 时为 `null`；`new Array(3).fill(0)` 得到 `[0,0,0]`。

`String.raw` 标签模板保留反斜杠并正常插值，例如 ``String.raw`x${1}\d` `` 为 `x1\d`。`String.fromCharCode(...units)` 按 UTF-16 码元构造，`String.fromCodePoint(...points)` 接受整数码点 0..0x10ffff，非法码点抛 `RangeError`。落单的代理码元在 VM 内保留，`JSON.stringify` 将其转义；直接通过要求 Unicode 标量字符串的宿主 JSON 边界会明确报错，不会静默替换。普通字符串仍按下文的 Unicode 标量索引规则处理。

`Object.hasOwn(o,k)` 检查自己的属性，数组空位不算属性。`Object.freeze(o)` 浅冻结并返回原引用；属性写入、删除已有属性、数组变更等严格操作抛 `TypeError`，嵌套对象不自动冻结。`structuredClone(value)` 对 JSON 值深拷贝，嵌套引用与原值分离。

`encodeURIComponent` / `decodeURIComponent` 按 URI 组件编码；非法百分号转义、非法 UTF-8 和编码时的落单代理码元抛 `URIError`。加号不会被解码为空格。

正则 `exec`、非全局 `match` 和 `matchAll` 的每项结果带 `groups`、`index`、`input`；未匹配的命名组为 `undefined`，没有命名组时 `groups` 为 `undefined`。全局 `match` 与 JS 一样仅返回完整匹配的字符串列表。`replace` / `replaceAll` 的替换文本支持 `$<name>`。

### Date

支持 `Date.now()`、`new Date()`、`new Date(毫秒数)`、`new Date(ISO字符串)`、`new Date(年,月,日[,时,分,秒,毫秒])`，以及 `Date.parse`、`Date.UTC`。月份从 0 开始；多参数构造的年份 0..99 按 1900..1999 解释。多参数构造和本地 `get*` 跟随宿主机时区，UTC 方法不依赖本地时区。Date 与 `Math.random` 一样接受执行时的不确定性。

实例支持 `getTime`、`valueOf`、`toISOString`、`toJSON`、`toString`，以及 `getFullYear/Month/Date/Day/Hours/Minutes/Seconds/Milliseconds` 和对应的 `getUTC*`。两个 Date 相减得到毫秒数；`+date`、`< > <= >=` 按时间值计算，`instanceof Date` 成立。`JSON.stringify({t:new Date(0)})` 为 `{"t":"1970-01-01T00:00:00.000Z"}`。Date 内部毫秒值随堆序列化，挂起和恢复后不重新取当前时间。

`new Date('x')` 是 Invalid Date，`getTime()` 为 NaN，`toISOString()` 抛 `RangeError`，`toJSON()` 及 JSON 序列化为 `null`。setter 暂不支持。`toLocaleString`、`toLocaleDateString`、`toLocaleTimeString` 因缺少区域数据而在编译期拒绝；改用 `toISOString()` 或手动拼接取值。`read_program_guide` 返回能力说明和 `builtin_contracts` 中的方法定义，声明中的 TS 类型只供阅读，不能复制进流程代码。

日期字符串优先使用 ISO 格式；这里不承诺 Node 的全部宽松旧格式解析。`toString()` 显示本地时间和数值 GMT 偏移，不附带 Node/ICU 的本地化时区长名称。极端年份的本地规则依赖宿主时区实现，跨机器计算应使用 UTC 方法。

- 数字使用 IEEE-754 双精度；大整数可能舍入，除零产生非有限数。非有限数无法直接通过宿主 JSON 边界。数字转换仍未覆盖完整 JavaScript 语义。
- 基本数字、字符串和布尔值支持部分算术及比较转换；对象到原始值的转换尚未实现。=== 对对象比较引用身份，== 对基本原始值支持部分转换。
- if、循环条件、!、&&、||、三元与 assert 使用 JS 风格真值：空字符串、false、null、undefined、0 和 NaN 为假，数组和对象为真。&&/|| 返回所选操作数；?? 在 null 或 undefined 时执行右侧。缺少对象字段返回 undefined；完整 JS 语义尚未支持。
- 三元只执行选中分支；函数／工具调用可以出现在表达式中，按从左到右的确定顺序执行。对象和数组的引用可在普通函数和别名之间共享；宿主工具、AI 参数和结果仍使用 JSON。push 在实参求值后追加到本地数组。后缀更新返回旧值，前缀返回新值。
- 模板插值与数组 join 允许 JSON 原始值，null 转为 "null"；数组／对象需要显式 JSON.stringify。字符串 length、索引、slice 与 indexOf 的单位统一为 Unicode 标量，不是 UTF-16 单元。slice 使用整数下标并支持负数；JSON.stringify 使用稳定键序。
- let/const 属于所在的块：内层块可以重复声明同名变量并遮蔽外层，声明执行前读取或赋值抛出 ReferenceError，同一块内重复声明在编译时拒绝。var 属于所在函数并提升，函数开始时为 undefined，可重复声明，重复无初值 var 不重置旧值。`for (let …)` 每轮迭代使用新的绑定；catch 参数只在 catch 块内可见；给未声明的变量赋值在编译时拒绝。for...of 按 Symbol.iterator 读取当前迭代器的内容。

数据方法只有文首列出的白名单：includes/indexOf 接受一个查找值（字符串接收者要求字符串实参；数组使用 JSON 值相等），slice 接受零到两个整数下标；startsWith/endsWith/split 接受一个字符串；trim/toLowerCase/toUpperCase 无实参；join 接受零或一个字符串分隔符（默认逗号）；JSON.stringify/parse 恰好一个实参。类型或实参数量不匹配明确报错，不猜测方法含义。

`at` 接受一个整数，支持负索引，越界返回 null；字符串按 Unicode 标量索引。`concat` 接受一个参数：数组追加值或数组元素，字符串只接受字符串。

`typeof` 支持已声明的值，null 和数组返回 `"object"`；未声明的名字仍在编译期拒绝。`Array.isArray` 判断数组；`Object.keys/values` 接受对象，整数键按索引排序，其余键按插入顺序排列。通过 JSON 输入的原始键顺序可能已经改变。

路径和 URL 由宿主 Rust 直接计算：如 `path.join("docs", "page.html")`、`path.resolve("/project", "docs", "page.html")` 和 `url.parse("../a.html", "https://example.com/docs/")`。`url.parse` 返回 `{href,origin,protocol,hostname,port,pathname,search,hash}`；`path.resolve` 至少要求一个绝对路径，`path.relative` 的两个参数必须均为绝对路径，避免隐式依赖服务进程目录。路径按运行平台的规则进行词法处理，不访问文件系统或解析符号链接；输入最多 64 KiB，输出最多 256 KiB。常见 POSIX 路径和 URL 行为与 Node 对照测试，Windows 复杂路径尚未做真实环境兼容性验证，不承诺完整 Node 行为。调用不启动外部进程或 AI 任务；文件读取与修改仍使用 `io` 工具。

`lastIndexOf` 接受一个字符串，返回 Unicode 标量下标或 -1。`replace` 和 `replaceAll` 分别替换首次或全部匹配；查找可以是字符串或正则（`replaceAll` 的正则必须带 `g`），替换文本按 JS 规则展开 `$$`、`$&`、`` $` ``、`$'`、`$1` 和 `$<name>`，也可以是替换函数（接收匹配、捕获组、位置和原文，有命名组时再接收 groups），结果最多 4 MiB。

支持正则字面量，例如 `/a(\\d+)/i.test(text)` 和 `text.match(/a(\\d+)/g)`，也支持 `new RegExp(pattern, flags)` 和复制已有正则。可用标志为 `g`、`i`、`m`、`s`、`u`；底层采用 Rust regex 的 Unicode 语义及语法，不支持回溯引用、环视或 JS 特有语法。字面量的非法模式和标志在编译时拒绝，动态构造的在运行时拒绝。`test` 无状态，即使有 `g` 也不维护 JS 的 `lastIndex`；`match` 不匹配返回 null，非 `g` 返回完整匹配与捕获组组成的 JSON 数组（未匹配组为 null，不含 JS 的 index/groups 属性），`g` 返回全部完整匹配的数组。正则以内部 JSON 描述保存；除 `replace`/`replaceAll` 外，其他正则方法暂不支持。模式最多 8192 字节，输入最多 2 MiB，结果最多 10000 项及 4 MiB 捕获文本；超出时报错。正则不替代结构化 HTML 解析。

## 工具声明与固定名字

模型请求发布按用途分组的 `program_tool_headers`。宿主库列出名字和用途，第三方库另列。用 `read_tool_contracts({libraries:["io"],overview:true})` 打开方法清单。只需要某个方法的完整契约时，用 `read_tool_contracts({methods:["io.read"]})`，省略 `libraries`。只传 `libraries` 且省略 `overview` 时，仍会读该库的全部契约。目录、方法清单和契约按当前可用权限生成。虚函数不写代码：它范围内的工具作为原生函数直接调用，范围是声明它的代码 import 和 require 的非 shell 方法，缺少时用 find_library_methods 查找；shell 不向虚函数开放；宿主照样校验原始 schema 与权限。程序源码用 import 或 require 引入宿主库，用 require 加载第三方库；声明资料不是可执行源码。主 AI 写程序之前先熟悉任务：能直接调用几个读取、编辑和看图的方法；shell 则交给模块实现，缺的用 find_library_methods 查，查不到写入类方法；程序开始执行时，这一轮此前的工具往来离开它的上下文（[设计文档](agent-design-guide.md)第 4 节）。

宿主自带的工具按能力分库，一个库只管一类能力，没有兜底的库（[设计文档](agent-design-guide.md)第 5.2 节）：io 是文件（read/read_lines/write/list/search/edit/replace/workspace_info），process 是运行程序和项目脚本、管理项目进程，http 是 probe，browser 是浏览器页面，memory 是工作记忆、架构记忆和符号的业务上下文，vision 是看图和图像分析，attachments 是读附件，session 是主对话自己的材料和记录（只有主脚本能用）。方法名去掉和库名重复的前缀（browser.open、http.probe），其余沿用原工具名。插件在 plugin.json 或远程握手的 plugin 中声明 library，例如 {"id":"ppt-master","library":"ppt"}；已登记的 ppt_master__get_design 对外的名字是 ppt.get_design：程序写 `const ppt = require("ppt")`，再调用 `ppt.get_design({...})`。插件的方法不需要先用 read_skill 激活。未声明 library 的旧插件按 ID 将连字符替换为下划线，作为固定兼容名字。项目技能的库名就是技能名。库名必须是有效、非保留的标识符，不能占用宿主自带的库名和 host；不同插件不能占用同一个库名。方法来自实际注册工具，不按业务含义猜测。

头文件生成器和编译器使用同一份当前作用域目录：所有已启用且当前权限、插件归属、执行限制允许的操作均发布，不要求预先加载工具组。未启用／未授权的工具不发布也不能编译。主程序和模块代码都只使用宿主目录，模型不提供或继承别名表。宿主编译结果保留固定绑定；helper 使用原编译结果，恢复不重新猜测工具目标。实际执行继续经过原工具分发、权限、插件归属和回执检查。

参数从真实 JSON schema 自动生成：整数、范围、格式、额外字段限制等通过注释保留，枚举和联合类型转成声明。返回类型来自 outputSchema（命令工具可用 output_schema）；没有返回 schema 时是 unknown，不虚构返回字段。递归／外部引用无法内联时保留引用注释并标 unknown，声明不代替原工具运行时契约。

写代码的请求只发送库声明，不重复发送这些操作的原始 JSON schema，业务工具也不作为原生函数发给它。虚函数的请求只发送它范围内的工具，用原工具的 JSON schema 作为原生函数，调用直接进入原工具分发、权限和回执检查。

```js
function loadSource(request) {
  const actual = io.read({path: request.source});
  if (!actual.error && actual.content.length) {
    return {content: actual.content, present: true};
  }
  return {present: false};
}
```

import 的库名是编译期绑定，不是可变 JS 对象；它的方法和 require 得到的方法都可以当函数值使用。整份源码先编译再执行，宿主库里的未知方法在任何效果发生前拒绝；第三方库在 require 时才链接，它缺少的方法在调用处报错。源码参数仍是原 JSON 对象，不把位置参数自动转换成字段。

### Shell stage 1：process 库

shell 只允许在 `@module` 的确定性实现代码中调用；主 AI、主程序和 `@virtual` 叶子均不能直接调用，也不能靠查找方法或 import 放宽。需要 shell 的主流程声明一个模块，由模块实现实际命令、轮询、解析和检查；虚函数仍是接收必要材料、只做一件事的 AI 判断钩子。三个方法属于 `host:process`：`process.exec_command({cmd,workdir,yield_time_ms})`、`process.write_stdin`、`process.kill_session`。旧工具保留，但不作为绕过 shell 边界的入口。shell 路径、能力和工作区由 system 动态注入；Windows 用 PowerShell 7（pwsh）语法与 `D:/project` 路径，Linux 用 Bash。每条 cmd 启动新进程、单独传 workdir，变量和 cd 不跨命令保留；长进程仍保留 session，标准输入输出用管道，无 PTY。

同一个 workspace 内，同 task、同 turn 的 shell 可以共存并共享 `change_interval`：例如先启动 `npm dev`、保留它的 session_id，再用后续 `exec_command` 执行 `curl` 探测。重叠期间的文件改动合并跟踪，直到区间里最后一个 shell 退出才结算；一个命令退出不代表整个区间完成。别的 task/turn，以及宿主文件写入、编辑和 undo，都必须等待该区间的所有 shell 停止。超过 `yield_time_ms` 等待仍未退出时，保留返回的 `session_id`，用 `process.write_stdin({session_id,chars:"",yield_time_ms})` 轮询增量输出；非空 `chars` 写 stdin，`process.kill_session({session_id})` 终止该会话的整个进程树。结果的 `running`、`exit_code`、`stdout`、`stderr`、`duration_ms`、`encoding`、`truncated` 是实际观测；空退出码不当作成功，业务验收仍需检查真实结果。

Windows 运行器每次调用 `pwsh -NoLogo -NoProfile -NonInteractive -Command`，宿主包装先设 UTF-8 输出编码、清空 `$LASTEXITCODE`，执行正文后有原生退出码就按该码退出；调用方不再套一层包装。宿主设置 `PYTHONUTF8=1`、`PYTHONIOENCODING=utf-8`、`GIT_TERMINAL_PROMPT=0`、`NO_COLOR=1`，将 CRLF 统一为 LF，并保留 UTF-8 失败时的 GBK 解码回退。检测不到 PowerShell 7 时不回退 5.1，不提供 shell 工具，并向模型说明不可用；结构化读写工具照常可用。代码会话显示安装、商店及重新检测／手动路径引导，插件会话不弹安装提示。

模块代码拿到实际 stdout、stderr 和退出码，结果先进入 JS heap，默认 stdout 与 stderr 合计的输出堆预算为 8 MiB；不是为 AI 生成首尾预览。超过预算时以 `SHELL_OUTPUT_OVER_CAPACITY` 明确失败，保留完整输出 asset、会话和读取引用，不交付成功前缀。解析前先确认进程已结束、退出码符合预期。需要完整内容时，模块用 `process.write_stdin({session_id,chars:"",stream:"stdout",char_offset:0,max_output_tokens})` 读取 `output`，按 `next_char_offset` 分页，stderr 单独从偏移 0 读取；分段处理，不盲目重新拼出超预算字符串，也不重跑命令来取输出。模块返回值先回到调用方的 JS heap，并非自动写进 AI 上下文；AI 只看到代码打印、诊断、显式传给虚函数的材料和成果摘要。按问题选择应交给 AI 的完整证据单元，保留文件和行范围。编排和模块实现继续写具体 JS。

定位代码时，模块优先用 `rg --files frontend/src` 列出受限目录的文件，用 `rg -n "关键词" frontend/src` 找定义再读相关范围。这两种写法默认遵守 `.gitignore`，不主动加 `-u` 扫描忽略目录；不要用递归 `Get-ChildItem` 把 `target`、`node_modules` 等产物全部遍历。完整示例见[主流程声明](../skills/task-program/examples/shell-in-module.js)及[模块实现](../skills/task-program/examples/shell-in-module-implementation.js)：模块运行平台中立的 Node JSON 命令，先检查运行状态、退出码和截断标志，再 `JSON.parse`，只返回小摘要；主流程只打印摘要。

## 第三方库与技能说明

插件和项目技能带的说明（SKILL.md）是库的文档。和普通语言里的函数一样，一个函数只带它自己用到的东西：哪段代码引用了这个库，说明就作用在那段代码上，不按调用关系往下传。

- 写代码的 AI 先用 `read_skill` 读要用的技能，读到的说明只留在它自己的上下文里。
- 代码 require 了某个库，宿主就把这个库的说明交给负责这段代码的 AI：主程序的是主 AI，包括自愈时；模块的是写它和修复它的 AI。
- 虚函数只带它在契约里用 `@uses` 点名的库的说明；没点名的虚函数不带任何技能说明，被调用的模块也不继承调用方读过的技能。
- 只有说明、没有方法的技能也用 require 引用，得到一个没有方法的对象，作用是把说明挂到这段代码上。

比如一个模块的代码里，虚函数声明在 require 了库的函数里，用 `@uses` 点名库的变量：

```js
function process(notes) {
  const md = require("markdown");
  /**
   * @virtual
   * 把 notes 写成一节 markdown，返回 {text:string}。
   * @uses md 按它的规则写
   */
  function writeSection(notes) {}
  return writeSection(notes);
}
```

## 任务函数与工具步骤

业务 skill 和用户决定范围、依赖、确认点、验收与交付；通用 task-program skill 负责表达这些关系。宿主为每个用户请求建立主任务；AI 准备缺失的技能／工具契约后，用 `write_program` 交出一段覆盖完整工作、检查与交付的 JS 程序。

普通 helper 在宿主的局部函数帧中执行，不创建 AI 任务节点；工具调用是实际操作步骤。显式的 @virtual 或 @module helper 创建独立的 AI 上下文，把职责注释与命名实参交给 AI。标记放在紧邻函数的前置 JSDoc 或函数体注释中，体内可为空，或仅写 throw new Error("固定说明") 作为占位；不允许占位 return 或其他可执行语句；未标记的空函数是普通 JS，返回 undefined，不声明 AI 工作。返回结构写入注释契约，实际返回来自 AI 或模块代码。普通代码中的注释没有执行效果。虚函数的 AI 用工具循环直接完成这件事；需要写成代码的工作声明为 @module。主程序里需要 AI 的地方一律是 @module，虚函数只写在模块代码里。注释应说明职责、输入与返回字段。所有调用一律按位置绑定实参，模块代码保持契约的参数个数；不按名字猜测，也不把多参自动打包成单对象。bindings 中的值就是局部参数值，没有额外 input 包装；closure 列出契约里 `@uses` 点名的变量。

```js
const request = {output: "report.html", requirements: "按用户要求制作并验收报告"};
let artifact = produce(request);
let check = inspect(artifact, request);
if (!check.passed) {
  artifact = repair(artifact, check, request);
  check = inspect(artifact, request);
}
assert(check.passed, "修复后验收仍未通过，停止交付");
deliver(artifact, request);
/** @module */
function produce(request) {
  // 按 request 制作实际产物，返回路径和版本。
}
/** @module */
function inspect(artifact, request) {
  // 读取或检查实际产物，对照 request 判断。
  // 返回 {passed:boolean, evidence:[实际工具观察], issues:[...] }。
}
/** @module */
function repair(artifact, check, request) {
  // 使用 check 的证据完成授权范围内的修正，返回实际产物。
}
/** @module */
function deliver(artifact, request) {
  // 完成 request 要求的检查与交付，返回真实结果和限制。
}
```

这个修复示例最多尝试一次修复。修复后必须再次 inspect；复检仍未通过就由 assert 停止，deliver 不会执行。

inspect 的判断由 AI 作出；!check.passed 由宿主消费布尔值。不再把 artifact !== expected 解释为“不满足预期”；它只比较 JSON 数据。证据保存在普通任务返回及原始工具回执中，没有运算符专用的 value/evidence 隐式协议。宿主检查调用与返回结构，不认证业务判断的真实性。

## 循环与并发

确定的循环由代码执行；彼此独立的 AI 工作可以声明为 async 模块（模块代码里则是 async 虚函数）并用 `Promise.all` 等待。共同材料和各项职责显式传参，父流程对实际产物执行验收。调用时读取参数和 `@uses` 点名的变量；恢复时不重放结果未知的操作。

## 规划与执行的职责边界

规划阶段将用户目标与业务 skill 编排成顶层流程和辅助函数；进入虚函数后，当前 AI 负责实现该函数的职责，可以直接理解、生成、转换、验收或使用工具并返回实际结果。当前调用已经满足“智能工作显式交给虚函数”的表达要求，执行者不必再次包装同一职责。

需要具体编排的工作写成代码：能直接写的步骤写在主程序里，需要 AI 的每一块写成 @module，由写模块的 AI 继续拆分。进一步拆分应产生职责明确、实际推进处理的子步骤；仅换函数名、传递相同输入并转交同一未完成工作，属于重复规划。虚函数说做不到是正常的信号，调用它的代码接住后拆细或换一种做法。

该边界由规划与执行指令表达。宿主继续负责调用、返回和记录，不根据函数注释猜测业务进展，不新增完成确认、业务状态或语义判重／嵌套深度限制。

## 持久化与兼容边界

源码整体编译通过后才开始执行。回复中的程序或模块代码验证失败时，宿主自动把 PROGRAM_SOURCE_INVALID 诊断交回写它的 AI，包含行列、附近源码及修正说明，保留当前任务和已有结果；AI 重写完整源码后用同样方式重新提交。被拒源码没有实际操作，宿主不自动修补或重放操作。运行时类型／算术错误仍沿 try/catch 传播，未捕获时按“失败与恢复”处理。任务返回 JSON，无 Markdown 围栏；无法完成可返回 {"$task_error":{"message":"具体原因"}}，沿调用链上抛。

agent_runtime_windows.state.program 保存源码、指令、函数帧、PC、变量、catch 栈、指令预算和挂起 effect ID。每次效果先持久化位置，已有回执恢复时直接消费，不重复计算已完成的更新或重放操作。缺少外部回执时结果未知，不能承诺跨进程恰好一次。

版本 13 及更早的现场、源码和回执保留，但拒绝按版本 14 续跑：作用域、对象引用、数值和条件语义已经变化，不能静默套用新规则。旧记录仍可作为历史查看，不自动重放已完成或状态未知的操作。源码上限 100,000 字节，最多 64 个顶层函数（连同嵌套函数和箭头函数最多 1024 个）、调用深度 64，每份运行中的代码（主程序，或一次模块调用）最多执行 1,000,000 条指令，普通函数调用算在调用方里；预算耗尽强制失败，不能被 catch 隐藏为成功。

## 统一的调用边界（版本 14）

内部函数调用保存 parameters 与 arguments 有序数组；普通 helper、虚函数和模块使用同一契约。program_task_input 投影 signature、description、bindings，虚函数还有 closure 和 skills，避免以 input 再包一层同名参数。模块代码的参数可改名但数量必须一致，按位置接收原实参；改变为一个“整体输入对象”的隐式重载已删除。

源码调用固定库方法；虚函数直接调用它范围内的工具。工具和 AI 结果保持实际契约，权限和回执检查仍由宿主执行。

声明保留完整工具说明，不再只截取首句；规划端与执行端都能看到后续条件、确认要求和注意事项。插件原文中的历史工具标识只作说明，唯一可调用名字仍以固定声明为准，底层 dispatcher 与外部 MCP/服务接口不因此改名。

资产索引 task_asset_index 的每个条目通过 delivery_reference 指向最近一次真实交付（task_id、event_seq）。需要验收证据号时使用这一引用，业务结果不追加宿主字段；快照的原始记录和再次交付仍可分别追溯。

## 函数作用域与工作空间预算

函数调用的实参在 program_task_input.bindings 中，`@uses` 点名的变量在 closure 中；虚函数看不到其他变量，也没有读取调用方变量的接口，要什么就在参数或 `@uses` 里写明。需要的参数和固定输入超过容量时明确报告。工具权限保持不变。

运行时在挂起效果的持久化边界保留仍处于函数作用域的源码变量，即使后续确定性代码没有引用；编译器临时量可按可达性回收。当前挂起效果持有自己的实际参数，恢复回执仍写回原目的变量；函数返回释放局部上下文，父／兄弟作用域不接收其工作全文。

上下文不会被折叠或压缩。虚函数返回时，它的 AI 工作上下文释放，调用方拿到返回值；确定性模块返回的是 JS 值，先回调用方的 JS heap，并不自动成为 AI 消息。只有打印、诊断、显式传给虚函数的输入和成果摘要进入相应 AI 上下文；虚函数的上下文放不下时以 `ContextOverflow` 错误结束；主窗口放不下的结果存为资产，AI 收到 `MATERIAL_OVER_CAPACITY` 和资产编号后，自己决定按页读取、释放已加载的材料或换一种做法。

## 示例的可执行回归

手册和 task-program Skill 中标为 js 的片段由测试提取原文编译，模块 process 的代码按模块代码编译。Skill 内嵌的完整示例还要和 examples/*.js 完全一致，并由测试直接运行原文，工具和 AI 的结果由测试控制，不重写流程。

| 示例 | 实际验证 |
| --- | --- |
| 手册和 Skill 里的所有 JS 片段 | 都能编译 |
| 最小串行 HTML（serial.js） | 实际写入、读回、检查；检查不通过时停止，不交付 |
| async 五页 HTML（parallel-five-pages.js） | 源材料读取、五个独立设计、实际写入与五项读回检查；检查不通过时停止；修正后复检 |
| 模块（module-notes.js） | 同一个模块处理三份文档，结果只写一次，读回核对 |
| Shell 模块（shell-in-module.js 与对应 implementation.js） | 主流程只调用模块；真实模块代码解析完整 JSON，限制摘要大小，拒绝非零退出码、截断和非法 JSON，并停止未结束的短步骤 |
| 二十页并发（parallel-twenty-html.js） | 能编译，虚函数和普通函数按声明区分 |

```bash
# 示例、语言规则和异常路径；不需要真实模型凭证
cargo test agent_program::tests:: -- --nocapture
cargo test shell_in_module_example -- --nocapture

# 用实际配置的模型跑两个完整示例；消耗模型额度，使用仓库外隔离目录
cargo test single_agent::documented_examples::real_model_serial_published_example -- --exact --ignored --nocapture
cargo test single_agent::documented_examples::real_model_async_five_pages_published_example -- --exact --ignored --nocapture
```

实现与断言见 [tests.rs](../src/agent_program/tests.rs) 和 [documented_examples.rs](../src/single_agent/documented_examples.rs)。新增或修改 JS 示例会随这些测试重新编译、执行；检查返回 false、编译通过和工具写入成功不能互相替代。真实生成仍需按实际产物验收，不把受控夹具通过当作模型质量保证。


### 模块结果与完整证据

模块按调用方要拿到的结果定义，例如核实过的事实、统计或附文件行号的结论；复用时检查返回结果是否适合当前任务，不能只看它能否执行命令。已知步骤继续写成普通 JavaScript；模块内收集、处理材料，需要 AI 判断时才调用职责窄的虚函数，不强制增加 AI 层。代码分析交给 AI 的证据应包含相关完整单元、外层条件和提前返回。搜索命中只能证明命中的一行。一个适量的完整单元用一次打印，并保留文件和行范围；不要按任意字符位置截掉尾部。宿主实际交付不下时明确报错并保存原结果，AI 根据读取引用分段查看或调整编排，保留已完成操作；未读内容不能作为结论依据。行为结论要核对完整控制路径或实际执行顺序，区分读过测试与运行过测试。

日志不再按单条 100,000 字符、200 条记录或最后 20 条静默裁剪。完整记录先保存在运行时；累计日志捕获超过堆预算时以 `OUTPUT_OVER_CAPACITY` 停止，拒收的完整记录也归档。主程序结局和模块探路结果在进入 AI 前按实际剩余上下文检查，放不下就明确失败，不交付成功前缀，原始输出、返回值和错误保存为 asset 并提供读取引用。资源错误由调用方调整编排，不作为普通实现错误自动修复重跑；已完成命令或写入不能盲目重放。只有 AI 下一次请求本身放不下时才是 `ContextOverflow`。显式按范围分页读取仍可使用，不把未请求的其余内容冒充已交付。


回合内定时器：支持 `setTimeout(callback, delay, ...args)`、`setInterval`、`clearTimeout`、`clearInterval`、`queueMicrotask`。句柄为数字 ID（取消也接受其字符串形式），两个 clear 可以互换；不是 Node Timeout 对象，不支持 ref/unref/refresh，普通回调的 this 是 undefined，bind 和箭头函数可保留接收者；不提供 Node Timeout 的 this 对象。delay 转成数字后取整数毫秒，小于 1、非有限或超过 2147483647 时按 1 毫秒处理。未取消的定时器使程序保持运行；回调的返回值忽略，未捕获异常使程序出错。Promise 与 queueMicrotask 共用 FIFO 微任务队列，每个定时器回调前排空微任务。回调内调用工具可以挂起恢复；async 回调的 Promise 不阻塞下一次定时器，未处理的拒绝按现有规则报错。

绝对到期时间、回调、参数和闭包保存在 Runtime 快照中。暂停/恢复不延长到期时间；过期 timeout 立即执行，interval 只触发一次、从实际派发时间安排下一次，不补跑积压次数。等待定时器时宿主同时等待并发调用，并响应取消和用户插话；停止、错误或回合结束后清理，不用于跨回合/跨天任务。

执行预算：两次已完成的宿主交互之间最多 1,000,000 条 VM 指令，整段程序另有 30 分钟墙钟期限（包含宿主等待和暂停时间）。快照保留计数与期限；真实宿主回复或宿主到期派发定时器回调刷新指令段，next()、存盘恢复和纯 Promise/queueMicrotask 微任务不刷新。耗尽以 INSTRUCTION_BUDGET_EXHAUSTED / PROGRAM_TIME_BUDGET_EXHAUSTED 停止，报告上限、已用量、累计量、位置和 hot_lines。中断交互的副作用可能未知，先核对再调整编排，不自动重放。


### Map / Set / WeakMap / WeakSet（阶段 1）

支持遵守 Symbol.iterator 协议的对象作为构造输入（包括数组、字符串、Map/Set、它们的迭代器及用户对象；Map/WeakMap 的每项须为键值对象）。null/undefined 构造空集合。Map/Set 保持插入顺序，键采用 SameValueZero：NaN 等于 NaN，正负零共用并保存为正零，对象键按引用身份。Map 有 get/set，Set 有 add；两者有 size、has/delete/clear、keys/values/entries/forEach，set/add 返回本集合。更新已有键不移动顺序，删除后重插排到末尾。

迭代器实时观察删除、追加和清空后的重插；一旦 done，后续追加不会让它重新开始。for…of、展开、数组解构、Array.from、Object.fromEntries、Promise 组合方法按 Symbol.iterator 接受内置集合、迭代器和用户可迭代对象。Array.from 的 mapper 随读取逐项调用；forEach 使用普通 VM 帧，回调调用工具时可以挂起恢复。集合元素、对象键身份、迭代位置随 Runtime 存盘；查找缓存恢复后从元素重建。

WeakMap/WeakSet 只允许对象键，set/add 非对象抛 TypeError；get/has/delete 的非对象查询返回 undefined/false。按本阶段约定用强引用保留对象，不暴露 GC 行为；没有 size、clear、枚举或 forEach。JSON.stringify 及工具/AI JSON 边界只看可枚举自有属性，普通集合为 {}；要传内容请明确用 Array.from(collection)。这与 Runtime 的完整堆快照不同。

Map/Set.forEach 现在支持 thisArg；原型链已支持，自定义 Symbol.iterator 已由阶段 4 接通。不把当前集合支持描述为完整 ES2015 一致性。新 prelude 会改变既有 prelude_hash，旧版本保存的运行时按现有保护规则拒绝直接恢复，原源码和输出仍保留。

### this 与函数调用（阶段 3 第一部分）

流程源码按严格模块语义执行：顶层 this、普通函数直接调用的 this 都是 undefined；对象方法（包括计算属性、带括号、可选调用和参数展开）保留接收者。显式 call(null) 的 this 仍为 null，传数字或字符串不装箱。箭头函数从创建处捕获 this，call/apply/bind 不会改掉它；参数默认值及其中的箭头函数也遵循此规则。

支持 f.call(thisArg,...args)、f.apply(thisArg,arrayLikeOrNull)、f.bind(thisArg,...prefixArgs)。apply 接受数组、按 length/数字下标读取的对象，null/undefined 表示无参数；其他原始值抛 TypeError，沿用最多 1,000,000 项的运行时数组上限。bind 保留第一次绑定的 this，重复绑定依次追加参数；绑定函数仍是函数，支持 typeof/instanceof Function、可读 name/length，并按函数规则被 JSON.stringify 忽略。name/length 的普通函数值及绑定元数据已实现，完整属性描述符和匿名函数名称推断尚不在本部分范围。

调用先取得实际函数，再计算参数，因此参数中修改同名方法不会替换本次已取得的函数；非函数在参数求值后抛 TypeError，报错点名方法。f()()、(a || b)()、arr[i]() 等使用实际求值得到的函数；可选调用短路时不计算参数。自有函数属性优先，push、slice 等名称不再覆盖用户自己的方法。支持给函数增加普通属性；原型继承与普通函数构造已在下一节补齐，描述符仍待后续部分。

Array 的 map/filter/forEach/some/every/find/findIndex/findLast/findLastIndex/flatMap、Map/Set.forEach、Array.from 的 mapper 支持 thisArg；箭头回调忽略 thisArg，reduce/sort/Promise 的普通回调使用 undefined。绑定函数和接收者进入 VM 堆快照与 GC 根，回调中的工具调用和 await 可挂起恢复，不重放已执行部分。宿主工具和 @module/@virtual 仍按原有参数/JSON 契约调用，不因 this 绑定额外向 AI 传入接收者。

已有 VM 方法已接入真实内置原型对象；属性描述符与访问器见下文，完整原型方法的泛型借用仍待补齐。禁止的旧全局 call()/ai() 包装与可用的 f.call/f.apply/f.bind 不同。定时器普通回调的 this 为 undefined，绑定函数和箭头捕获可保留接收者，但不提供 Node Timeout 的 this 对象。

### 原型链与普通函数构造（阶段 3 第二部分）

Object.create(proto)、Object.getPrototypeOf(value)、Object.setPrototypeOf(value,proto) 可用。原型为对象或 null；设置原型返回目标，拒绝循环链、改变冻结对象的原型，以及改变 Object.prototype 的不可变原型。相同原型无需改动，可以返回冻结对象本身。getPrototypeOf 对字符串/数字/布尔值返回对应包装原型，null/undefined 抛 TypeError；setPrototypeOf 在验证原型后直接返回非空原始值。Object(value)/new Object(value) 可以包装原始值。

读取属性及方法、自有属性覆盖和 delete、in、for…in、instanceof 都使用真实原型链。for…in 包含继承的可枚举属性，去重并跳过内置非枚举属性；Object.keys/values/entries、对象展开、Object.assign、JSON 与宿主输入仍只看自有可枚举属性。数组空位会在读取、map、内置迭代及 JSON 时取继承的数字属性。对象字面量中的 __proto__: object/null 用来设置原型，原始值忽略；JSON.parse 的同名键始终是普通数据。

Object/Function/Array/String/Number/Boolean/Date/RegExp/Promise/集合/Error 的已有能力接到共享内置原型对象；Function.prototype 是可调用的空函数。Function.prototype.call/apply/bind、Object.prototype.hasOwnProperty/isPrototypeOf 等标准写法可用，原型方法可以被替换和删除，之后的读取立即可见。方法取出后执行仍按内部接收者类型校验，改变对象的原型不会授予 Map/Promise/Date 等内部槽。

普通同步函数支持 new F(...args)：实例继承当前 F.prototype，非对象 prototype 回退到 Object.prototype；构造体的 this 为实例，返回对象时用该对象，返回原始值时仍用实例。绑定构造函数追加参数并委托目标，instanceof bound 也委托目标。箭头、async 函数、方法简写和 AI 声明不能构造。普通函数有非枚举 prototype 与 prototype.constructor；箭头和绑定函数默认没有自有 prototype。支持已有内置构造函数的别名/绑定调用。instanceof 接受任意求值得到的函数，不再只接受内置类名；右值不是函数或它的 prototype 不是对象时按 JS 抛 TypeError。

原型链接、内置原型身份、构造中的 this 和返回规则进入 Runtime 快照及 GC 根；构造或继承方法内的工具调用、await 可以存盘恢复。JSON/structuredClone(JSON 值) 不保存原型图；clone 的普通对象使用默认 Object.prototype。普通空函数现在合法，直接返回 undefined；只有显式注解表示 AI 工作。

公开属性描述符、用户 getter/setter 与 Reflect 已由第三部分接入。class 和 new.target 已由阶段 5 接入；完整原型方法泛型借用仍未提供；Promise 静态方法目前要求内置 Promise 构造函数，不支持自定义 constructor/species。动态 Function/eval 仍不可用。新 prelude 的旧快照保护规则不变。


### 属性描述符、访问器与 Reflect（阶段 3 第三部分）

Object.defineProperty(o,key,descriptor)、Object.defineProperties(o,descriptors)、Object.create(proto,descriptors) 支持数据和访问器描述符。数据属性使用 value/writable，访问器使用 get/set，二者不能混用；get/set 只能是函数或 undefined。创建时缺少的布尔标志默认为 false，修订时保留未提供的字段。不可配置属性不能任意改类型、枚举性或重新开放写入；不可写数据值按 SameValue 比较，NaN 等于自身，+0/-0 不相同。defineProperties 先读取、校验全部描述符，再开始定义；定义期间失败可能保留前面已经完成的定义。

Object.getOwnPropertyDescriptor/Descriptors 返回自有属性的描述符，不执行 getter。Object.getOwnPropertyNames 返回所有自有字符串键，Object.keys 仅返回可枚举字符串键。Object.preventExtensions 禁止新增属性，seal 同时禁止删除及重新配置，freeze 还禁止已有数据属性写入；isExtensible/isSealed/isFrozen 可查询。冻结访问器不会取消 setter，setter 仍可修改闭包或其他对象。数组 length 和下标遵守描述符约束；缩短长度遇到不可配置下标时保留已经完成的删除和修正后的长度。严格赋值与 delete 失败抛 TypeError。

支持对象字面量 get x(){...}/set x(v){...}，包括计算键。getter/setter 使用普通 VM 帧，保留最初 receiver，异常会传播；内部工具调用可挂起、存盘、恢复，闭包和属性引用参与 GC。成员读取、可选链、赋值及更新运算、对象解构和展开、Object.values/entries/assign、JSON.stringify、数组回调/内置迭代、Array.from 以及 apply/bind 元数据读取会执行相应访问器。键和描述符查询不执行访问器。流程输入输出仍是 JSON 数据边界；需要传出访问器产生的值时，显式读取或用上述转换生成普通数据；直接传带可枚举访问器的对象会报错，避免把 getter 当 undefined 丢掉。尚未迁入可挂起路径的数组原生读取方法遇到访问器会明确报错，可先 Array.from(a) 物化再调用。

Reflect 提供 13 个方法：apply/construct、get/set、defineProperty/deleteProperty、getOwnPropertyDescriptor、ownKeys、getPrototypeOf/setPrototypeOf、has、isExtensible/preventExtensions。目标必须是对象；get/set 可指定不同 receiver，显式传 undefined 与省略 receiver 不同。合法但被属性约束拒绝的 set/defineProperty/deleteProperty/setPrototypeOf 返回 false；无效目标、混合描述符或非法原型仍抛 TypeError。Reflect.construct 支持普通/绑定/类构造和另一个 newTarget，new.target 可在构造器中读取实际构造目标。

描述符、访问器和完整性状态进入 Runtime 快照；JSON 不保存它们。Symbol 已由阶段 4 接通，ownKeys 包含字符串及 Symbol 键，getOwnPropertySymbols 返回 Symbol 键。Proxy、动态 Function/eval 和通用原型方法泛型借用仍不在本部分。


### Symbol 与同步迭代协议（阶段 4）

Symbol(description) 每次创建不同身份，description 可以相同；Symbol.for(key)/Symbol.keyFor(symbol) 使用本 Runtime 的注册表。typeof 返回 symbol，Object(symbol) 创建包装对象。new Symbol() 抛 TypeError，但 Symbol 可作为 Reflect.construct 的 newTarget。String(symbol) 可显式取描述文本；隐式转为字符串或数字抛 TypeError。description、注册表、Symbol 键及集合中的 Symbol 身份均进入 Runtime 快照，不通过 JSON 模拟身份。

对象支持 Symbol 计算键、继承、访问器、属性描述符、in/delete 和 Reflect。键采用可逆内部编码，所有以零字符开头的用户字符串另行转义，因此不能和 Symbol 命名空间碰撞；普通键及 Symbol 键分别保持顺序。Object.keys/values/entries、for-in 和 JSON 不包含 Symbol 键；Object.assign、对象展开/剩余项和 getOwnPropertyDescriptors 包含它们。JSON.stringify 对 Symbol 值按标准省略对象字段、在数组中写 null，顶层返回 undefined。宿主工具、AI 函数和模块的数据边界仍是 JSON，Symbol 不能直接跨越这些边界；显式转换成字符串或普通数据。

本阶段提供 Symbol.iterator、Symbol.asyncIterator、Symbol.hasInstance、Symbol.toPrimitive 和 Symbol.toStringTag。instanceof 和 Object.prototype.toString 执行相应钩子；属性键、运算符/模板、String/Number 及单对象 Date 构造支持 toPrimitive，并在钩子缺失时使用普通转换方法。其他原生内置函数的通用对象转换仍可能受既有子集限制。Symbol.species、isConcatSpreadable 及正则相关 Symbol 钩子尚未开放。

同步 for-of、数组及调用参数展开、数组解构、Array.from、Map/Set/WeakMap/WeakSet 构造、Object.fromEntries 和 Promise.all/allSettled/race/any 均读取 Symbol.iterator；覆盖内置对象的该方法也会生效。Array/String/Map/Set 有内置可迭代协议；数组 keys/values/entries 返回实时迭代器，字符串按 Unicode 码点迭代。迭代器本身的 Symbol.iterator 返回自身。

每次开始迭代时缓存 next 一次，验证返回对象并读取 done/value。正常耗尽后不再取值；break、return、跨层 continue、绑定/mapper/集合 adder 等消费步骤抛错时调用 return 清理，保留迭代器 receiver。next、done 或 value 读取本身失败时不重复关闭；已有原始异常优先于清理异常。数组解构部分消费会关闭，省略项不读 value，rest 正常耗尽。for-of 的 let/const 保留右侧 TDZ 和每轮的新绑定。

迭代记录、缓存函数、游标、done 标记、闭包和部分执行的 next/return 都是 VM 值及普通帧，参与 GC 并随 Runtime 存盘。恢复后继续同一次调用，不重新取得迭代器、不重放 next 或 return。Symbol.asyncIterator 已接通 for-await 和异步生成器，详见下文；同步 function*/yield 与 yield* 也可用。


### class、继承与构造目标（阶段 5）

支持 class 声明/表达式、显式或默认 constructor、extends、实例和 static 方法、计算名称（含 Symbol）以及 getter/setter。类主体使用严格语义；方法不可枚举、不能 new，类构造器不能作为普通函数调用。声明有 TDZ，类内部名称是不可改写的绑定；匿名类可从变量绑定或赋值目标推断名称。

```js
class AnalysisError extends Error {
  constructor(message) { super(message); this.name = 'AnalysisError'; }
}
class Result {
  constructor(value) { this.value = value; this.createdBy = new.target; }
  summary() { return { value: this.value }; }
}
class DetailedResult extends Result {
  summary() { return { ...super.summary(), detail: true }; }
}
const result = new DetailedResult(7);
console.log(result.summary());
```

派生构造器在 super() 成功返回之前访问 this 会抛 ReferenceError；重复初始化也抛 ReferenceError。派生构造器可直接返回对象；返回非 undefined 的原始值抛 TypeError，正常结束或 return undefined 则要求 this 已初始化。super() 使用实际父构造器；super 属性从方法定义时的 home object 的原型查找，用实际 this 调用 getter/setter/方法，提取方法或在箭头函数中使用也成立。支持计算访问、赋值、复合赋值和更新。计算名称与类型转换的顺序保留，不能用动态 this 的原型代替 home object。

new.target 在普通调用和方法调用中是 undefined，在 new、绑定构造和 Reflect.construct 中指向有效构造目标；箭头函数从外围捕获。Error、Array、Map/Set/WeakMap/WeakSet、Date、RegExp、Promise、Object 和 Number/String/Boolean 子类保留内置槽位与相应原型关系。构造器、super 调用/访问器及 newTarget.prototype getter 可以在宿主调用处挂起；派生 this 的未初始化状态、home object、只读类名绑定和 new.target 都进入 GC 与 Runtime 快照，恢复后接着执行。JSON 只携带可枚举数据，不保留类身份或方法；变更 prelude 后沿用旧快照拒绝恢复的规则，历史源码与输出保留。

类字段、私有字段、static 初始化块、动态 Function/eval 和异步迭代仍不提供；同步生成器见阶段 6。Promise/Array 等内置 species 算法、完整原型泛型借用、arguments、部分函数名推断及原生构造器的所有通用对象转换仍有缺口；支持继承不表示完整实现所有内置构造算法。对应限制与 test262 实际结果见 tests/vm/REPORT-stage5.md。


### 异步调用、名称推断与诊断修复（阶段 5 补充）

Promise 的本地回调统一经过正常 VM 调用入口；普通函数、绑定函数、内置函数、类方法和 async 函数使用相同的 this、home object、构造检查、类型转换与 Promise 采用规则。本地 undefined、Symbol 或对象不会仅因回调分发就被转换成 JSON；真实工具/AI 调用仍遵守各自的 JSON 参数契约。内置回调和工具参数错误保留 .then 注册位置，用户回调自身发生的错误保留用户源码位置；这些信息也参与存盘恢复。

匿名函数、箭头和类在声明、标识符赋值、逻辑赋值、参数/解构默认值、对象属性值中推断 name，支持计算属性及 Symbol 名称。显式函数/类名称不被覆盖；类自身声明的 static name 成员不被覆盖。推断 name 不会增加一个词法自引用变量，复合算术赋值不推断名称。名称保留 UTF-16 码元，字符串读取入口不再强制把已有 UTF-16 值转换成 Unicode 标量字符串。

Number.prototype.toExponential(fractionDigits?) 支持普通/包装数字和 Number 子类。省略参数时输出最短指数形式；显式参数为 0..100，按实际二进制数的精确十进制展开取舍，不使用 Rust 默认的 ties-to-even 精度格式化替代 JS 规则。参数对象的转换可以挂起恢复，非 Number receiver 抛 TypeError，有限数的越界位数抛 RangeError，NaN/Infinity 的转换与检查顺序保留。

未识别的内置方法会先报告具体方法名称，不再先尝试 JSON 转换而给出误导性边界错误。动态 Function/eval 仍按原约定禁用；Function 的子类构造限制仍在，不因本轮测试修复而开放动态代码生成。详细测试和剩余问题见 tests/vm/REPORT-stage5fix.md。


### 同步生成器与持久化（阶段 6）

支持 function* 声明/表达式、对象和类的生成器方法、yield、yield*，以及 next(value)、return(value)、throw(error)。调用时完成参数初始化，首次 next 才执行函数体，首次 next 的参数被忽略。每个实例都有独立的局部变量、闭包、this、home object、异常处理器和指令位置；生成器不能 new，重入 next/return/throw 抛 TypeError。

next 返回 {value,done}；return 和 throw 可进入 finally，finally 内再次 yield 时保留尚未完成的返回值或异常。yield* 按迭代协议缓存 next，保留接收者、结果对象身份和访问器顺序，转发 return/throw；缺失 throw 时按规范关闭被委托迭代器。可供 for-of、展开、解构、Array.from 等既有同步迭代消费者使用。

```js
function* chunks(items, size) {
  for (let i = 0; i < items.length; i += size) yield items.slice(i, i + size);
}
for (const batch of chunks([1, 2, 3], 2)) console.log(batch);
```

生成器的开始前状态、yield 暂停点、委托状态、finally 中待完成的返回/异常，以及执行到宿主调用时的全部调用帧，都参与 GC 和 Runtime 存盘恢复；恢复后继续执行，不重新创建生成器或重放已完成的副作用。JSON.stringify 只序列化可枚举数据，不保存生成器执行状态；structuredClone 不支持生成器，跨宿主边界请先物化需要的数据。异步生成器和 for-await 已在阶段 7 接通；动态 GeneratorFunction/AsyncGeneratorFunction 构造仍未开放。


### 异步迭代、异步生成器与持久化（阶段 7）

支持 async function* 声明/表达式、对象和类的 async 生成器方法、yield/yield*，以及 for await...of。调用生成器时初始化参数，首次 next 执行函数体；next/return/throw 总是返回 Promise。等待期间的调用按请求队列依次执行，重入调用会排队；不沿用同步生成器的“正在执行”错误。错误接收者通过 Promise 拒绝报告。

yield 表达式和显式 return 表达式先 await 值；return; 和自然结束没有这次额外等待。return(value) 在开始前、yield 暂停或完成状态都会等待 value；等待拒绝可进入 yield 周围的 catch/finally。next(value) 和 throw(error) 不自动等待输入值。异步 yield* 优先使用 Symbol.asyncIterator，转发并等待 next/return/throw 的结果，但保留自定义异步迭代器结果中的 value（即使它是 Promise）。

for await 优先使用 Symbol.asyncIterator；没有时回退到同步迭代器，并等待同步结果的 value。异步迭代器的 value 则原样传入循环体。next 只读取一次并缓存，校验结果对象；break、return 和循环体抛错执行异步 return 清理。正常耗尽和 next/done/value 读取失败不会额外清理；已有的异常优先于清理错误。协议调用保持规定的微任务等待顺序。

```js
async function* summarize(batches) {
  for (const batch of batches) yield { count: batch.length };
}
const batches = [[1, 2], [3]];
const summaries = [];
for await (const result of summarize(batches)) summaries.push(result);
```

等待值、请求队列、当前调用帧、缓存 next、闭包、this/super、异常处理器和 finally 中待完成的返回/异常全部参与 GC 和 Runtime 存盘；恢复继续同一次调用，不重放已完成的副作用。JSON.stringify/structuredClone 不保存这些执行状态。await 的 PromiseResolve 边界会读取 Promise.constructor 并同化普通 thenable；getter 和 thenable 回调可经过宿主调用挂起恢复。Promise.resolve 与构造器 resolve 的一般 thenable 同化仍是已有独立限制，本轮没有把整个 Promise 实现声明为完整兼容。
