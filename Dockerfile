# syntax=docker/dockerfile:1

# ── 前端构建 ──
FROM node:22-alpine AS frontend-builder
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── Rust 构建 ──
FROM rust:1.85-bookworm AS rust-builder
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
RUN cargo fetch || true
# 拷贝真实源码与前端产物
COPY src ./src
COPY resources ./resources
COPY python_worker ./python_worker
COPY --from=frontend-builder /build/frontend/dist ./frontend/dist
# 复写 dummy 入口后再正式编译
RUN touch src/main.rs src/helper_main.rs
RUN cargo build --release

# ── 运行时 ──
FROM python:3.12-slim-bookworm
ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    UV_LINK_MODE=copy

# 基础系统依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 安装 uv（Python 包管理器，Worker 依赖安装用）
COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/

# 拷贝 Rust 二进制
COPY --from=rust-builder /build/target/release/campus-auth /usr/local/bin/campus-auth
COPY --from=rust-builder /build/target/release/campus-auth-helper /usr/local/bin/campus-auth-helper

# 拷贝 Python Worker 源码
COPY python_worker ./python_worker

# 预装 Python 依赖与 Playwright 浏览器（加速首次启动，无网络时可离线运行）
# 优先用 uv 创建 venv 并安装，再装 Chromium 及其 OS 依赖；失败则降级到 pip
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --project python_worker --frozen --no-dev 2>&1 | tail -n 20; \
    if [ -f python_worker/.venv/bin/python ]; then \
        echo "[docker] venv 已创建，安装 Chromium..."; \
        python_worker/.venv/bin/pip install --no-cache-dir "playwright>=1.40" 2>&1 | tail -n 5; \
        python_worker/.venv/bin/playwright install --with-deps chromium 2>&1 | tail -n 20 || \
        echo "[warn] venv 内 Chromium 安装失败，启动时重试"; \
    else \
        echo "[docker] venv 创建失败，尝试系统级安装..."; \
        pip install --no-cache-dir "playwright>=1.40" 2>&1 | tail -n 5; \
        playwright install --with-deps chromium 2>&1 | tail -n 20 || \
        pip install --no-cache-dir playwright && playwright install chromium 2>&1 | tail -n 20 || \
        echo "[warn] Playwright 预装失败，容器启动时将按需安装"; \
    fi; \
    # 同时预装系统级 playwright 供健康检查 fallback
    pip install --no-cache-dir "playwright>=1.40" 2>&1 | tail -n 5 || true

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
