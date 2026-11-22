# syntax=docker/dockerfile:1

# ── 前端构建 ──
# 基础镜像统一 digest pin（tag 可变，digest 不变），注释保留版本号便于升级时核对。
# digest 为多架构 OCI index（manifest list），linux/amd64 与 linux/arm64 共用同一值，
# 与 release.yml 的双架构镜像构建兼容。来源：Docker Hub tag API（library/<name>/tags/<tag>
# 的 digest 字段）与 GHCR registry API，2026-11-13 查询。
FROM node:22-alpine@sha256:5b05da3f938f1c604b24c633646d48805a6667f378cb4908438616f3cc48a0bf AS frontend-builder
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── Rust 构建 ──
# tag 的 1.98 与 rust-toolchain.toml 的 channel = "1.98.0" 必须一致（主/次版本相同即可，
# patch 由 rustup 按 channel 解析），CI（rust-clippy job）有脚本守卫；漂移会让容器内
# 现场下载另一套工具链，产物与本地构建不可复现。
FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS rust-builder
# 编译依赖：tray-icon 的 gtk 在 Docker 运行时不使用，但编译期仍需头文件
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# 利用 Cargo 依赖缓存：先拷贝清单拉依赖，再拷贝源码
COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY openapi.json ./
# 创建空入口骗过 cargo fetch 的路径检查
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && echo "fn main() {}" > src/helper_main.rs
RUN cargo fetch --locked
# 拷贝真实源码与前端产物
# 注意：不要把 python_worker 拷进本阶段——Rust 编译期零引用（嵌入面仅
# frontend/dist、openapi.json、docs/guides、tray.png），多拷只会让 Worker
# 侧任何改动都毒化 Rust 层缓存、整段重编
COPY src ./src
COPY resources ./resources
COPY docs/guides ./docs/guides
COPY --from=frontend-builder /build/frontend/dist ./frontend/dist
# 复写 dummy 入口后再正式编译
RUN touch src/main.rs src/helper_main.rs
RUN cargo build --release --locked

# ── 运行时 ──
FROM python:3.12-slim-bookworm@sha256:58525e1a8dada8e72d6f8a11a0ddff8d981fd888549108db52455d577f927f77
ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    UV_LINK_MODE=copy \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright

# 基础系统依赖；Rust 二进制虽以 --no-tray 运行，动态链接器仍需能解析编译时的 GTK/托盘库。
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    libgtk-3-0 libayatana-appindicator3-1 librsvg2-2 libxdo3 \
    && rm -rf /var/lib/apt/lists/*

# 非 root 运行：uid/gid 固定 1000（campus）。容器以 root 启动 entrypoint.sh，由它
# 完成存量卷属主修正后经 setpriv 降权 exec 主进程（postgres 官方镜像同款模式，见
# entrypoint.sh 头注释）。选 setpriv 而非 gosu/su-exec：它是基础镜像 Essential 包
# util-linux 的组成部分（/usr/bin/setpriv），零额外安装；gosu 需 apt 引入新包。
RUN useradd --uid 1000 --user-group --create-home --home-dir /home/campus campus

WORKDIR /app

# 安装 uv（Python 包管理器，Worker 依赖安装用）；digest 取自 GHCR registry API（2026-11-13），
# 为 linux amd64/arm64 双平台的 OCI index
COPY --from=ghcr.io/astral-sh/uv:0.12.6@sha256:88bc6eb1ccd4b82efd0e1b530caffabddf50dc2bf612e66c14ea25b8ee8a4d3d /uv /uvx /bin/

# 拷贝 Rust 二进制
COPY --from=rust-builder /build/target/release/campus-auth /usr/local/bin/campus-auth
COPY --from=rust-builder /build/target/release/campus-auth-helper /usr/local/bin/campus-auth-helper
RUN campus-auth --version

# 拷贝 Python Worker 源码
COPY python_worker ./python_worker

# 静态资源（托盘图标 / 任务录制器脚本）。图标已编译进二进制，此处供
# `GET /api/tools/task-recorder.user.js` 读取——它由 base_path 下的
# `resources/` 提供且无编译期嵌入副本（对比 docs/guides 有 GuideAsset 兜底），
# 缺失会让前端「安装录制器」404。entrypoint.sh 每次启动覆盖同步到
# ${CAMPUS_AUTH_BASE_PATH}/resources。
COPY --from=rust-builder /build/resources /opt/campus-auth/resources

# 预装 Python 依赖与 Playwright 浏览器（加速首次启动，无网络时可离线运行）。
# 任一环节失败都终止构建，禁止产出“镜像成功、Worker 不可用”的半成品。
# 三步均带 --no-dev：dev 组（pytest 等）是仓库测试依赖，运行时不需要；
# 缺任一步 uv 都会按默认把 dev 组拉回 venv，白增一层与运行无关的包。
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --project python_worker --frozen --no-dev && \
    uv run --project python_worker --frozen --no-dev playwright install --with-deps chromium && \
    uv run --project python_worker --frozen --no-dev python -c "import playwright; import worker_main"

# 构建产物属主收口：以上产物默认属 root，而运行期主进程以 campus(1000) 执行——
# - python_worker：环境引导的自愈路径（src/environment/health.rs 在清单漂移/探针失败时
#   执行 uv sync）与 ddddocr 安装/卸载都要写 pyproject.toml、uv.lock 与 .venv
# - /ms-playwright：Web API「安装浏览器」（system.rs perform_playwright_install）按需补装
# - /data：预置属主后，新命名卷首次挂载经 copy-up 继承 campus 属主，首启即可写
RUN chown -R campus:campus /app/python_worker /ms-playwright \
    && mkdir -p /data && chown campus:campus /data

# 暴露端口
EXPOSE 50721

# 数据卷：配置 / 任务 / 日志 / 加密密钥（HOME=/data/home，见下）
VOLUME ["/data"]

# 环境默认：绑定 0.0.0.0、数据目录 /data、禁用托盘与自动打开浏览器
# HOME 指向数据卷：加密密钥 ~/.campus_network_auth（config/crypto.rs 按 HOME 解析）与
# uv 缓存随 /data 持久化。旧版以 root 运行时密钥存于容器临时层 /root，重建容器即失，
# 已加密的方案密码全部失效——升级迁移步骤见 docker/README.md
ENV CAMPUS_AUTH_HOST=0.0.0.0 \
    CAMPUS_AUTH_BASE_PATH=/data \
    CAMPUS_AUTH_PORT=50721 \
    HOME=/data/home

# 健康检查：/api/health 无需鉴权
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:${CAMPUS_AUTH_PORT:-50721}/api/health || exit 1

# 入口：建目录 / 同步资源 / root 启动时修正属主后降权 exec
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
# 显式声明 root（与隐式默认一致）：entrypoint 需要 root 才能修正存量 root 属主命名卷，
# 然后由它经 setpriv 降权 exec——主进程 / Worker / Chromium 实际均以 campus(1000) 运行。
# 不以 `USER campus` 收尾的原因：那会让 entrypoint 失去 chown 权限，存量卷迁移就必须
# 手动执行；绕过 entrypoint 调试时建议显式 `--user 1000:1000`。
USER root
ENTRYPOINT ["/entrypoint.sh"]
CMD ["campus-auth", "--no-tray"]
