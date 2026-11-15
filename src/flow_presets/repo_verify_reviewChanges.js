/**
 * Gather git status and diff stat deterministically, then delegate one focused
 * Layer-2 review against focus.
 */
async function reviewChanges(focus) {
  const targetFocus = typeof focus === "string" && focus.trim()
    ? focus
    : "correctness and regressions";
  const proc = require("process");
  const statusRes = proc.exec_command({ cmd: "git status --short", workdir: ".", yield_time_ms: 15000 });
  if (statusRes && statusRes.running && statusRes.session_id) {
    proc.kill_session({ session_id: statusRes.session_id });
    throw new Error("git status exceeded 15s yield window");
  }
  const diffRes = proc.exec_command({ cmd: "git diff --stat", workdir: ".", yield_time_ms: 15000 });
  if (diffRes && diffRes.running && diffRes.session_id) {
    proc.kill_session({ session_id: diffRes.session_id });
    throw new Error("git diff --stat exceeded 15s yield window");
  }
  const statusText = statusRes && typeof statusRes.stdout === "string" ? statusRes.stdout : "";
  const diffStat = diffRes && typeof diffRes.stdout === "string" ? diffRes.stdout : "";
  return await assessRepoChanges(statusText, diffStat, targetFocus);
}

/**
 * @virtual
 * Assess git statusText and diffStat against focus.
 * Return { summary: string, risks: string[], changedFiles: string[] }.
 */
async function assessRepoChanges(statusText, diffStat, focus) {}
