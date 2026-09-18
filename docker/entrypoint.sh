#!/bin/sh
set -eu

# Docker 入口：准备数据目录并启动主进程

DATA_DIR="${CAMPUS_AUTH_BASE_PATH:-/data}"

# 确保数据目录与子目录存在（首次挂载空卷时）
mkdir -p "${DATA_DIR}/config" "${DATA_DIR}/tasks" "${DATA_DIR}/logs" "${DATA_DIR}/environment" 2>/dev/null || true

# 静态资源同步到数据目录：`GET /api/tools/task-recorder.user.js` 从
# <base_path>/resources/tools/ 读取且无编译期嵌入副本，缺失会让前端
# 「安装录制器」404。镜像只读副本位于 /opt/campus-auth/resources，每次启动
# 覆盖同步——与便携版 helper 的 overlay 口径一致（覆盖同名、新增缺失），
# 避免升级镜像后仍用旧脚本。
# 失败不阻断启动：仅影响录制器下载一个入口，不应让整个容器起不来。
if [ -d /opt/campus-auth/resources ]; then
    mkdir -p "${DATA_DIR}/resources" 2>/dev/null || true
    cp -rf /opt/campus-auth/resources/. "${DATA_DIR}/resources/" 2>/dev/null || \
        echo "[entrypoint] 警告：静态资源同步失败，任务录制器脚本可能不可用"
fi

# 修正权限（挂载卷可能为 root 所属，容器内非 root 运行时可写）
# 仅在可写时尝试，避免只读挂载报错退出
chmod 755 "${DATA_DIR}" 2>/dev/null || true

# 若未显式指定 host/port，使用环境变量默认值
# 由 launcher 的 clap env 已处理，此处仅打印提示
echo "[entrypoint] 数据目录: ${DATA_DIR}"
echo "[entrypoint] 监听: ${CAMPUS_AUTH_HOST:-0.0.0.0}:${CAMPUS_AUTH_PORT:-50721}"
echo "[entrypoint] 启动命令: $*"

exec "$@"
