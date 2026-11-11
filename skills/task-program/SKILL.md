---
name: task-program
description: 将用户目标与业务 skill 表达为可执行的流程程序：顶层执行、虚函数（第一层生成并复用实现，第二层 AI 判断）、工具调用、确定性表达式和基础控制流。
---

# 用程序表达任务

用 JavaScript 描述工作流程：步骤如何调用、依赖什么、数据如何传递。主编排和生成的模块实现都运行在宿主的可恢复 Rust VM 中，使用已支持的 JS 内置能力和宿主发布的库。主层把尚未实现的能力声明成带 JSDoc @virtual 的空函数，简要说明职责、参数和返回值，不在编排时实现这些方法，也不指定实现文件位置。宿主负责链接或生成实现；第一层实现中的 AI 判断也使用 @virtual。已知的编排和具体计算用普通函数，不必标成虚函数。按实际任务决定拆分和并发，不预设扫描框架、AST 或问题数量。被分析项目的技术栈由第一层实现检查真实输入后确定。业务范围和验收来自用户与业务 skill。

## 从完整示例学习

编排代码使用 JavaScript 语法，外部工具通过库方法提供（宿主 SDK 用 `import io from "io"`、命名导入、命名空间导入或 `require("io")`，旧 `host:` 前缀对 import 和 require 都是兼容别名；插件和项目技能的在用到的地方 `require`），两层 AI 能力都以 JSDoc @virtual 声明，宿主按实际调用层级选择代码实现或 AI 判断。这里的 import/require 只解析宿主库，不加载 Node 内置模块、npm 包或工作区 node_modules；VM 没有 Node 的全局 process 对象或 Buffer。确需 Node/npm 时，由第一层实现通过已发布的 process SDK 运行独立工作区脚本，检查实际结果后把数据返回流程。下面三份完整程序供查阅；宿主不会在每次规划时自动注入本 Skill 或示例。完整说明见 [语言手册](../../docs/agent-program.md)，包括运行方式、逐阶段数据流、失败处理和尚未支持的 JS 功能。示例只是教学，不把 HTML、页数或分工规定为其他业务的流程。

主程序负责编排结果和数据依赖，第一层虚函数的实现负责采集、命令、处理、检查；第二层虚函数只做需要 AI 判断或生成的聚焦工作。第一层输入显式传参，第二层可以用 @uses 捕获词法变量。第二层不提交程序、不再创建 AI 层，越层报 VIRTUAL_LAYER_LIMIT；不再创建第三层 AI 工作。已有实现由宿主按契约复用。

### 最小串行程序

源码：[serial.js](examples/serial.js)。调用 write_program 交出程序（这一次回复里只调用它），宿主把它写成这个聊天文件夹里的文件，再从文件读出运行；回复正文里的代码不会运行。用户请求中的数据直接写进代码。普通函数调用顺序执行。compose 返回正文，工具保存并读回，inspect 对实际正文检查，断言通过才交付。两个模块各做一件事：一个写，一个查；保存和读回由主程序完成。模块的代码在第一次调用时由 AI 写出，需要 AI 生成或判断的那一小步，在模块里声明虚函数。

```js
import * as io from "io";
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

/** @virtual */
function compose(request) {
  // 按 request.topic 和 request.requirements 设计一个完整、独立的中文 HTML。
  // 返回设计好的 {content:string}，不只返回计划。
}

/** @virtual */
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
import * as io from "io";
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

/** @virtual */
async function designPage(shared, page) {
  // shared 含实际源材料与统一要求，page 含标题、重点和输出路径。
  // 设计本页，逐项覆盖 page.focus 中的要求并依据实际源材料说明；
  // 区分现行机制和历史记录，不擅自推断源材料的版本或日期。
  // 主题不同于其他页，HTML/CSS 自包含。
  // 返回 {content:string}；不写文件，不重复读取已传入的材料。
}

/** @virtual */
async function inspectPage(shared, page, content) {
  // 对照实际源材料和本页要求检查真实读回正文。
  // 只验收本页；五页之间是否不同、是否协调不在本函数职责内，不因此判为不通过。
  // 只有与源材料不符、遗漏本页要求等实质问题判为不通过；措辞上的建议可写进 issues，但不影响 passed。
  // 返回 {passed:boolean, path:string, issues:[string], evidence:[string]}。
  // 不虚构未执行的浏览器或视觉检查。
}

/** @virtual */
async function revisePage(shared, page, content, issues) {
  // content 是本页已保存的真实正文，issues 是验收指出的问题。
  // 依据 shared 中的源材料逐条修正这些问题，保留本页其余正确内容和风格。
  // 返回 {content:string}；不写文件。
}
```

### 用模块处理几份文档

源码：[module-notes.js](examples/module-notes.js)。几份文档的要点都由同一个模块 summarizeFile 完成：第一次调用时由 AI 按契约写成代码，之后对同一契约的调用直接运行这份代码；几个调用同时第一次运行时，只由一个请 AI 写，其余的等它写完。模块需要的东西，包括用户的要求，都作为参数传进去。

```js
import * as io from "io";
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

/** @virtual */
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

/** @virtual */
function inspectPackage(workdir) {
  // 用 Node 读取 workdir/package.json 的实际 JSON；检查进程结束、退出码和输出完整性。
  // 返回 {name:string, scriptCount:number, scriptNames:[string]}，列出实际脚本名称。
  // 不修改文件；不把完整 stdout/stderr 打印或传给 AI。
}
```

模块内部取得的完整输出与返回值留在 JS heap；本例只打印契约要求的统计结果。若短步骤超过等待时间，本例停止自己的 session 并失败；长服务场景则在模块代码中安排轮询和终止。容量耗尽时明确失败，不解析部分输出、不伪造摘要，完整资产或分页用于后续恢复。

## 固定写法

- 顶层语句按顺序覆盖用户要求的工作、验证和交付，像 JS 模块一样从上到下执行，没有入口函数，也不需要返回值。程序需要的数据写在代码里，或用导入的工具读取。function 定义辅助任务，helper(args) 调用。
- 语言就是标准的现代 JavaScript，按严格模块语义执行：变量与解构、if/else 和各种循环、闭包、class、function*/yield、async/await、for await、Promise.all 等组合方法、Map/Set、Symbol、正则都和标准一致。下面只列宿主特有的部分，与标准的差异和尚不可用的功能用 read_program_guide 查。
- 表达式由宿主确定性计算，绝不请求 AI。已知循环写进代码，不交给一个宽泛注释任务反复调度。普通函数按 JS 处理实参数量；虚函数和模块严格核对参数。
- 宿主工具和 AI 的参数、结果使用 JSON：Map/Set 要先 Array.from，循环引用和非有限数会被拒绝。程序内部的对象保留引用。
- 宿主自带的工具：从 `host:<library>` 导入同步版本，从 `host:<library>/promises` 导入异步版本；也可以像 Node 里 `require("fs")` 那样写 `const io = require("io")`。没有 import 的程序里，宿主自带的库按名字就能用，自己声明了同名变量时，这个名字归程序自己。插件和项目技能的库是第三方库，在用到的地方写 `const lib = require("<库名>")`，异步版本是 `require("<库名>/promises")`：执行到这一行才加载，同一次运行里再 require 拿到同一个对象，找不到就在这一行抛出可以捕获的 MODULE_NOT_FOUND。所有调用仍经过权限与工具契约检查。
- 路径与 URL 可直接计算：`path.join/resolve/relative/normalize/dirname/basename/extname` 和 `url.parse(value, base)`；结果用于普通流程，不调用 AI。resolve 至少一个绝对路径、relative 的两个参数必须绝对；url.parse 返回 href/origin/protocol/hostname/port/pathname/search/hash。它们由宿主 Rust 实现，处理路径不访问文件系统；文件访问仍使用 io。
- 对顶层 async 函数或 /promises 工具的调用不 await 就直接取属性、参与运算、作为条件、传给 assert 或放进模板字符串，会在编译时报错；需要先保存 Promise 或 await。只读异步工具最多 32 项一批，async 的虚函数和模块可并发，模型请求共享并发槽位。
- 没有 call()/ai() 包装，需要 AI 就声明模块或虚函数；除 host: 库之外的 import（第三方库用 require）和 export 也不支持。

函数只看自己的变量和顶层变量，调用方的局部变量作为参数传入。字符串 length、索引、slice 和 indexOf 按 Unicode 标量计数，不是 UTF-16 单元。完整手册在 docs/agent-program.md。

## 工具库

`program_tool_headers` 是按用途分组的库目录。读代码时，在相关目录用 `io.search` 定位定义，再用 `io.read_lines` 查看指定行。用 `read_tool_contracts({libraries:["io"],overview:true})` 查看库里的方法和用途。选定方法后只传 `methods`，例如 `read_tool_contracts({methods:["io.read_lines"]})`，读取准确参数、约束和返回字段。第三方库也可以按库名查。宿主自带的库按能力分开：io 是本地文件，process 是运行程序和项目脚本，browser 是浏览器页面，另有 http、memory、vision、attachments、session 等，import 什么就是要什么能力；插件有自己的库名，项目技能的库名就是技能名；只公布本作用域可用的工具。import、require 和读取契约都不扩大权限。

shell 三方法 `process.exec_command`、`process.write_stdin`、`process.kill_session` 只允许在模块的确定性实现代码中调用。主 AI、主程序和第二层 `@virtual` 均不能直接 shell；需要 shell 的主流程声明第一层 `@virtual`，模块内写实际运行、轮询、解析和检查代码，虚函数只接必要材料做小型 AI 判断。旧方法保留，不用于绕过此边界。shell 路径和能力以 system 动态注入为准：Windows 用 PowerShell 7，Linux 用 Bash；每条命令是新进程，workdir 每次传，变量和 cd 不保留；长进程仍用 session，管道，无 PTY。同 task、同 turn 的 shell 可共存、共享 `change_interval`，最后退出才结算；别的 task/turn 和宿主文件编辑、写入、undo 等待全部 shell 停止。

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

两层 AI 工作统一声明为带 JSDoc `@virtual` 的空函数，注释说明职责、参数和返回字段。宿主按实际调用层级决定行为：

- 主编排里的第一层虚函数：首次调用时生成实现代码，按契约保存为模块；后续调用复用实现，无须导入实现文件。
- 实现里的第二层虚函数：使用证据和授权工具完成聚焦的理解、判断或生成，返回约定的 JSON；不再提交代码或创建 AI 工作。

标记放在紧邻函数的前置 JSDoc 中。普通 helper 写真实函数体；未标记的空函数是普通 JavaScript，返回 undefined。第一层输入显式传参，第二层可用 `@uses` 点名词法变量。旧模块标记兼容保留，新代码统一使用 `@virtual`。

子任务完成就正常返回，做不到就向调用方抛出具体错误；正常返回的数据按任务契约自由定义，没有统一的状态包装。调用方不要检查契约没有声明的 `status`、`saved` 等字段；返回 `null` 也可能是正常完成。正常返回只说明子任务完成，不代替对实际产物或业务判断的必要验证。外部操作结果不确定时，应报告不确定并先核实，不要当成确定失败而重复执行。

按常见的软件设计原则划分函数：单一职责、高内聚，针对契约编程，通过组合形成流程。复杂或无法直接写成代码的能力交给 AI，每个虚函数尽量只承担一项明确的工作；普通函数负责组织调用、传递数据和处理结果。按实际任务选择粒度，不为形式拆分。

验收要和实际检查相称。字符串、数量或存在性检查是有用的局部证据，但不足以说明内容正确或整个任务通过；内容理解交给职责清楚的虚函数。每项结论对应实际检查及其证据；没检查或无法确认的要求如实报告。遇到编译限制或缺少工具时，换一种可行的做法，或者如实返回未完成及原因，而不是删掉必要检查让流程看起来成功。

```js
import * as io from "io";
const request = {output: "report.html", requirements: "按用户要求制作并验收报告"};
io.write({path: request.output, content: produce(request).content});
let check = inspect(request, io.read({path: request.output}).content);
if (!check.passed) {
  const current = io.read({path: request.output}).content;
  io.write({path: request.output, content: repair(request, current, check.issues).content});
  check = inspect(request, io.read({path: request.output}).content);
}
assert(check.passed, "修复后验收仍未通过，停止交付");
/** @virtual */
function produce(request) {
  // 按 request 制作报告正文，返回 {content:string}；不写文件。
}
/** @virtual */
function inspect(request, content) {
  // content 是已保存报告的实际正文；对照 request 判断，返回 {passed:boolean, evidence:[string], issues:[string]}。
}
/** @virtual */
function repair(request, content, issues) {
  // 依据 issues 修正 content 中授权范围内的问题，返回 {content:string}；不写文件。
}
```

修复后再 inspect 一次；复检仍失败时由 assert 停止，不继续交付。这个例子只修一次，不循环重试。实际任务允许修复时，可以照这样为验收发现的问题留出修正分支：把失败证据、当前内容和要求交给一个修正函数，再读回实际结果复检；处理的是当前问题，不重启整条流程，也不重放已完成的操作。三个模块各做一件事，写入、读回和要不要再修由主程序决定。

已经取得的实际值，直接作为参数传给需要它的虚函数，不必进入函数后再读一遍；要核实外部状态或修改后的结果时再读回。一个虚函数的职责是只检查或只处理；检查之后要处理的，由代码看了结果再调用处理的函数。函数产生哪些副作用由它的职责决定：只负责生成并返回内容时，保存留给调用方；承担修改职责时才写入。契约里约定了返回值与输入之间可由代码确定的关系时，调用方在使用返回值前核对这个关系。

工作区里的文件是所有函数共享的状态，就像对象的字段。改代码这类工作，虚函数直接用工具改好文件就完成了，契约可以不返回内容（它回复 `null`）；下一步要用改后的内容就去读文件，检查的虚函数自己读，或者由代码读好传进去。一个文件在一步里只有一个写入者：要么虚函数直接改、不返回内容，要么它只返回内容、自己不写，由代码写入。不要两样都做：虚函数改完了文件，契约又要它返回文件内容，它只能拿修改说明凑数，代码再把说明写回去，源码就被覆盖了。判断改得对不对，读实际的文件、跑实际的测试，不把上一个函数的说明当事实。并发的函数不写同一个文件。

```js
import * as io from "io";
import * as process from "process";
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
 * @virtual
 * 按 requirements（用户的原话）修改 target 和 tests，problems 是上一轮验收发现的问题；直接改文件，不返回内容。
 */
async function modify(requirements, target, tests, problems) {}
/**
 * @virtual
 * 读取 target 和 tests 的当前内容并运行测试，对照 requirements（用户的原话）判断；只读，不修改。
 * 返回 {ok:boolean, problems:string[]}。
 */
async function review(requirements, target, tests) {}
```

modify 和 review 各做一件事：modify 改文件、不返回内容，review 读文件、跑测试、只返回判断；改几轮、什么时候停，由主程序决定。target、tests 这样的路径来自熟悉项目时看到的事实，写进程序里；用户的原话作为参数传进去，模块不用 `@uses`。review 的代码里也拿得到写文件的工具，所以契约里写明只读。

## 模块怎样被写出来

模块第一次被调用时，宿主把它的签名、契约注释和这次调用的实参交给一个 AI，由它调用 write_program 交出这个函数的完整实现、需要的辅助函数和 import；实现可含 import、顶层常量和 require；每次调用使用新的模块作用域，await 写在函数内部。写模块的 AI 只有契约和实参，调用方读过的技能不会带过来；它要用哪个技能，自己用 read_skill 读。这份代码存成这个聊天的模块文件，之后对同一契约（函数名、签名和注释原文完全一样）的调用都直接运行它，实现满足当前契约和实际输入即可，不提前设计通用框架；也不要把分析结论硬编码成返回值。代码里需要 AI 的部分声明成虚函数，它们的工具范围就是这段代码 import 和 require 的方法；普通子流程继续写成 JavaScript，不再创建另一层代码实现。这个聊天已有哪些模块，见请求里的 chat_modules；写出同样的声明就会复用。

例如模块 process(shared, part) 的代码可以是：

```js
import * as io from "io";
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

返回值要能被下一步直接使用。工具成功、函数返回或编译通过都不等于业务验收；按业务技能完成修复、收集异步结果和确认点后再交付。库方法返回工具的原始 JSON，例如 io.read 的 result.content。程序代码里的 `io.read({path})` 一次返回整个文件（`complete:true`），`io.read_lines` 一次返回整个请求范围，内容进入 JS heap，不占 AI 上下文；不要写分页循环。只有超过 200 万字符的文件才会 `complete:false`，再用 `next_start_line` 继续。AI 自己直接调用读取工具时仍是每页最多 6 万字符。

## 源码检查和错误处理

程序和模块代码都在任何实际操作前整份编译；编译失败时什么都不执行，带行列和原因的诊断交回写代码的 AI，由它改正后重新提交。

模块运行时出错且没有被捕获时，宿主交给一个修复者：它只拿到模块文件和这次的问题，即错误和停止位置、这次调用的实参、打印的输出和已完成的写入，以及模块 require 的库的说明，用 edit_file 改模块文件；改好后宿主存成新版本，从头重跑这次调用，之后的调用都用新版本。修不了时它回复 {"$task_error":{"message":"具体原因"}}，错误才抛给调用方。虚函数做不到时同样回复 $task_error，这是正常的信号：调用它的代码可以捕获，把活拆细或换一种做法。主程序里没有捕获的错误交回主 AI，由它改正后重写程序。出错的程序（主程序或模块的一次调用）还在跑的调用，在各自下一步之前停下，不再交付结果；重写或重跑的程序从头开始。结果未知的外部写入不会自动重放。权限和插件边界始终有效。

## 并发与工作区

并发的虚函数和模块在同一工作区运行，各自只拿到调用时的参数。独立任务分别处理各自的文件，或者返回内容交给调用方统一写入。并发写同一文件发生冲突时，宿主保留现有文件和待写副本并停止，不自动合并或重放。


### 模块结果与完整证据

模块按调用方要拿到的结果定义，例如核实过的事实、统计或附文件行号的结论；复用时检查返回结果是否适合当前任务，不能只看它能否执行命令。已知步骤继续写成普通 JavaScript；模块内收集、处理材料，需要 AI 判断时才调用职责窄的虚函数，不强制增加 AI 层。代码分析交给 AI 的证据应包含相关完整单元、外层条件和提前返回。搜索命中只能证明命中的一行。一个适量的完整单元用一次打印，并保留文件和行范围；不要按任意字符位置截掉尾部。宿主实际交付不下时明确报错并保存原结果，AI 根据读取引用分段查看或调整编排，保留已完成操作；未读内容不能作为结论依据。行为结论要核对完整控制路径或实际执行顺序，区分读过测试与运行过测试。

日志不再按单条 100,000 字符、200 条记录或最后 20 条静默裁剪。完整记录先保存在运行时；累计日志捕获超过堆预算时以 `OUTPUT_OVER_CAPACITY` 停止，拒收的完整记录也归档。主程序结局和模块探路结果在进入 AI 前按实际剩余上下文检查，放不下就明确失败，不交付成功前缀，原始输出、返回值和错误保存为 asset 并提供读取引用。资源错误由调用方调整编排，不作为普通实现错误自动修复重跑；已完成命令或写入不能盲目重放。只有 AI 下一次请求本身放不下时才是 `ContextOverflow`。显式按范围分页读取仍可使用，不把未请求的其余内容冒充已交付。
