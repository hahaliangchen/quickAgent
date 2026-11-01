# Agent 流程伪代码与 Rust 执行器

> 本手册描述当前 Rust JS 子集，不代表最终目标必须使用自建 VM。设计理念与待验证的 Node 方向见 [设计与实现入口](agent-design-guide.md)。

当前语言版本 **14**。使用常见 JS 基础语法表达流程，由 SWC 解析、Rust 执行持久化指令。内部值使用虚拟机堆以保留对象引用，变量按 JS 的块级作用域和词法作用域存放；宿主工具和 AI 仍通过 JSON 交接。**普通表达式由宿主确定性求值，智能判断必须写成显式任务函数。** 相同程序、相同输入和相同工具／任务返回值产生相同控制流；AI 任务及外部操作本身不因此变成确定性的。

流程就是普通的 JavaScript。宿主实现其中的一个子集，本手册说明这个子集、尚未支持的功能和宿主提供的工具库。业务 skill 提供需求和流程，语言负责表达函数、数据依赖和控制流，宿主负责执行；学习语言不替代加载当前业务的 skill。

## 运行方式与工具导入

主任务的 AI 把完整流程写成一段 JS，放在回复的一个 ```js 代码块里；宿主收到后直接编译运行，不需要另外调用提交工具。用户请求就是主任务，由宿主建立。程序像 JS 模块一样从顶层第一条语句开始顺序执行，没有入口函数，也不需要返回值；需要的数据写在代码里，或用导入的工具读取。宿主自带的工具通过 `import * as io from "host:io"` 或 `import {read as load} from "host:io"` 引入；支持命名导入和命名空间导入，别名仅为本地绑定，不扩大权限。也可以像 Node 里 `require("fs")` 那样写 `const io = require("io")`，执行到这一行时拿到同一组方法，方法是否存在在调用处才检查。没有 import 的程序里，宿主自带的库按名字就能用；程序自己声明了同名的变量时，这个名字归程序自己。插件和项目技能的库是第三方库，在用到的地方用 `const ppt = require("ppt")` 加载：执行到这一行，宿主才链接这个库，返回它的方法对象；同一次运行里再 require 拿到同一个对象；找不到就在这一行抛出可以捕获的 `MODULE_NOT_FOUND`。参数必须是库名字符串。`host:` 只用于宿主自带的库，用它引入插件库会收到改用 require 的诊断。工具仍以一个 JSON 对象传参并经过原权限检查。默认导入、export 和其他模块导入不支持。顶层运行完毕表示流程结束，不代表业务验收自动通过。

program_tool_headers 是精简的目录：宿主自带的库列出方法，第三方库只列名字和用途；完整契约由 read_tool_contracts 按需获取，不随每次请求重复注入。

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

`compose` 和 `inspect` 带 `@virtual`，需要 AI 理解与处理；写文件、读文件及布尔判断有确定规则，直接由宿主执行。普通函数调用使用局部 Runtime 函数帧，不创建 AI 任务节点，也不启动另一个 Agent。此例用 HTML 说明串行数据流，只检查实际执行过的内容／结构，不声称已经完成浏览器视觉验收。

已有的真实结果通过参数交给需要它的函数，不因为进入虚函数就重读原材料。虚函数可在同一上下文连续观察、调用工具和处理；它只做一件事时最稳：只检查，或只处理，检查之后要不要处理由代码决定。调用方对函数契约中可以确定的输入与输出关系做代码核对，不能把不匹配的返回结果当作有效验收。处理实际状态后需要复检时才重新读回；多步的复杂处理写成代码或模块，不交给一个虚函数。

虚函数是流程中的一次 AI 能力调用，有明确输入、职责和返回值；不要把它当作省略整段流程实现的默认容器。能具体表达的读取、遍历、判断分支、写入和复查等步骤尽量由普通函数编排；需要 AI 理解、判断或生成的部分才交给职责明确的虚函数，调用方再用其返回值继续推进。划分应符合当前任务，虚函数内仍可按职责和授权逐步使用工具、处理与复查，无需为了形式强制拆分。

验收要覆盖当前任务的实际要求，禁止用表面特征冒充完整验收。基础文本、数量或存在性检查只能证明其实际检查到的事项，不能单独证明内容正确或资源可用。可确定的步骤尽量写成普通代码，需要理解与判断时调用职责明确的 AI 函数或适用工具；未检查或无法确认的事项如实报告。遇到语言限制或工具缺失时，不得删减必要检查后将结果当作全面通过，应该改用其他可行能力或说明未完成的原因。

把这份源码原样放进回复的 ```js 代码块，宿主就会运行它。回复里没有 ```js 代码块时是普通回答，不运行任何程序；只想展示、不想运行的代码请用其他标记的代码块。

## 异步函数、工具和并发

宿主自带的工具从 `host:<library>` 导入，同步等待实际回执；需要异步工具时从 `host:<library>/promises` 导入。用 require 时，异步方法用 `require("<库名>/promises")` 加载。async 函数和 async 虚函数返回 Promise，调用方可在等待前继续执行。`Promise.all`、`allSettled`、`race`、`any`、`then`、`catch`、`finally` 提供组合与错误传播；all 和 allSettled 按输入顺序排列结果，race 和 any 按完成结果决定。

独立只读操作可由宿主合批，最多 32 项一批，逐项记录成功或失败。多个 async 虚函数运行在独立窗口，调用时固定参数和 `@uses` 点名的变量；模型请求共用并发队列，默认三个槽位。共享文件应由调用方统一写入；并发函数对同一文件的整文件写入会保留冲突候选并停止。

[五页示例](../skills/task-program/examples/parallel-five-pages.js)并发设计，统一写入并读回，再并发验收。[二十页示例](../skills/task-program/examples/parallel-twenty-html.js)用相同模式生成独立页面。示例不声称执行过浏览器或视觉检查。

## 四类调用和执行者

| 写法 | 谁处理 | 何时完成 |
| --- | --- | --- |
| 普通 `helper(args)` | 宿主进入函数局部作用域并执行其代码 | 函数 `return` 后恢复调用方；注释不会自动执行 |
| `@virtual` 的 `helper(args)` | 一个 AI 读职责注释和实际参数，用工具循环完成这一件事 | 返回约定的 JSON 值 |
| `@module` 的 `helper(args)` | 第一次调用时 AI 按契约写成代码，之后对同一契约的调用直接运行这份代码 | 代码 `return` 后恢复调用方 |
| `library.method({...})` | 已发布的真实工具：import 的宿主库，或 require 得到的第三方库对象 | 得到工具原始 JSON；业务验收仍由流程定义 |

普通函数按词法作用域执行。虚函数只看到自己的参数和契约里用 `@uses` 点名的变量，也就是它的闭包，值在调用时读取；async 虚函数发起之后，父流程变量的变化不会同步过去。模块只通过参数拿输入。

## 模块：AI 写成的代码

`@module` 函数是一块完整的工作。第一次调用时，宿主把它的签名、契约注释和这次调用的实参交给一个 AI，由它回复这个函数的完整代码；编译通过后存成这个聊天的模块文件再运行。之后对同一契约（函数名、签名和注释原文完全一样）的调用直接运行这份代码，不再请求 AI，所以模块要写成通用代码，实参只是一个例子。同一契约的几个调用同时第一次运行时，只写一次。示例见 [module-notes.js](../skills/task-program/examples/module-notes.js)。

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

顶层和 async 函数可以 await Promise；`Promise.all` 等组合子支持 async 虚函数与异步工具。Promise 回调和 await 恢复按 JS 微任务顺序执行：async 函数返回 Promise、`then` 回调返回 Promise 或 resolve 传入 Promise 时，与 JS 一样多等两轮。直接把顶层 async 函数（含 async 虚函数）或 `host:<库>/promises` 工具的调用结果当作值使用时编译失败，例如 `inspect(page).passed`、`if (inspect(page))`、`assert(inspect(page))`、`` `${work()}` `` 或参与运算；赋值保存、传给 `Promise.all`、调用 `then`/`catch`/`finally` 和 `void` 不受影响。局部函数值或变量保存的 Promise 在编译期无法识别，运行时读取 Promise 属性时报“this is a Promise; the value may need await”。运行时实现的是 JS 子集，不是完整 Node.js。支持对象和数组成员的赋值、复合赋值及自增自减；声明、普通函数参数和 catch 支持嵌套解构、默认值及解构剩余项，对象和数组字面量支持展开；支持可选函数调用、剩余参数及调用参数展开。非简单参数先在独立的参数作用域中初始化；默认值及其创建的闭包不会看到函数体内的同名绑定。其余仍禁止 function*/yield、call()/ai() 包装、除 host: 库之外的 import（第三方库用 require）和 export、TS 类型／断言、class/this/自定义 new、BigInt，以及 JS 全局环境。`new RegExp` 和 `new Promise` 是构造例外。源码字符串里的这些词语只是数据，不按词语拦截。需要智能处理时声明注释任务；没有“计算失败后自动交给 AI”的路径。

函数是值：嵌套函数声明、函数表达式、箭头函数和对象方法简写创建闭包，可保存、传递和调用。数组回调支持同步函数值、多语句和嵌套调用，包括在回调中调用工具并跨存盘恢复；字符串 replace/replaceAll 的替换函数也支持此方式。数组 map 支持 async 回调；其他数组回调当前仍要求同步。支持通过索引赋值和 length 产生稀疏数组；数组字面量空位及一般非索引数组属性尚不支持。

## 确定性的值规则

- 数字使用 IEEE-754 双精度；大整数可能舍入，除零产生非有限数。非有限数无法直接通过宿主 JSON 边界。数字转换仍未覆盖完整 JavaScript 语义。
- 基本数字、字符串和布尔值支持部分算术及比较转换；对象到原始值的转换尚未实现。=== 对对象比较引用身份，== 对基本原始值支持部分转换。
- if、循环条件、!、&&、||、三元与 assert 使用 JS 风格真值：空字符串、false、null、undefined、0 和 NaN 为假，数组和对象为真。&&/|| 返回所选操作数；?? 在 null 或 undefined 时执行右侧。缺少对象字段返回 undefined；完整 JS 语义尚未支持。
- 三元只执行选中分支；函数／工具调用可以出现在表达式中，按从左到右的确定顺序执行。对象和数组的引用可在普通函数和别名之间共享；宿主工具、AI 参数和结果仍使用 JSON。push 在实参求值后追加到本地数组。后缀更新返回旧值，前缀返回新值。
- 模板插值与数组 join 允许 JSON 原始值，null 转为 "null"；数组／对象需要显式 JSON.stringify。字符串 length、索引、slice 与 indexOf 的单位统一为 Unicode 标量，不是 UTF-16 单元。slice 使用整数下标并支持负数；JSON.stringify 使用稳定键序。
- let/const 属于所在的块：内层块可以重复声明同名变量并遮蔽外层，声明执行前读取或赋值抛出 ReferenceError，同一块内重复声明在编译时拒绝。var 属于所在函数并提升，函数开始时为 undefined，可重复声明，重复无初值 var 不重置旧值。`for (let …)` 每轮迭代使用新的绑定；catch 参数只在 catch 块内可见；给未声明的变量赋值在编译时拒绝。for...of 读取当前数组的内容。

数据方法只有文首列出的白名单：includes/indexOf 接受一个查找值（字符串接收者要求字符串实参；数组使用 JSON 值相等），slice 接受零到两个整数下标；startsWith/endsWith/split 接受一个字符串；trim/toLowerCase/toUpperCase 无实参；join 接受零或一个字符串分隔符（默认逗号）；JSON.stringify/parse 恰好一个实参。类型或实参数量不匹配明确报错，不猜测方法含义。

`at` 接受一个整数，支持负索引，越界返回 null；字符串按 Unicode 标量索引。`concat` 接受一个参数：数组追加值或数组元素，字符串只接受字符串。

`typeof` 支持已声明的值，null 和数组返回 `"object"`；未声明的名字仍在编译期拒绝。`Array.isArray` 判断数组；`Object.keys/values` 接受对象，整数键按索引排序，其余键按插入顺序排列。通过 JSON 输入的原始键顺序可能已经改变。

路径和 URL 由宿主 Rust 直接计算：如 `path.join("docs", "page.html")`、`path.resolve("/project", "docs", "page.html")` 和 `url.parse("../a.html", "https://example.com/docs/")`。`url.parse` 返回 `{href,origin,protocol,hostname,port,pathname,search,hash}`；`path.resolve` 至少要求一个绝对路径，`path.relative` 的两个参数必须均为绝对路径，避免隐式依赖服务进程目录。路径按运行平台的规则进行词法处理，不访问文件系统或解析符号链接；输入最多 64 KiB，输出最多 256 KiB。常见 POSIX 路径和 URL 行为与 Node 对照测试，Windows 复杂路径尚未做真实环境兼容性验证，不承诺完整 Node 行为。调用不启动外部进程或 AI 任务；文件读取与修改仍使用 `io` 工具。

`lastIndexOf` 接受一个字符串，返回 Unicode 标量下标或 -1。`replace` 和 `replaceAll` 分别替换首次或全部匹配；查找可以是字符串或正则（`replaceAll` 的正则必须带 `g`），替换文本按 JS 规则展开 `$$`、`$&`、`` $` ``、`$'`、`$1` 和 `$<name>`，也可以是替换函数（接收匹配、捕获组、位置和原文，有命名组时再接收 groups），结果最多 4 MiB。

支持正则字面量，例如 `/a(\\d+)/i.test(text)` 和 `text.match(/a(\\d+)/g)`，也支持 `new RegExp(pattern, flags)` 和复制已有正则。可用标志为 `g`、`i`、`m`、`s`、`u`；底层采用 Rust regex 的 Unicode 语义及语法，不支持回溯引用、环视或 JS 特有语法。字面量的非法模式和标志在编译时拒绝，动态构造的在运行时拒绝。`test` 无状态，即使有 `g` 也不维护 JS 的 `lastIndex`；`match` 不匹配返回 null，非 `g` 返回完整匹配与捕获组组成的 JSON 数组（未匹配组为 null，不含 JS 的 index/groups 属性），`g` 返回全部完整匹配的数组。正则以内部 JSON 描述保存；除 `replace`/`replaceAll` 外，其他正则方法暂不支持。模式最多 8192 字节，输入最多 2 MiB，结果最多 10000 项及 4 MiB 捕获文本；超出时报错。正则不替代结构化 HTML 解析。

## 工具声明与固定名字

模型请求发布 TS 风格的 program_tool_headers 索引，列出当前允许的宿主库方法，以及可以 require 的第三方库的名字和用途，不是参数契约。使用 read_tool_contracts({methods:["io.read"]}) 或 read_tool_contracts({libraries:["io"]}) 按需取得真实参数、约束、返回类型和完整说明。虚函数不写代码：它范围内的工具作为原生函数直接调用，范围是声明它的代码 import 和 require 的方法，缺少时用 find_library_methods 查找；宿主照样校验原始 schema 与权限。程序源码用 import 或 require 引入宿主库，用 require 加载第三方库；声明资料不是可执行源码。主 AI 写程序之前先熟悉任务：能直接调用几个读、运行和看图的方法，缺的用 find_library_methods 查，查不到写入类方法；程序开始执行时，这一轮此前的工具往来离开它的上下文（[设计文档](agent-design-guide.md)第 4 节）。

宿主自带的工具按能力分库，一个库只管一类能力，没有兜底的库（[设计文档](agent-design-guide.md)第 5.2 节）：io 是文件（read/read_lines/write/list/search/edit/replace/workspace_info），process 是运行程序和项目脚本、管理项目进程，http 是 probe，browser 是浏览器页面，code 是代码地图，code_go/code_rust/code_ts/code_python 是各语言的符号，memory 是工作记忆、架构记忆和符号的业务上下文，vision 是看图和图像分析，attachments 是读附件，session 是主对话自己的材料和记录（只有主脚本能用）。方法名去掉和库名重复的前缀（browser.open、http.probe），其余沿用原工具名。插件在 plugin.json 或远程握手的 plugin 中声明 library，例如 {"id":"ppt-master","library":"ppt"}；已登记的 ppt_master__get_design 对外的名字是 ppt.get_design：程序写 `const ppt = require("ppt")`，再调用 `ppt.get_design({...})`。插件的方法不需要先用 read_skill 激活。未声明 library 的旧插件按 ID 将连字符替换为下划线，作为固定兼容名字。项目技能的库名就是技能名。库名必须是有效、非保留的标识符，不能占用宿主自带的库名和 host；不同插件不能占用同一个库名。方法来自实际注册工具，不按业务含义猜测。

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

## 第三方库与技能说明

插件和项目技能带的说明（SKILL.md）是库的文档。和普通语言里的函数一样，一个函数只带它自己用到的东西：哪段代码引用了这个库，说明就作用在那段代码上，不按调用关系往下传。

- 写代码的 AI 先用 `read_skill` 读要用的技能，读到的说明只留在它自己的上下文里。
- 代码 require 了某个库，宿主就把这个库的说明交给负责这段代码的 AI：主程序的是主 AI，包括自愈时；模块的是写它和修复它的 AI。
- 虚函数只带它在契约里用 `@uses` 点名的库的说明；没点名的虚函数不带任何技能说明，被调用的模块也不继承调用方读过的技能。
- 只有说明、没有方法的技能也用 require 引用，得到一个没有方法的对象，作用是把说明挂到这段代码上。

```js
const md = require("markdown");
const notes = ["要点一", "要点二"];
const section = writeSection(notes);
/**
 * @virtual
 * 把 notes 写成一节 markdown，返回 {text:string}。
 * @uses md 按它的规则写
 */
function writeSection(notes) {}
```

## 任务函数与工具步骤

业务 skill 和用户决定范围、依赖、确认点、验收与交付；通用 task-program skill 负责表达这些关系。宿主为每个用户请求建立主任务；AI 准备缺失的技能／工具契约后，在回复中给出一段覆盖完整工作、检查与交付的 JS 程序。

普通 helper 在宿主的局部函数帧中执行，不创建 AI 任务节点；工具调用是实际操作步骤。显式的 @virtual 或 @module helper 创建独立的 AI 上下文，把职责注释与命名实参交给 AI。标记放在紧邻函数的前置 JSDoc 或函数体注释中，体内可为空，或仅写 throw new Error("固定说明") 作为占位；不允许占位 return 或其他可执行语句；未标记的空函数拒绝编译。返回结构写入注释契约，实际返回来自 AI 或模块代码。普通代码中的注释没有执行效果。虚函数的 AI 用工具循环直接完成这件事；需要写成代码的工作声明为 @module。注释应说明职责、输入与返回字段。所有调用一律按位置绑定实参，模块代码保持契约的参数个数；不按名字猜测，也不把多参自动打包成单对象。bindings 中的值就是局部参数值，没有额外 input 包装；closure 列出契约里 `@uses` 点名的变量。

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
/** @virtual */
function produce(request) {
  // 按 request 制作实际产物，返回路径和版本。
}
/** @virtual */
function inspect(artifact, request) {
  // 读取或检查实际产物，对照 request 判断。
  // 返回 {passed:boolean, evidence:[实际工具观察], issues:[...] }。
}
/** @virtual */
function repair(artifact, check, request) {
  // 使用 check 的证据完成授权范围内的修正，返回实际产物。
}
/** @virtual */
function deliver(artifact, request) {
  // 完成 request 要求的检查与交付，返回真实结果和限制。
}
```

这个修复示例最多尝试一次修复。修复后必须再次 inspect；复检仍未通过就由 assert 停止，deliver 不会执行。

inspect 的判断由 AI 作出；!check.passed 由宿主消费布尔值。不再把 artifact !== expected 解释为“不满足预期”；它只比较 JSON 数据。证据保存在普通任务返回及原始工具回执中，没有运算符专用的 value/evidence 隐式协议。宿主检查调用与返回结构，不认证业务判断的真实性。

## 循环与并发

确定的循环由代码执行；彼此独立的 AI 工作可以声明为 async 虚函数并用 `Promise.all` 等待。共同材料和各项职责显式传参，父流程对实际产物执行验收。调用时读取参数和 `@uses` 点名的变量；恢复时不重放结果未知的操作。

## 规划与执行的职责边界

规划阶段将用户目标与业务 skill 编排成顶层流程和辅助函数；进入虚函数后，当前 AI 负责实现该函数的职责，可以直接理解、生成、转换、验收或使用工具并返回实际结果。当前调用已经满足“智能工作显式交给虚函数”的表达要求，执行者不必再次包装同一职责。

需要具体编排的工作写成代码：简单的直接写在主程序里，完整的一块写成 @module。进一步拆分应产生职责明确、实际推进处理的子步骤；仅换函数名、传递相同输入并转交同一未完成工作，属于重复规划。虚函数说做不到是正常的信号，调用它的代码接住后拆细或换一种做法。

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

上下文不会被折叠或压缩。函数返回时，它的上下文整体释放，调用方只拿到返回值；虚函数的上下文放不下时以 `ContextOverflow` 错误结束；主窗口放不下的结果存为资产，AI 收到 `MATERIAL_OVER_CAPACITY` 和资产编号后，自己决定按页读取、释放已加载的材料或换一种做法。

## 示例的可执行回归

手册的三段 JS 和 task-program Skill 的六段 JS 都由测试提取原文编译，模块 process 的代码按模块代码编译。Skill 内嵌的完整示例还要和 examples/*.js 完全一致，并由测试直接运行原文，工具和 AI 的结果由测试控制，不重写流程。

| 示例 | 实际验证 |
| --- | --- |
| 手册和 Skill 里的所有 JS 片段 | 都能编译 |
| 最小串行 HTML（serial.js） | 实际写入、读回、检查；检查不通过时停止，不交付 |
| async 五页 HTML（parallel-five-pages.js） | 源材料读取、五个独立设计、实际写入与五项读回检查；检查不通过时停止；修正后复检 |
| 模块（module-notes.js） | 同一个模块处理三份文档，结果只写一次，读回核对 |
| 二十页并发（parallel-twenty-html.js） | 能编译，虚函数和普通函数按声明区分 |

```bash
# 示例、语言规则和异常路径；不需要真实模型凭证
cargo test agent_program::tests:: -- --nocapture

# 用实际配置的模型跑两个完整示例；消耗模型额度，使用仓库外隔离目录
cargo test single_agent::documented_examples::real_model_serial_published_example -- --exact --ignored --nocapture
cargo test single_agent::documented_examples::real_model_async_five_pages_published_example -- --exact --ignored --nocapture
```

实现与断言见 [tests.rs](../src/agent_program/tests.rs) 和 [documented_examples.rs](../src/single_agent/documented_examples.rs)。新增或修改 JS 示例会随这些测试重新编译、执行；检查返回 false、编译通过和工具写入成功不能互相替代。真实生成仍需按实际产物验收，不把受控夹具通过当作模型质量保证。
