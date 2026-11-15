/**
 * Run verification shell commands sequentially in a Layer-1 module frame and
 * return { total, passed, failed, ok, results }.
 */
async function runChecks(checks, workdir) {
  if (!Array.isArray(checks) || checks.length === 0) {
    throw new Error("runChecks requires a non-empty array of command strings");
  }
  const cwd = typeof workdir === "string" && workdir.trim() ? workdir : ".";
  const proc = require("process");
  const results = [];
  let passed = 0;
  let failed = 0;
  for (const cmd of checks) {
    if (typeof cmd !== "string" || !cmd.trim()) {
      throw new Error("runChecks entries must be non-empty command strings");
    }
    const res = proc.exec_command({ cmd: cmd, workdir: cwd, yield_time_ms: 30000 });
    if (res && res.running && res.session_id) {
      proc.kill_session({ session_id: res.session_id });
      failed += 1;
      results.push({ cmd: cmd, ok: false, exitCode: -1, excerpt: "command exceeded 30s yield window" });
      continue;
    }
    const code = res && typeof res.exit_code === "number" ? res.exit_code : 0;
    const out = res && typeof res.stdout === "string" ? res.stdout : "";
    const err = res && typeof res.stderr === "string" ? res.stderr : "";
    const text = err ? out + "\n" + err : out;
    const excerpt = text.length > 1200 ? text.slice(text.length - 1200) : text;
    const ok = code === 0;
    if (ok) {
      passed += 1;
    } else {
      failed += 1;
    }
    results.push({ cmd: cmd, ok: ok, exitCode: code, excerpt: excerpt });
  }
  return { total: results.length, passed: passed, failed: failed, ok: failed === 0, results: results };
}
