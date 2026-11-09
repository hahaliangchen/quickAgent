---
name: task-program
description: 将用户目标与业务 skill 表达为可执行的流程程序：顶层执行、虚函数（AI 钩子）、模块（AI 写成并复用的代码）、工具调用、确定性表达式和基础控制流。
---

# 用程序表达任务

语言版本 14。用普通 JS 基础语法表达完整流程；Rust 宿主执行普通计算，AI 只出现在两种显式声明的函数里：`@module` 模块由 AI 按契约写成代码，之后的调用直接复用；`@virtual` 虚函数由 AI 在运行时完成一件事，写在模块的代码里。主程序里需要 AI 的地方都写成模块，主程序里声明了虚函数，宿主在交出时就拒绝。业务要求、确认点、验收标准来自用户和业务 skill，不写进通用宿主。

## 从完整示例学习

流程就是普通的 JavaScript：宿主实现其中的一个子集，工具是库方法（宿主自带的从 `host:<library>` 导入，或者 `require`；插件和项目技能的在用到的地方 `require`），AI 工作是 `@virtual` 或 `@module` 函数。下面三份完整程序供查阅；宿主不会在每次规划时自动注入本 Skill 或示例。完整说明见 [语言手册](../../docs/agent-program.md)，包括运行方式、逐阶段数据流、失败处理和尚未支持的 JS 功能。示例只是教学，不把 HTML、页数或分工规定为其他业务的流程。

主程序负责编排：能写成代码的直接写，需要 AI 的每一块交给一个模块。分几个模块、怎么组合由你决定，和写项目一样；前两个示例的每个模块只负责一件事，第三个示例用同一个模块处理几份文档。

### 最小串行程序

源码：[serial.js](examples/serial.js)。调用 write_program 交出程序（这一次回复里只调用它），宿主把它写成这个聊天文件夹里的文件，再从文件读出运行；回复正文里的代码不会运行。用户请求中的数据直接写进代码。普通函数调用顺序执行。compose 返回正文，工具保存并读回，inspect 对实际正文检查，断言通过才交付。两个模块各做一件事：一个写，一个查；保存和读回由主程序完成。模块的代码在第一次调用时由 AI 写出，需要 AI 生成或判断的那一小步，在模块里声明虚函数。

```js
import * as io from "host:io";
// 最小串行流程：设计 → 保存 → 读回 → 验收 → 交付。
const request = {
  topic: "通用 Agent 的职责边界",
  output: "agent-overview.html",
  requirements: "制作一个独立中文 HTML，说明用户、AI、通用宿主、插件和工具的职责。内嵌 CSS，可在桌面与手机阅读；准确区分流程结束与业务验收。"
};

const draft = compose(request);
const receipt = io.write({path: request.output, content: draft.content});
const actual = io.read({path: request.output});
const review = inspect(request, actual.content);
assert(review.passed, "验收未通过，不能交付");
// 读写和验收完成后，流程自然结束。

/** @module */
function compose(request) {
  // 按 request.topic 和 request.requirements 设计一个完整、独立的中文 HTML。
  // 返回设计好的 {content:string}，不只返回计划。
}

/** @module */
function inspect(request, content) {
  // content 是已保存文件的真实读回正文；对照 request 检查内容与 HTML 结构。
  // 返回 {passed:boolean, issues:[string], evidence:[string]}。
  // 只做实际能完成的检查，未经浏览器渲染不能声称视觉验收通过。
}
```

### 并发制作并验收五页

源码：[parallel-five-pages.js](examples/parallel-five-pages.js)。读取真实源材料后，每页通过 async 模块并发设计；父流程统一写入，再对实际读回的内容并发验收，未通过的页修正一轮后复检。设计、验收、修正是三个各做一件事的模块，同一契约只写一次代码，五页共用；多步的流程写在主程序里。失败进入 `await` 的异常路径。

```js
// 五页并发设计、统一写入，再并发检查实际读回的内容；未通过的页修正一轮后复检。
import * as io from "host:io";
const request = {
  source: "docs/agent-design-guide.md",
  requirements: "制作五个内容不同、风格协调的独立中文 HTML。忠实于源材料的当前设计，准确区分已废弃的机制和现行设计；内嵌 CSS、无外部资源依赖，兼顾桌面与手机阅读。完成后对实际读回的各页进行验收。",
  pages: [
    {path: "agent-01-overview.html", title: "设计出发点", focus: "朴素循环为何最稳、上下文有限带来的问题，以及用函数作用域代替压缩"},
    {path: "agent-02-program.html", title: "流程、虚函数与模块", focus: "流程代码就是 JavaScript、虚函数是 AI 钩子、模块是 AI 写成并复用的代码，以及主程序为什么只用模块"},
    {path: "agent-03-scope.html", title: "作用域与上下文", focus: "参数显式传入、返回值是跨作用域的唯一通道、上下文超限时抛错而不压缩"},
    {path: "agent-04-libraries.html", title: "工具库与并发", focus: "工具即库并通过 import 引入、虚函数的工具范围与运行时查找、Promise 取代子 Agent"},
    {path: "agent-05-acceptance.html", title: "错误与交付", focus: "错误统一走异常、模块出错交给修复者、副作用边界、已废弃机制，以及真实证据与完成条件"}
  ]
};

const source = io.read({path: request.source});
const shared = {
  source_path: request.source,
  source_content: source.content,
  requirements: request.requirements
};

const designs = await Promise.all(request.pages.map(page => designPage(shared, page)));
for (let i = 0; i < request.pages.length; i++) {
  io.write({path: request.pages[i].path, content: designs[i].content});
}
const review = async page => {
  const actual = io.read({path: page.path});
  return await inspectPage(shared, page, actual.content);
};
const reviews = await Promise.all(request.pages.map(review));
// 未通过的页按验收意见修正一轮，写入后重新读回复检。
const revised = await Promise.all(request.pages.map(async (page, i) => {
  if (reviews[i].passed) return reviews[i];
  const current = io.read({path: page.path});
  const fixed = await revisePage(shared, page, current.content, reviews[i].issues);
  io.write({path: page.path, content: fixed.content});
  return await review(page);
}));
for (let i = 0; i < request.pages.length; i++) {
  assert(revised[i].passed, "修正后验收仍未通过：" + request.pages[i].path);
}

/** @module */
async function designPage(shared, page) {
  // shared 含实际源材料与统一要求，page 含标题、重点和输出路径。
  // 设计本页，逐项覆盖 page.focus 中的要求并依据实际源材料说明；
  // 区分现行机制和历史记录，不擅自推断源材料的版本或日期。
  // 主题不同于其他页，HTML/CSS 自包含。
  // 返回 {content:string}；不写文件，不重复读取已传入的材料。
}

/** @module */
async function inspectPage(shared, page, content) {
  // 对照实际源材料和本页要求检查真实读回正文。
  // 只验收本页；五页之间是否不同、是否协调不在本函数职责内，不因此判为不通过。
  // 只有与源材料不符、遗漏本页要求等实质问题判为不通过；措辞上的建议可写进 issues，但不影响 passed。
  // 返回 {passed:boolean, path:string, issues:[string], evidence:[string]}。
  // 不虚构未执行的浏览器或视觉检查。
}

/** @module */
async function revisePage(shared, page, content, issues) {
  // content 是本页已保存的真实正文，issues 是验收指出的问题。
  // 依据 shared 中的源材料逐条修正这些问题，保留本页其余正确内容和风格。
  // 返回 {content:string}；不写文件。
}
```

### 用模块处理几份文档

源码：[module-notes.js](examples/module-notes.js)。几份文档的要点都由同一个模块 summarizeFile 完成：第一次调用时由 AI 按契约写成代码，之后对同一契约的调用直接运行这份代码；几个调用同时第一次运行时，只由一个请 AI 写，其余的等它写完。模块需要的东西，包括用户的要求，都作为参数传进去。

```js
import * as io from "host:io";
// 同一个模块处理几份文档：第一次调用时由 AI 写成代码，之后的调用直接运行这份代码。
const request = {
  files: ["README.md", "docs/agent-design-guide.md", "docs/agent-program.md"],
  output: "notes.md",
  requirements: "为每份文档写三到五条中文要点，每条一句话，忠实于原文，不加原文没有的内容。"
};

const notes = await Promise.all(request.files.map(path => summarizeFile(path, request.requirements)));
const text = notes.map(note => "## " + note.path + "\n" + note.points.map(point => "- " + point).join("\n")).join("\n\n");
io.write({path: request.output, content: text});
const actual = io.read({path: request.output});
assert(actual.content === text, "要点没有完整写入");

/** @module */
async function summarizeFile(path, requirements) {
  // 读取 path 的实际内容，按 requirements 写出要点，并核对条数符合要求。
  // 返回 {path:string, points:[string]}；不写文件。
}
```

[二十页示例](examples/parallel-twenty-html.js)先设计共享样式，再并发生成每页正文，统一写入、读回和验收。示例中的页数、格式和检查标准仅适用于该业务；未执行浏览器或视觉检查时不能声称已完成。

### Shell 只在模块的确定性实现中运行

源码：[主流程](examples/shell-in-module.js)与[模块实现](examples/shell-in-module-implementation.js)。主流程只声明和调用模块，不直接 import process 或调用 shell。模块的真实实现运行 Node 读取 package.json，先检查 running 和 exit_code，再 JSON.parse；它只返回包名和脚本数量等小对象，由主流程决定打印。命令在 Windows PowerShell 7 与 Linux Bash 使用相同写法，需要已安装 Node；不调用任何虚函数。

```js
// 主流程只编排：shell 留在模块的确定性实现中。
const summary = inspectPackage(".");
console.log(JSON.stringify(summary));

/** @module */
function inspectPackage(workdir) {
  // 用 Node 读取 workdir/package.json 的实际 JSON；检查进程结束、退出码和输出完整性。
  // 返回 {name:string, scriptCount:number, scriptNames:[string]}，列出实际脚本名称。
  // 不修改文件；不把完整 stdout/stderr 打印或传给 AI。
}
```

模块内部取得的完整输出与返回值留在 JS heap；本例只打印契约要求的统计结果。若短步骤超过等待时间，本例停止自己的 session 并失败；长服务场景则在模块代码中安排轮询和终止。容量耗尽时明确失败，不解析部分输出、不伪造摘要，完整资产或分页用于后续恢复。

## 固定写法

- 顶层语句按顺序覆盖用户要求的工作、验证和交付，像 JS 模块一样从上到下执行，没有入口函数，也不需要返回值。程序需要的数据写在代码里，或用导入的工具读取。function 定义辅助任务，helper(args) 调用。
- 内部值保存在 VM 堆中，对象和数组在普通函数调用间保留引用；宿主工具和 AI 参数／结果使用 JSON。支持 const/let/var、局部赋值及赋值表达式、对象／下标赋值及复合赋值、更新、++/--、+= -= *= /= %=、对象简写、简单数组解构、成员／下标读取、模板字符串。
- 控制流支持 if/else、传统 for、while、do/while、数组 for...of、简单绑定的 for...in、switch、带标签 break/continue、三元、assert、return、try/catch/finally/throw。已知循环写进代码，不交给一个宽泛注释任务反复调度。
- + - * / %、比较、逻辑 && || ?? !、模板转换由宿主确定性计算，普通表达式绝不请求 AI。条件中空字符串、false、null、undefined、0 和 NaN 为假，数组和对象为真。&& 和 || 返回所选操作数；短路和三元只执行选中的分支，副作用按从左到右的顺序执行一次。基本数字、布尔与字符串支持部分转换，对象到原始值的转换仍有限制。
- 数字采用 IEEE-754 双精度，超过安全整数范围会像 JS Number 一样舍入；除零产生非有限数，但非有限数不能直接越过宿主 JSON 边界。=== 对对象比较引用身份；基本原始值的 == 支持部分类型转换。复杂对象转换和完整 JS 数值语义仍未覆盖。
- 宿主自带的工具：从 `host:<library>` 导入同步版本，从 `host:<library>/promises` 导入异步版本；也可以像 Node 里 `require("fs")` 那样写 `const io = require("io")`。没有 import 的程序里，宿主自带的库按名字就能用，自己声明了同名变量时，这个名字归程序自己。插件和项目技能的库是第三方库，在用到的地方写 `const lib = require("<库名>")`，异步版本是 `require("<库名>/promises")`：执行到这一行才加载，同一次运行里再 require 拿到同一个对象，找不到就在这一行抛出可以捕获的 MODULE_NOT_FOUND。所有调用仍经过权限与工具契约检查。
- 本地数组 push 收集结果；支持确定性方法 includes/indexOf/slice/at/concat、字符串 lastIndexOf/startsWith/endsWith/trim/toLowerCase/toUpperCase/split/replace/replaceAll/match、正则 test、数组 join、JSON.stringify/parse、Array.isArray、Object.keys/values、typeof。Object.keys/values 仅接受对象，整数键按索引排序，其余键按插入顺序；宿主 JSON 输入的键顺序可能已经变化。replace/replaceAll 接受字符串或正则查找（replaceAll 的正则须带 g），替换文本按 JS 展开 $&、$1、$<name> 等，替换函数见下条；未知方法编译失败，不能假定会让 AI 解释。
- 声明、普通函数参数和 catch 支持解构及默认值；对象与数组字面量支持展开，可使用可选函数调用。支持函数剩余参数与调用参数展开。非简单参数在独立参数作用域初始化，默认值与其中的闭包不读取函数体局部绑定。
- 函数和闭包可保存、传递和调用。数组 map 支持 async 回调；其他数组回调目前要求同步。普通函数按 JS 处理实参数量；虚函数和模块严格核对参数。
- 路径与 URL 可直接计算：`path.join/resolve/relative/normalize/dirname/basename/extname` 和 `url.parse(value, base)`；结果用于普通流程，不调用 AI。resolve 至少一个绝对路径、relative 的两个参数必须绝对；url.parse 返回 href/origin/protocol/hostname/port/pathname/search/hash。它们由宿主 Rust 实现，处理路径不访问文件系统；文件访问仍使用 io。
- 支持正则字面量及 `/pattern/i.test(text)`、`text.match(/pattern/g)`；只有 g/i/m/s/u 标志，Rust regex 语法不支持环视和回溯引用。test 无状态；match 未命中返回 null，非 g 返回完整匹配和捕获组数组，g 返回完整匹配数组。模式最多 8192 字节，输入最多 2 MiB，结果最多 10000 项、捕获文本最多 4 MiB；超出时报错。不是完整 JavaScript RegExp，支持 new RegExp 和 replace/replaceAll；不支持 lastIndex。
- async 函数返回 Promise，await 挂起并在结果到达后恢复。`Promise.all`、`allSettled`、`race`、`any`、`then`、`catch`、`finally` 可组合结果；只读异步工具最多 32 项一批。async 的虚函数和模块可并发，模型请求共享并发槽位。Promise 按 JS 微任务顺序执行，用 Promise 结算另一个 Promise 时同样多等两轮。对顶层 async 函数或 /promises 工具的调用不 await 就直接取属性、参与运算、作为条件、传给 assert 或放进模板字符串，会在编译时报错；需要先保存 Promise 或 await。
- 这是受限 JS 环境：await 支持 thenable 同化；Promise.resolve/构造器 resolve 的一般 thenable 同化仍不完整。call()/ai() 包装、除 host: 库之外的 import（第三方库用 require）、export、TypeScript 类型和 BigInt 也还不支持。同步 function*/yield/yield*、class/extends/super/new.target 和 this 已支持；生成器执行状态随 Runtime 存盘恢复，JSON 不保存执行状态。

let/const 属于所在的块，内层块可以重复声明同名变量并遮蔽外层，声明前读取会抛出 ReferenceError；var 属于所在函数并提升，首次无初值为 undefined。函数只看自己的变量和顶层变量，调用方的局部变量作为参数传入。普通函数中的对象别名共享修改；AI 作用域和宿主工具以 JSON 交接。字符串 length、索引、slice 和 indexOf 使用 Unicode 标量。模板只插入原始值，对象和数组使用 JSON.stringify。完整规范在 docs/agent-program.md。

## 工具库

`program_tool_headers` 是按用途分组的库目录。读代码时，在相关目录用 `io.search` 定位定义，再用 `io.read_lines` 查看指定行。用 `read_tool_contracts({libraries:["io"],overview:true})` 查看库里的方法和用途。选定方法后只传 `methods`，例如 `read_tool_contracts({methods:["io.read_lines"]})`，读取准确参数、约束和返回字段。第三方库也可以按库名查。宿主自带的库按能力分开：io 是本地文件，process 是运行程序和项目脚本，browser 是浏览器页面，另有 http、memory、vision、attachments、session 等，import 什么就是要什么能力；插件有自己的库名，项目技能的库名就是技能名；只公布本作用域可用的工具。import、require 和读取契约都不扩大权限。

shell 三方法 `process.exec_command`、`process.write_stdin`、`process.kill_session` 只允许在模块的确定性实现代码中调用。主 AI、主程序、`@virtual` 均不能直接 shell；需要 shell 的主流程声明 `@module`，模块内写实际运行、轮询、解析和检查代码，虚函数只接必要材料做小型 AI 判断。旧方法保留，不用于绕过此边界。shell 路径和能力以 system 动态注入为准：Windows 用 PowerShell 7，Linux 用 Bash；每条命令是新进程，workdir 每次传，变量和 cd 不保留；长进程仍用 session，管道，无 PTY。同 task、同 turn 的 shell 可共存、共享 `change_interval`，最后退出才结算；别的 task/turn 和宿主文件编辑、写入、undo 等待全部 shell 停止。

模块收到实际 stdout/stderr 与退出码，结果先进入 JS heap，默认两路输出合计 8 MiB；检查 `running`、`exit_code` 后才能解析。完整输出超出堆交付预算时以 `SHELL_OUTPUT_OVER_CAPACITY` 停止，不交付成功前缀，保留完整 asset 和会话读取引用。模块可用 `process.write_stdin({session_id,chars:"",stream:"stdout",char_offset:0,max_output_tokens})` 读取 `output`，跟随 `next_char_offset` 分页处理，stderr 单独读取；空 chars 轮询，非空发送 stdin，kill_session 终止本会话进程树。分页不重跑命令，也不要盲目拼回超预算字符串。模块返回值先回调用方 JS heap；AI 只接收打印、诊断、显式虚函数输入和成果摘要，不会自动看到整个模块返回值或原始 shell 输出。主流程编排和模块实现继续用具体 JS。定位代码优先 `rg --files frontend/src`、`rg -n "关键词" frontend/src`，默认遵守 `.gitignore`；不要递归 Get-ChildItem 扫 target、node_modules。详见[手册](../../docs/agent-program.md#shell-stage-1process-库)。

虚函数以工具循环工作：它能用的工具是声明它的模块代码 import 和 require 的当前允许方法，排除 shell，缺少时可以用 find_library_methods 按关键词查找。所以模块代码里引入的，也是对这些虚函数需要哪些能力的预期。

技能的说明是库的文档，谁引用谁才有。写代码前用 read_skill 读要用的技能；读过的说明只在你自己这里，不会带进你调用的模块和虚函数。模块要按某个技能做事，由写它的 AI 在模块的函数里 require 那个库；要让模块里的一个虚函数按这个技能做事，就把虚函数声明在这个函数里，用 `@uses` 点名库的变量。只有说明、没有方法的技能也这样 require，得到的对象没有方法。比如模块 process 的代码：

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

## AI 工作：模块和虚函数

需要 AI 的地方有两种写法，主程序只用其中的模块：

- `@module` 模块：一块完整的工作，第一次调用时由另一个 AI 按契约写成代码，之后的调用直接运行这份代码，见下面的“模块怎样被写出来”。主程序里需要 AI 的地方都写成模块：主程序负责编排，模块的代码负责把这一块读清、拆小、检查。
- `@virtual` 虚函数：AI 在运行时用工具循环完成一件事，返回一个 JSON 值，写在模块的代码里。它只做一件专一的事时最稳；多步的流程，比如先跑测试再修改，写成代码，一步步调用虚函数。它要看的材料，最好由代码读好作为参数传进去，不让它自己去找。第一层写出的虚函数往往太粗，所以主程序里声明虚函数，宿主在交出时就拒绝。

在注释里写清职责、输入和返回字段；调用它就是交给 AI 的一次工作。标记写在紧挨函数的前置 JSDoc 里，或写在只有注释的函数体里；函数体保持为空，或只写 throw new Error("固定说明") 作为占位。两种函数都可以像普通 JS 一样嵌套声明在另一个函数里，一个函数只标其中一种。普通函数里的注释只是说明，不会执行；普通代码直接 return 的数据就是返回值本身，不会替你制作或检查产物。没有标记的空函数会在编译时被拒绝。

子任务完成就正常返回，做不到就向调用方抛出具体错误；正常返回的数据按任务契约自由定义，没有统一的状态包装。调用方不要检查契约没有声明的 `status`、`saved` 等字段；返回 `null` 也可能是正常完成。正常返回只说明子任务完成，不代替对实际产物或业务判断的必要验证。外部操作结果不确定时，应报告不确定并先核实，不要当成确定失败而重复执行。

按常见的软件设计原则划分函数：单一职责、高内聚，针对契约编程，通过组合形成流程。复杂或无法直接写成代码的能力交给 AI，每个虚函数尽量只承担一项明确的工作；普通函数负责组织调用、传递数据和处理结果。按实际任务选择粒度，不为形式拆分。

验收要和实际检查相称。字符串、数量或存在性检查是有用的局部证据，但不足以说明内容正确或整个任务通过；内容理解交给职责清楚的虚函数。每项结论对应实际检查及其证据；没检查或无法确认的要求如实报告。遇到编译限制或缺少工具时，换一种可行的做法，或者如实返回未完成及原因，而不是删掉必要检查让流程看起来成功。

```js
import * as io from "host:io";
const request = {output: "report.html", requirements: "按用户要求制作并验收报告"};
io.write({path: request.output, content: produce(request).content});
let check = inspect(request, io.read({path: request.output}).content);
if (!check.passed) {
  const current = io.read({path: request.output}).content;
  io.write({path: request.output, content: repair(request, current, check.issues).content});
  check = inspect(request, io.read({path: request.output}).content);
}
assert(check.passed, "修复后验收仍未通过，停止交付");
/** @module */
function produce(request) {
  // 按 request 制作报告正文，返回 {content:string}；不写文件。
}
/** @module */
function inspect(request, content) {
  // content 是已保存报告的实际正文；对照 request 判断，返回 {passed:boolean, evidence:[string], issues:[string]}。
}
/** @module */
function repair(request, content, issues) {
  // 依据 issues 修正 content 中授权范围内的问题，返回 {content:string}；不写文件。
}
```

修复后再 inspect 一次；复检仍失败时由 assert 停止，不继续交付。这个例子只修一次，不循环重试。实际任务允许修复时，可以照这样为验收发现的问题留出修正分支：把失败证据、当前内容和要求交给一个修正函数，再读回实际结果复检；处理的是当前问题，不重启整条流程，也不重放已完成的操作。三个模块各做一件事，写入、读回和要不要再修由主程序决定。

已经取得的实际值，直接作为参数传给需要它的虚函数，不必进入函数后再读一遍；要核实外部状态或修改后的结果时再读回。一个虚函数的职责是只检查或只处理；检查之后要处理的，由代码看了结果再调用处理的函数。函数产生哪些副作用由它的职责决定：只负责生成并返回内容时，保存留给调用方；承担修改职责时才写入。契约里约定了返回值与输入之间可由代码确定的关系时，调用方在使用返回值前核对这个关系。

工作区里的文件是所有函数共享的状态，就像对象的字段。改代码这类工作，虚函数直接用工具改好文件就完成了，契约可以不返回内容（它回复 `null`）；下一步要用改后的内容就去读文件，检查的虚函数自己读，或者由代码读好传进去。一个文件在一步里只有一个写入者：要么虚函数直接改、不返回内容，要么它只返回内容、自己不写，由代码写入。不要两样都做：虚函数改完了文件，契约又要它返回文件内容，它只能拿修改说明凑数，代码再把说明写回去，源码就被覆盖了。判断改得对不对，读实际的文件、跑实际的测试，不把上一个函数的说明当事实。并发的函数不写同一个文件。

```js
import * as io from "host:io";
import * as process from "host:process";
const requirements = "……用户的原话……";
const target = "mini-webpack.js";
const tests = "test/bundler.test.js";
let verdict = {ok: false, problems: []};
for (let round = 0; round < 3 && !verdict.ok; round++) {
  await modify(requirements, target, tests, verdict.problems);
  verdict = await review(requirements, target, tests);
}
if (!verdict.ok) throw new Error("验收未通过：" + verdict.problems.join("；"));
/**
 * @module
 * 按 requirements（用户的原话）修改 target 和 tests，problems 是上一轮验收发现的问题；直接改文件，不返回内容。
 */
async function modify(requirements, target, tests, problems) {}
/**
 * @module
 * 读取 target 和 tests 的当前内容并运行测试，对照 requirements（用户的原话）判断；只读，不修改。
 * 返回 {ok:boolean, problems:string[]}。
 */
async function review(requirements, target, tests) {}
```

modify 和 review 各做一件事：modify 改文件、不返回内容，review 读文件、跑测试、只返回判断；改几轮、什么时候停，由主程序决定。target、tests 这样的路径来自熟悉项目时看到的事实，写进程序里；用户的原话作为参数传进去，模块不用 `@uses`。review 的代码里也拿得到写文件的工具，所以契约里写明只读。

## 模块怎样被写出来

模块第一次被调用时，宿主把它的签名、契约注释和这次调用的实参交给一个 AI，由它调用 write_program 交出这个函数的完整实现、需要的辅助函数和 import；除 import 外没有顶层语句，所以要 require 的库，宿主自带的或第三方的，都在用它的函数里 require。写模块的 AI 只有契约和实参，调用方读过的技能不会带过来；它要用哪个技能，自己用 read_skill 读。这份代码存成这个聊天的模块文件，之后对同一契约（函数名、签名和注释原文完全一样）的调用都直接运行它，所以要写成通用代码：实参只是一个例子，不能围绕它把结果写死。代码里需要 AI 的部分声明成虚函数，它们的工具范围就是这段代码 import 和 require 的方法；完整的子流程也可以再声明成模块。这个聊天已有哪些模块，见请求里的 chat_modules；写出同样的声明就会复用。

例如模块 process(shared, part) 的代码可以是：

```js
import * as io from "host:io";
function process(shared, part) {
  const source = io.read({path: part.source});
  const changed = transform(shared, part, source.content);
  const saved = io.write({path: part.output, content: changed.content});
  return {path: part.output, receipt: saved};
}
/** @virtual */
function transform(shared, part, content) {
  // 根据 shared、part 处理 content，返回 {content:string}。
}
```

形参与实参按位置绑定，不会自动把多个参数打包成一个对象；program_task_input.bindings 列出的就是各形参的实际值。声明为 async 的模块，代码也写成 async。

## 作用域与生命周期

虚函数只拿到自己的签名、契约、实际参数，以及注释里用 JSDoc 标签 `@uses <变量名> <说明>` 点名的外层变量：像闭包一样按声明处解析，在调用的那一刻读取；点名的是 require 得到的库时，它带上的是这个库的说明（见“工具库”）。它看不到调用方的对话、其他变量和读过的技能。主程序把用户的要求原样作为参数传给模块，比转述可靠。模块只通过参数拿输入，在模块上写 `@uses` 会编译报错；模块里声明的虚函数可以用 `@uses` 点名模块自己的变量，把虚函数声明在模块的函数里，就能点名它的参数。async 函数拿到的是调用时的参数值，之后调用方的变化不会同步过去。虚函数返回后，它的 AI 工作上下文释放，返回值交给调用方；模块执行与返回使用 JS heap，不能把 JS 堆值统称为 AI 上下文。

上下文不会被压缩。单个结果太大、放不进虚函数剩下的上下文时不算超限：结果不交付，虚函数收到它有多大，换一个更窄的调用。虚函数自己的上下文放不下下一次请求时，宿主以 CONTEXT_OVERFLOW 错误结束它，错误里带上已完成的写入；调用它的代码可以把工作拆小后重新调用，或继续上抛。

## 数据流与交付

共同依赖与各自职责分开传参：共享规则传完整对象，资产传真实路径、版本及必要正文。嵌套调用继续传递所需依赖，并发分支各自拿到自己的那份工作。正文、HTML 等长数据由函数返回或保存为文件，编排代码里不展开巨型字符串。

返回值要能被下一步直接使用。工具成功、函数返回或编译通过都不等于业务验收；按业务技能完成修复、收集异步结果和确认点后再交付。库方法返回工具的原始 JSON，例如 io.read 的 result.content。

## 源码检查和错误处理

程序和模块代码都在任何实际操作前整份编译；编译失败时什么都不执行，带行列和原因的诊断交回写代码的 AI，由它改正后重新提交。

模块运行时出错且没有被捕获时，宿主交给一个修复者：它只拿到模块文件和这次的问题，即错误和停止位置、这次调用的实参、打印的输出和已完成的写入，以及模块 require 的库的说明，用 edit_file 改模块文件；改好后宿主存成新版本，从头重跑这次调用，之后的调用都用新版本。修不了时它回复 {"$task_error":{"message":"具体原因"}}，错误才抛给调用方。虚函数做不到时同样回复 $task_error，这是正常的信号：调用它的代码可以捕获，把活拆细或换一种做法。主程序里没有捕获的错误交回主 AI，由它改正后重写程序。出错的程序（主程序或模块的一次调用）还在跑的调用，在各自下一步之前停下，不再交付结果；重写或重跑的程序从头开始。结果未知的外部写入不会自动重放。权限和插件边界始终有效。

## 并发与工作区

并发的虚函数和模块在同一工作区运行，各自只拿到调用时的参数。独立任务分别处理各自的文件，或者返回内容交给调用方统一写入。并发写同一文件发生冲突时，宿主保留现有文件和待写副本并停止，不自动合并或重放。


### 模块结果与完整证据

模块按调用方要拿到的结果定义，例如核实过的事实、统计或附文件行号的结论；复用时检查返回结果是否适合当前任务，不能只看它能否执行命令。已知步骤继续写成普通 JavaScript；模块内收集、处理材料，需要 AI 判断时才调用职责窄的虚函数，不强制增加 AI 层。代码分析交给 AI 的证据应包含相关完整单元、外层条件和提前返回。搜索命中只能证明命中的一行。一个适量的完整单元用一次打印，并保留文件和行范围；不要按任意字符位置截掉尾部。宿主实际交付不下时明确报错并保存原结果，AI 根据读取引用分段查看或调整编排，保留已完成操作；未读内容不能作为结论依据。行为结论要核对完整控制路径或实际执行顺序，区分读过测试与运行过测试。

日志不再按单条 100,000 字符、200 条记录或最后 20 条静默裁剪。完整记录先保存在运行时；累计日志捕获超过堆预算时以 `OUTPUT_OVER_CAPACITY` 停止，拒收的完整记录也归档。主程序结局和模块探路结果在进入 AI 前按实际剩余上下文检查，放不下就明确失败，不交付成功前缀，原始输出、返回值和错误保存为 asset 并提供读取引用。资源错误由调用方调整编排，不作为普通实现错误自动修复重跑；已完成命令或写入不能盲目重放。只有 AI 下一次请求本身放不下时才是 `ContextOverflow`。显式按范围分页读取仍可使用，不把未请求的其余内容冒充已交付。


异步迭代同样是普通流程代码：可写 async function* 和 for await...of。next/return/throw 返回 Promise，等待时后续请求排队；请求队列、finally 待完成值和宿主调用中的帧随 Runtime 保存。for-await 优先异步协议，否则回退同步协议并等待其 value；自定义异步协议的 value 原样保留。用它们逐批处理数据，仍按要交付的结果定义模块，在代码中收集和提炼材料后再交给 AI。
