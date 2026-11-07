VM 阶段 0 的两把尺子

本目录保存选择范围、固定版本和逐项结果基线；不保存 test262 源码、模型源码或凭据。

test262 来自 [TC39 官方仓库](https://github.com/tc39/test262)，BSD 许可。运行规则参照 [INTERPRETING.md](https://github.com/tc39/test262/blob/e4fbf57745c6aae3d1f12f181ecef78b1665d812/INTERPRETING.md)。固定 revision 与下载归档 SHA256 见 test262-selection.json；脚本不创建 Git 分支，不提交。

PowerShell 中准备及运行：

```powershell
$env:TEST262_ROOT = python scripts/vm/prepare-test262.py
$env:VM_TEST262_REPORT = 'runs/vm-test262-current.json'
$env:VM_TEST262_BASELINE = 'tests/vm/test262-baseline.json'
cargo test --bin vm-measurements --features vm-measurements test262_subset -- --ignored --nocapture
```

首次建立新范围基线时不设置 VM_TEST262_BASELINE；后续必须对同一 revision、范围、用例源码哈希比较。旧通过用例变失败、被跳过、被删除，或源码变化，比较都失败。可用 VM_TEST262_FILTER 过滤路径，但过滤后属于不同选择范围，不能拿来和完整子集比较。

默认选集是集合、词法声明、箭头函数、Array 及少量现有内置方法，按 features 分组。选择的是 ES2015 范围和显式列出的已支持后续特性，不代表完整 ES2015 一致性测试。module、$262 和未选择的后续 feature 明确计入 skipped。未来阶段扩范围须保留旧范围的比较结果。

每个用例创建独立 Runtime。正例与 runtime 负例要实际执行；parse/early 负例只有 SWC 原始脚本解析器确实报 SyntaxError 才通过，不能把 VM 的“不支持 Map”当作语法错误通过。结果分 pass、run_failed、compile_rejected，另记录 skipped 及原因。VM 当前按严格模块模式编译，所以 noStrict 单独记 compile_rejected，不伪装成已测试非严格脚本。默认只测这一模式，不声称等同 test262 的严格/非严格双变体。

最小宿主 harness 只在测试构建中启用 assert、sameValue（包括 NaN 与正负零）、notSameValue、throws、compareArray、Test262Error、$DONE。throws 的回调照常在 VM 帧中执行。其他 includes 读取固定版本原文件；原版 propertyHelper 等如果仍依赖缺失能力，记实际编译失败，不把断言改成空函数。缺失的标准构造器值等也不在测试版偷偷补齐；其导致的编译拒绝保留为真实缺口。报告 TS_PROGRAM 行号包含 throws helper 与 includes 前缀，定位原测试时需要减去该前缀；这个限制在后续切换原版 harness 时再统一。

第二把尺子只编译模型写过的原始源码，统一调用实际交付使用的 compile_with_libraries，不执行任何宿主操作：

```powershell
python scripts/vm/export-corpus.py --app-db <宿主数据库> --codex-sessions <日志目录> --output <仓库外的corpus.jsonl>
$env:VM_CORPUS_JSONL = '<仓库外的corpus.jsonl>'
$env:VM_CORPUS_REPORT = 'runs/vm-corpus-current.json'
$env:VM_CORPUS_BASELINE = 'tests/vm/corpus-baseline.json'
cargo test --bin vm-measurements --features vm-measurements model_source_corpus -- --ignored --nocapture
```

导出保留每次提交（包括被拒的尝试），不修改或修补源码。Codex orchestration 的 tools/text/store 等宿主 API 与本产品不同，相关拒绝也会出现；这份通过率不能直接当标准 JS 覆盖率，更不能和来源、时间范围不同的旧语料比例硬比。基线记录样本哈希和诊断，不含源码；原始 JSONL 保留在本机外部缓存，下一阶段复用同一份，不重新导出改变分母。

额外目录测量可用 `VM_TEST262_SELECTION` 指向另一份选择文件；默认仍使用冻结的 `test262-selection.json`。例如阶段 3 第一部分另跑 `test262-selection-stage3a-functions.json`，覆盖 call/apply/bind、this、调用表达式及箭头函数。额外报告与原有 1756 项报告分开，存在重叠，不能把两者的通过数相加；只有原选集报告用于阶段间逐项比较。

阶段 3 第二部分另有 `test262-selection-stage3b-prototypes.json`，覆盖 Object.create/getPrototypeOf/setPrototypeOf/isPrototypeOf、instanceof、new 和对象字面量目录。设置 `VM_TEST262_SELECTION` 后单独运行，结果见 `test262-stage3b-prototypes.json`。它同样与其他选集重叠，不能相加；其中属性描述符、getter/setter、Symbol、class 等仍有未实现用例，报告实际拒绝或失败。

常规测试不会启动这两项测量，它们均为 #[ignore]。没有任何真实模型调用。

阶段 3 第三部分增加 `test262-selection-stage3c-descriptors.json`，独立覆盖属性描述符、完整性级别、propertyIsEnumerable、Reflect 和对象字面量，共 3804 项；范围及全部结果单独保存，不能与重叠选集相加。原型扩展选集相对阶段 3b 的比较仍报告 `Object/create/15.2.3.5-4-300.js`，没有改旧报告或放宽比较器。该旧通过项依赖尚未实现描述符时提前抛 TypeError，如今暴露原有 script-global-this 环境限制；Node 对照、逐项分类及原始失败日志见 [阶段 3c 报告](REPORT-stage3c.md)。固定 1756 项及函数选集仍要求旧通过项零退步。

阶段 4 增加 `test262-selection-stage4-iteration.json`，覆盖 Symbol、四种内置迭代器原型、for-of 及数组表达式，共 957 项。只新增 Symbol.asyncIterator/description 的 feature 选择，不把 for-await 或生成器标成已支持；相关后续能力仍按真实编译结果计入拒绝或跳过。原有四组选集分别继续与阶段 3c 逐项比较，通过项均不得退步；新选集与它们重叠，不相加。结果、Node 对照保留的一处规范差异和异常进程重跑记录见 [阶段 4 报告](REPORT-stage4.md)。

轻量入口 vm-measurements 直接编译同一份 src/agent_program.rs 及其子模块，不复制、模拟或改写编译器。它仅提供动作 ID 的随机数来源，没有模型服务或宿主工具。两个依赖完整宿主工具目录的声明测试在此入口排除，默认主程序入口照常保留。因此这组 VM 测试不能代替主程序的全量失败集合比较：

```powershell
cargo test --bin vm-measurements --features vm-measurements -- --test-threads=4
cargo test --bin codex-workspace-mcp -- --test-threads=8
```
