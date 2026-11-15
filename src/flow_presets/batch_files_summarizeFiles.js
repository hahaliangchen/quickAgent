/**
 * Collect multiple files deterministically via batch-files.readMany, then ask
 * one isolated Layer-2 judgment helper to answer question from those contents.
 */
async function summarizeFiles(paths, question) {
  if (typeof question !== "string" || !question.trim()) {
    throw new Error("summarizeFiles requires a non-empty question");
  }
  const files = await readMany(paths);
  return await synthesizeFiles(files, question);
}

/** @virtual Read candidate files deterministically using sibling readMany. */
async function readMany(paths) {}

/**
 * @virtual
 * Given collected { path, ok, content, error } records and question,
 * return a concise, high-signal { answer: string, citations: string[], missing: string[] }
 * that directly answers question with exact file/symbol evidence and no redundant prose.
 */
async function synthesizeFiles(files, question) {}
