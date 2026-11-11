ARG TOOLS_IMAGE=quickagent-kylin-arm64-env:v1
FROM ${TOOLS_IMAGE} AS tools
# ARM64 manifest of the official Playwright 1.62.1 Noble image.
FROM mcr.microsoft.com/playwright@sha256:941cc91e5022880ac1d14ae90b476b624deb6399dbbc28d612d5d5bd7928fcbd
COPY --from=tools /opt/ppt-tools /opt/ppt-tools
COPY --from=tools /opt/node /opt/node
COPY --from=tools /usr/local/share/fonts/noto /usr/local/share/fonts/noto
COPY --from=tools /opt/environment-licenses/noto /opt/environment-licenses/noto
ENV PATH=/opt/node/bin:$PATH NODE_PATH=/opt/ppt-tools/node_modules \
    PPT_PLAYWRIGHT_MODULE=/opt/ppt-tools/node_modules/playwright \
    PPT_SVG_CANVAS_MODULE=/opt/ppt-tools/node_modules/@napi-rs/canvas \
    PPT_DESIGN_BROWSER=/opt/environment/chromium.sh \
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
RUN mkdir -p /usr/share/fonts/quickagent \
    && cp /usr/local/share/fonts/noto/NotoSansCJKsc-Regular.otf /usr/share/fonts/quickagent/ \
    && fc-cache -f
COPY check-browser.cjs /opt/environment/check-browser.cjs
COPY chromium.sh /opt/environment/chromium.sh
RUN chmod 0755 /opt/environment/chromium.sh
WORKDIR /work
CMD ["bash"]
