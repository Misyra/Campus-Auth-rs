#!/bin/sh
set -eu

# Docker 入口：建目录 → 同步静态资源 → （root 启动时）修正数据目录属主 → 降权 exec 主进程
#
# 降权模型（postgres 官方镜像同款「root 启动 + 降权 exec」）：
#   - 镜像默认以 root 启动本脚本：只有 root 能修正存量 root 属主命名卷（历史版本容器
#     全程以 root 运行，卷内文件均属 root），随后立即经 setpriv 降权 exec——主进程、
#     Worker、Chromium 实际全部以 campus(1000) 运行，容器内不存在 root 工作负载；
#   - 显式 `--user 1000:1000`（或 Kubernetes/OpenShift 注入任意 uid）启动时走非 root
#     分支：跳过 chown，尽力建目录与同步资源，失败仅告警。

DATA_DIR="${CAMPUS_AUTH_BASE_PATH:-/data}"
# 降权目标：uid/gid 1000，与 Dockerfile 的 useradd 保持一致
RUN_UID=1000
RUN_GID=1000

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

# 修正权限与属主：存量 root 属主命名卷平滑迁移（历史版本容器全 root 运行，
# 升级后卷内文件必须归 campus(1000) 所有，主进程才能读写）。
# 递归 chown 严格限定在 ${DATA_DIR} 内部；失败（如只读挂载）不阻断，仅告警。
if [ "$(id -u)" = "0" ]; then
    # 属主探测只看第一层且命中即停（find -print -quit）：存量整卷 root 属主首次启动
    # 必命中并触发一次递归修正，之后每层均为 1000、每次启动只做浅层扫描。深层个别
    # 文件属主不符（如曾以 --user root 调试写入）不在此探测——每次启动全量递归对
    # 浏览器数据这类大目录代价过高；此类权限问题由主进程报错并指向 README 迁移说明。
    if [ -n "$(find "${DATA_DIR}" -maxdepth 1 ! -uid "${RUN_UID}" -print -quit 2>/dev/null)" ]; then
        echo "[entrypoint] 数据目录存在非 ${RUN_UID} 属主条目，递归修正（仅 ${DATA_DIR} 内部）"
        chown -R "${RUN_UID}:${RUN_GID}" "${DATA_DIR}" 2>/dev/null || \
            echo "[entrypoint] 警告：数据目录属主修正失败，主进程可能因权限不足启动失败（见 docker/README.md）"
    fi
fi

# 修正权限（挂载卷可能为只读或受限，仅在可写时生效，避免报错退出）
chmod 755 "${DATA_DIR}" 2>/dev/null || true

# 若未显式指定 host/port，使用环境变量默认值
# 由 launcher 的 clap env 已处理，此处仅打印提示
echo "[entrypoint] 数据目录: ${DATA_DIR}"
echo "[entrypoint] 监听: ${CAMPUS_AUTH_HOST:-0.0.0.0}:${CAMPUS_AUTH_PORT:-50721}"
# 管理凭据状态提示（仅提示是否设置，绝不回显值）：非回环绑定（0.0.0.0）时
# 未配置管理凭据会 fail-closed 拒绝启动，此处提前给出三条出路便于排障
if [ -n "${CAMPUS_AUTH_MANAGEMENT_PASSWORD:-}" ]; then
    echo "[entrypoint] 管理凭据: 已设置（CAMPUS_AUTH_MANAGEMENT_PASSWORD）"
else
    echo "[entrypoint] 管理凭据: 未设置（绑定非回环地址时将拒绝启动；请设置 management_password 或 CAMPUS_AUTH_MANAGEMENT_PASSWORD，或显式 CAMPUS_AUTH_ALLOW_UNAUTH_BIND=true 豁免）"
fi

# root 启动：降权后 exec 主进程。降权工具选 setpriv（util-linux 是基础镜像的
# Essential 组成，镜像内自带；gosu/su-exec 均需额外安装）。不用 --reset-env——
# 它会清空 CAMPUS_AUTH_* 环境变量；HOME 已由镜像 ENV 指向数据卷。
# --init-groups 按 /etc/group 初始化附加组；显式清空继承/环境能力位，保证
# exec campus-auth（无文件能力）后进程不带任何能力。
if [ "$(id -u)" = "0" ]; then
    if ! command -v setpriv >/dev/null 2>&1; then
        # 绝不以 root 运行主进程：降权手段缺失时直接失败，由部署侧排查镜像
        echo "[entrypoint] 错误：setpriv 不可用，拒绝以 root 运行主进程（镜像被裁剪了 util-linux？）" >&2
        exit 1
    fi
    echo "[entrypoint] 降权为 campus(${RUN_UID}) 后运行: $*"
    exec setpriv --reuid="${RUN_UID}" --regid="${RUN_GID}" --init-groups \
        --inh-caps=-all --ambient-caps=-all "$@"
fi

# 非 root 启动（--user 1000:1000 / 平台注入任意 uid）：属主修正已在上面跳过，直接 exec
echo "[entrypoint] 启动命令: $*"
exec "$@"
