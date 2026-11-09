import * as process from "host:process";

function inspectPackage(workdir) {
  // 相同的单引号参数写法可用于 Windows PowerShell 7 和 Linux Bash；需要已安装 Node。
  const result = process.exec_command({
    cmd: "node -p 'JSON.stringify(require(\"./package.json\"))'",
    workdir: workdir,
    yield_time_ms: 30000
  });
  if (result.running) {
    // 本例给这个短小只读步骤 30 秒；超时后停止自己的进程，不解析尚未完成的输出。
    process.kill_session({session_id: result.session_id});
    throw new Error("package inspection is still running; stopped this session");
  }
  assert(result.exit_code === 0, "package inspection exit_code: " + result.exit_code + "; stderr: " + result.stderr);
  // Capacity failures stop delivery and preserve a reference; no partial JSON
  // is returned as success. Parse only after the process has finished.
  const manifest = JSON.parse(result.stdout);
  const scripts = manifest.scripts || {};
  const names = Object.keys(scripts);
  return {
    name: typeof manifest.name === "string" ? manifest.name : "",
    scriptCount: names.length,
    scriptNames: names
  };
}
