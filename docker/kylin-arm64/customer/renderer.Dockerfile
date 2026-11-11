ARG BROWSER_IMAGE=quickagent-ppt-browser-arm64-env:v1
FROM ${BROWSER_IMAGE}
COPY renderer/engine/ /opt/ppt-engine/
WORKDIR /opt/ppt-engine
EXPOSE 8787
CMD ["node", "/opt/ppt-engine/node_modules/vite-node/vite-node.mjs", "tools/render-server.ts", "--", "--host", "0.0.0.0", "--port", "8787"]
