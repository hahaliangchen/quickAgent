/**
 * Read multiple files deterministically in a Layer-1 module frame and return
 * structured { path, ok, content, error } entries without polling individual reads.
 */
async function readMany(paths) {
  if (!Array.isArray(paths) || paths.length === 0) {
    throw new Error("readMany requires a non-empty array of file paths");
  }
  const io = require("io");
  const results = [];
  for (const path of paths) {
    if (typeof path !== "string" || !path.trim()) {
      throw new Error("readMany path entries must be non-empty strings");
    }
    try {
      const data = io.read({ path: path });
      const content = data && typeof data.content === "string" ? data.content : "";
      results.push({ path: path, ok: true, content: content, error: "" });
    } catch (err) {
      const msg = err && err.message ? String(err.message) : String(err);
      results.push({ path: path, ok: false, content: "", error: msg });
    }
  }
  return results;
}
