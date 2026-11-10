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
