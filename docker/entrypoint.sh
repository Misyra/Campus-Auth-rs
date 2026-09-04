#!/bin/sh
set -eu

# Docker 入口：准备数据目录并启动主进程

DATA_DIR="${CAMPUS_AUTH_BASE_PATH:-/data}"

# 确保数据目录与子目录存在（首次挂载空卷时）
mkdir -p "${DATA_DIR}/config" "${DATA_DIR}/tasks" "${DATA_DIR}/logs" "${DATA_DIR}/environment" 2>/dev/null || true

# 修正权限（挂载卷可能为 root 所属，容器内非 root 运行时可写）
# 仅在可写时尝试，避免只读挂载报错退出
chmod 755 "${DATA_DIR}" 2>/dev/null || true

# 若未显式指定 host/port，使用环境变量默认值
# 由 launcher 的 clap env 已处理，此处仅打印提示
echo "[entrypoint] 数据目录: ${DATA_DIR}"
echo "[entrypoint] 监听: ${CAMPUS_AUTH_HOST:-0.0.0.0}:${CAMPUS_AUTH_PORT:-50721}"
echo "[entrypoint] 启动命令: $*"

exec "$@"
