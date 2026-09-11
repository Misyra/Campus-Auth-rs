# syntax=docker/dockerfile:1

# ── 前端构建 ──
FROM node:22-alpine AS frontend-builder
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── Rust 构建 ──
FROM rust:1.98-bookworm AS rust-builder
# 编译依赖：tray-icon 的 gtk 在 Docker 运行时不使用，但编译期仍需头文件
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
# 利用 Cargo 依赖缓存：先拷贝清单拉依赖，再拷贝源码
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY openapi.json ./
# 创建空入口骗过 cargo fetch 的路径检查
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && echo "fn main() {}" > src/helper_main.rs
RUN cargo fetch --locked
# 拷贝真实源码与前端产物
COPY src ./src
COPY resources ./resources
COPY python_worker ./python_worker
COPY docs/guides ./docs/guides
COPY --from=frontend-builder /build/frontend/dist ./frontend/dist
# 复写 dummy 入口后再正式编译
RUN touch src/main.rs src/helper_main.rs
RUN cargo build --release --locked

# ── 运行时 ──
FROM python:3.12-slim-bookworm
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

WORKDIR /app

# 安装 uv（Python 包管理器，Worker 依赖安装用）
COPY --from=ghcr.io/astral-sh/uv:0.12.6 /uv /uvx /bin/

# 拷贝 Rust 二进制
COPY --from=rust-builder /build/target/release/campus-auth /usr/local/bin/campus-auth
COPY --from=rust-builder /build/target/release/campus-auth-helper /usr/local/bin/campus-auth-helper
RUN campus-auth --version

# 拷贝 Python Worker 源码
COPY python_worker ./python_worker

# 预装 Python 依赖与 Playwright 浏览器（加速首次启动，无网络时可离线运行）。
# 任一环节失败都终止构建，禁止产出“镜像成功、Worker 不可用”的半成品。
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --project python_worker --frozen --no-dev && \
    uv run --project python_worker --frozen playwright install --with-deps chromium && \
    uv run --project python_worker --frozen python -c "import playwright; import worker_main"

# 暴露端口
EXPOSE 50721

# 数据卷：配置 / 任务 / 日志
VOLUME ["/data"]

# 环境默认：绑定 0.0.0.0、数据目录 /data、禁用托盘与自动打开浏览器
ENV CAMPUS_AUTH_HOST=0.0.0.0 \
    CAMPUS_AUTH_BASE_PATH=/data \
    CAMPUS_AUTH_PORT=50721

# 健康检查：/api/health 无需鉴权
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:${CAMPUS_AUTH_PORT:-50721}/api/health || exit 1

# 入口：确保 /data 权限后启动
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
CMD ["campus-auth", "--no-tray"]
