ARG TOOLS_IMAGE=quickagent-kylin-arm64-env:v1
FROM ${TOOLS_IMAGE}
COPY app/ /opt/customer/
ENV GIN_MODE=release TZ=Asia/Shanghai \
    PPT_CUSTOMER_EDITION=1 \
    GVA_PYTHON=/opt/ppt-python/bin/python PPT_ASSET_PYTHON=/opt/ppt-python/bin/python
RUN test "$(uname -m)" = aarch64 \
    && chmod 0755 /opt/customer/bin/quickagent /opt/customer/bin/ai-ppt-server /opt/customer/bin/customer-init
WORKDIR /state/server
EXPOSE 8899
ENTRYPOINT ["python", "/opt/customer/bin/entrypoint.py"]
CMD ["serve"]
