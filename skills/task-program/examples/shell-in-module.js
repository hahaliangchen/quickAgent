// 主流程只编排：shell 留在模块的确定性实现中。
const summary = inspectPackage(".");
console.log(JSON.stringify(summary));

/** @virtual */
function inspectPackage(workdir) {
  // 用 Node 读取 workdir/package.json 的实际 JSON；检查进程结束、退出码和输出完整性。
  // 返回 {name:string, scriptCount:number, scriptNames:[string]}，列出实际脚本名称。
  // 不修改文件；不把完整 stdout/stderr 打印或传给 AI。
}
