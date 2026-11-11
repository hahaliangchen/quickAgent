# Tool environment only: no application source, application build or credentials.
ARG BASE_IMAGE=macrosan/kylin@sha256:525d7ef1663a5d9debcfeb041a8759608e890efc08ab70d9bed2804b309552ba
FROM ${BASE_IMAGE}
ARG TARGETARCH
ARG BUILD_JOBS=4
ENV LANG=C.UTF-8 TZ=Asia/Shanghai \
    PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
    RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/go/bin:/opt/ppt-python/bin:/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin \
    LD_LIBRARY_PATH=/opt/cxx-runtime/lib:/usr/local/lib \
    NODE_PATH=/opt/ppt-tools/node_modules \
    PPT_SVG_CANVAS_MODULE=/opt/ppt-tools/node_modules/@napi-rs/canvas \
    PPT_PLAYWRIGHT_MODULE=/opt/ppt-tools/node_modules/playwright \
    GVA_PYTHON=/opt/ppt-python/bin/python PPT_ASSET_PYTHON=/opt/ppt-python/bin/python

RUN test "$TARGETARCH" = arm64 \
    && sed -i 's|http://update.cs2c.com.cn:8080|https://update.cs2c.com.cn|g' /etc/yum.repos.d/*.repo \
    && env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy -u all_proxy \
        yum --setopt=timeout=30 --setopt=retries=2 --setopt=proxy= -y install \
        ca-certificates curl tar gzip xz unzip zstd git file rsync findutils which patch \
        gcc gcc-c++ make cmake pkgconf binutils glibc-devel \
        openssl-devel bzip2-devel libffi-devel zlib-devel xz-devel readline-devel sqlite-devel \
        cairo-devel pango-devel gdk-pixbuf2 librsvg2 fontconfig freetype-devel \
        bubblewrap poppler-utils \
    && yum clean all

COPY artifacts/ /tmp/environment/artifacts/
COPY artifacts.sha256 /tmp/environment/
RUN cd /tmp/environment && sha256sum -c artifacts.sha256

# Private C++ runtime: Kylin's GCC 7 runtime lacks the GLIBCXX version needed by
# official Node 22. Extract the pinned conda-forge runtime with its licenses;
# never replace system glibc or /usr/lib64/libstdc++.
RUN mkdir -p /opt/cxx-runtime /tmp/cxx \
    && cd /tmp/cxx \
    && for package in /tmp/environment/artifacts/libgcc-*.conda /tmp/environment/artifacts/libstdcxx-*.conda; do \
        unzip -o "$package" 'pkg-*.tar.zst' 'info-*.tar.zst' >/dev/null || exit 1; \
    done \
    && for archive in *.tar.zst; do tar --zstd -xf "$archive" -C /opt/cxx-runtime || exit 1; done \
    && cd / && rm -rf /tmp/cxx \
    && mkdir -p /opt/node /usr/local/go \
    && tar -xJf /tmp/environment/artifacts/node-v22.22.1-linux-arm64.tar.xz -C /opt/node --strip-components=1 \
    && tar -xzf /tmp/environment/artifacts/go1.27.1.linux-arm64.tar.gz -C /usr/local \
    && node --version && go version

RUN if ! test -x /usr/local/bin/python3.12; then \
        tar -xzf /tmp/environment/artifacts/Python-3.12.11.tgz -C /tmp \
        && cd /tmp/Python-3.12.11 \
        && ./configure --prefix=/usr/local --enable-shared --with-ensurepip=install \
        && make -j"$BUILD_JOBS" && make altinstall \
        && echo /usr/local/lib > /etc/ld.so.conf.d/quickagent-python.conf \
        && ldconfig && cd / && rm -rf /tmp/Python-3.12.11 || exit 1; \
    fi \
    && /usr/local/bin/python3.12 -m venv /opt/ppt-python

RUN chmod +x /tmp/environment/artifacts/rustup-init \
    && /tmp/environment/artifacts/rustup-init -y --no-modify-path --profile minimal \
        --default-host aarch64-unknown-linux-gnu --default-toolchain 1.98.1 \
        --target wasm32-unknown-unknown \
    && mkdir /tmp/wasm-pack /tmp/ripgrep \
    && tar -xzf /tmp/environment/artifacts/wasm-pack-v0.15.1-aarch64-unknown-linux-musl.tar.gz -C /tmp/wasm-pack --strip-components=1 \
    && tar -xzf /tmp/environment/artifacts/ripgrep-15.1.0-aarch64-unknown-linux-gnu.tar.gz -C /tmp/ripgrep --strip-components=1 \
    && install -m 0755 /tmp/wasm-pack/wasm-pack /tmp/ripgrep/rg /usr/local/bin/ \
    && mkdir -p /opt/environment-licenses/wasm-pack /opt/environment-licenses/ripgrep \
    && cp /tmp/wasm-pack/LICENSE* /opt/environment-licenses/wasm-pack/ \
    && cp /tmp/ripgrep/COPYING /tmp/ripgrep/LICENSE-MIT /tmp/ripgrep/UNLICENSE /opt/environment-licenses/ripgrep/ \
    && rm -rf /tmp/wasm-pack /tmp/ripgrep

COPY ppt-requirements.txt /opt/environment/ppt-requirements.txt
COPY ppt-tools.package.json /opt/ppt-tools/package.json
COPY ppt-tools.package-lock.json /opt/ppt-tools/package-lock.json
RUN python -m pip install -r /opt/environment/ppt-requirements.txt \
    && python -m pip freeze > /opt/environment/python-packages.lock \
    && npm --prefix /opt/ppt-tools ci --include=optional --no-audit --no-fund \
    && mkdir -p /usr/local/share/fonts/noto /opt/environment-licenses/noto \
    && cp /tmp/environment/artifacts/NotoSansCJKsc-Regular.otf /usr/local/share/fonts/noto/ \
    && cp /tmp/environment/artifacts/NotoCJK-LICENSE /opt/environment-licenses/noto/LICENSE \
    && fc-cache -f \
    && cp /tmp/environment/artifacts.sha256 /opt/environment/ \
    && rpm -qa | sort > /opt/environment/rpm-packages.lock \
    && rm -rf /tmp/environment

COPY check.sh check-tools.cjs /opt/environment/
RUN mkdir -p /usr/share/fonts/quickagent \
    && cp /usr/local/share/fonts/noto/NotoSansCJKsc-Regular.otf /usr/share/fonts/quickagent/
RUN bash /opt/environment/check.sh
RUN python -m pip install PyYAML==6.0.3 \
    && python -m pip freeze > /opt/environment/python-packages.lock
COPY project-sources.json /opt/environment/project-sources.json
WORKDIR /work
CMD ["bash"]
