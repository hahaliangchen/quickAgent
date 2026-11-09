# 偶发访问异常调查：已复现到逻辑处理器 8

2026-11-08。本轮没有修改 VM 或宿主的生产代码。调查从旧 dump 推进到独立最小程序和 CPU 亲和性对照，已经跨 Rust、Python 复现数据处理错误。当前强证据指向逻辑处理器 8 的执行稳定性或其固件/运行条件；尚不能区分配置不稳定与 CPU 本体故障，不能宣称 CPU 已损坏或问题已修复。

已确认的事实：

- 阶段 5 崩溃日志中，784 个测试已有 783 个完成，唯一未结束的是 `single_agent::program_flow::tests::paused_concurrent_calls_resume_where_they_stopped`。重新解析 dump 的 ThreadNames 流，异常线程名称也正是这个测试。[未结束用例比较](../../runs/crash-audit-stage5-unfinished-tests.json)。其他历史 dump 的异常线程和异常代码另行保存，没有将 shell 测试的中止退出和 VM 访问异常自动归为同一原因。
- 保存当前完整测试二进制及匹配 PDB 后，单独重复该测试 **8 次全部通过**，每次约 14–18 秒。这只能说明它不稳定复现，不能说明异常消失。[记录](../../runs/crash-investigation/isolated-results.json)。
- 编写了不加载项目 VM、宿主、Tokio、SWC 或数据库的 [Rust 最小程序](../../runs/crash-investigation/json-stdlib-repro.rs)，只依赖 std 和现有 serde_json，反复构建 BTreeMap、序列化、解析、校验、删除。未限制核心的第一次运行出现 JSON 非法控制字符，随后以 **0xc0000005** 退出，并生成了独立 dump。[日志](../../runs/crash-investigation/json-stdlib-first.txt)。这个现象不要求项目 VM 存在。
- 同一 Rust exe 限制在 P 核组（mask `0xffff`）时，**两次分别在 1.827 秒、5.708 秒访问异常**；限制在 E 核组（`0xfff0000`）时，**两次各运行 35 秒通过**。可执行文件相同，输入和线程数相同，只改变子进程的亲和性。[完整对照](../../runs/crash-investigation/json-affinity-results.json)。
- 进一步为每个进程只开 **1 个工作线程**，分别固定到逻辑处理器 0、2、4、6、8、10、12、14、16：只有 **8** 在 5.78 秒内访问异常，其他八个各运行 20 秒通过。随后单独复测 8 又在 6.35 秒访问异常；0、9 在这次 20 秒对照中通过。不能因一次通过就认定 9 永远健康，也不能将该结果表述为全部 P 核都坏。[单核对照](../../runs/crash-investigation/json-single-core-results.json)、[复测](../../runs/crash-investigation/cross-language-core8-results.json)。
- [独立 Python 程序](../../runs/crash-investigation/python-json-repro.py) 只用标准库 json 和单线程，固定在 8 上约 **2.748 秒**出现 dumps/loads 结果不相等；固定在 16 上运行 20 秒通过。捕获实验第二次出现 JSON 解析错误，第三次保留了一项明确差异：`key-1961` 的原值是 `1961`，错误输出中变成 `4111111108381111111111111111111071961`。[首次日志](../../runs/crash-investigation/python-core8.txt)、[差异日志](../../runs/crash-investigation/python-core8-capture2.txt)。
- 捕获的 **同一份 JSON 原文** 在避开 8、9 后重新解析，4096 项全部符合生成规则，没有错误字段；原文 SHA256 为 `fe660f3835011f34ddfef431b0f296603ff02e4ac703ec2e183d5d116fe5e14b`。这说明不能只把错误归给输入 JSON，异常发生在该核上的处理/执行过程中；具体是解析、整数计算/比较还是数据读取损坏，还不能仅靠错误打印细分。[原文](../../runs/crash-investigation/python-core8-bad-json.txt)、[对照重解析](../../runs/crash-investigation/python-capture-reparse.json)。

保留的阴性对照：

不含 serde 的 Rust BTreeMap<String,String> 测试运行 40 秒通过。MSVC 编译的 C++/nlohmann-json 独立程序在 P/E 核组四轮各 35 秒均通过，另一次固定在 8 上运行 20 秒也通过。没有删除这些与“所有工作负载都会坏”不符的结果。这是工作负载相关的稳定性问题；单个程序通过不能推翻已捕获的跨语言数据错误，也不能单凭这些实验确认硅片退化。[C++ 源码](../../runs/crash-investigation/json-msvc-repro.cpp)、[P/E 对照](../../runs/crash-investigation/msvc-affinity-results.json)。第三方单头文件来自 nlohmann/json 官方 v3.12.0，SHA256 与复现源码一同记录。

机器和固件信息：

- CPU：Intel Core i7-14700KF，28 个逻辑处理器。CPU Set 拓扑显示 8、9 属于同一个 P 核，16–27 为 E 核；这里的编号是 Windows 逻辑处理器编号，不是建议直接在 BIOS 中禁用某个同号核心。[拓扑](../../runs/crash-investigation/cpu-topology.json)。
- 主板：MSI B760M GAMING PLUS WIFI (MS-7D99)，Version 3.0。
- BIOS：H.31，日期 2024-06-27；注册表原始微码修订字节 `23 01 00 00`，按小端解释为 **0x123**。内存为两条 16GB，配置频率 5600。只读取了这些信息，没有修改 BIOS、功耗、降压、超频或内存设置。
- [Intel 官方说明](https://www.intel.com/content/www/us/en/support/articles/000102331/processors.html) 涉及部分 13/14 代桌面处理器的 Vmin Shift 稳定性问题，建议使用 Intel Default Settings 及含 **0x12F 或更新微码**的最新 BIOS。当前固件早于这一建议。本机实验与该类问题相符，但还不是特定硬件故障的厂家鉴定。

复测与修复边界：

需要先确认是否有 CPU 降压、超频或非默认功耗参数；这些设置本身也可能导致某个核心不稳定。下一步应在 [这块主板的 MSI 官方支持页](https://www.msi.com/Motherboard/B760M-GAMING-PLUS-WIFI/support) 核对和更新正式固件，并使用 Intel Default Settings，然后重复同一最小程序的 8/控制核对照。更新固件会改变机器并需要重启，本轮未执行。若官方固件和默认配置下仍复现，应将最小程序、差异记录和 dump 交给 CPU/主板售后鉴定，而不是把它伪装成 VM 已修复。

没有给正式程序增加永久绑核、吞异常或自动重试。亲和性只设置在本轮启动的实验子进程，以及用于可靠分析数据的独立 Python 控制器进程；没有更改正在运行的 Grow/Codex 或其他用户程序的亲和性。

逐项比较失败集合：

在完整测试子进程中临时排除 8、9（mask `0xffffcff`），使用完全相同的测试二进制、8 个测试线程、全部用例，无过滤，结果为 **764 通过、29 失败、23 忽略**，68.12 秒。与阶段 5 补充修复的基线逐名比较：**新增失败 0，消失失败 0**。[完整日志](../../runs/crash-investigation/full-exclude-core8-9.txt)、[亲和性及退出状态](../../runs/crash-investigation/full-exclude-core8-9-result.json)、[完整失败集合](../../runs/crash-investigation/failure-comparison.json)。这是诊断性环境对照，不是修复后的默认硬件验收。

阶段 5 补充修复记录中的所有生产 VM/指南源码哈希保持一致，因此本轮没有新的语言语义或程序行为变更，也没有通过修改断言消除旧失败。[证据收集脚本](../../runs/crash-investigation/collect-evidence.py) 验证源码哈希、最小程序源码、实验退出码、捕获数据重解析和逐项失败集合；[机器记录](../../runs/crash-investigation/investigation-validation.json) 保存全部对照、阴性结果及尚未确认的边界。
