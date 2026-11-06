# 重新生成 联通汇报版式模板.pptx

在 pptx-editor-engine 目录执行（需要已构建 rust-engine/pkg-node）：

    npx vite-node /home/goodtime/Documents/project/codex-workspace-mcp/scratch/yalong-layout-template/build/build.ts \
      /home/goodtime/Documents/project/codex-workspace-mcp/scratch/unicom-template-standard/original-template.pptx \
      /home/goodtime/Documents/project/codex-workspace-mcp/scratch/yalong-layout-template/联通汇报版式模板.pptx \
      /tmp/yalong-renders

以原红白模板为底：保留封面、目录、结尾，关键字换成示例文字；内容页复制 10 份，按 layouts.ts 写入版式，页眉装饰标为固定，图标组标为参考素材。
