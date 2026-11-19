# Campus-Auth Docker 部署

> **根 vs docker/ 职责**（`docker compose up` 开箱即用，无需 `-f`）

| 位置 | 文件 | 职责 |
|---|---|---|
| 根 | `Dockerfile` | 多阶段构建（Node 前端 → Rust → Python 3.12-slim 预装 `python_worker`+Chromium，基础镜像 digest pin，主进程经降权以 `campus(1000)` 运行），产物预装 `/app/python_worker` |
| 根 | `docker-compose.yml` | 默认拉取 GHCR 多架构测试镜像并编排（`127.0.0.1:50721→50721`、`VOLUME /data`、`HEALTHCHECK /api/health`、`stop_grace_period: 40s` 容纳 26s 优雅关闭预算、`init: true` 回收浏览器子进程树） |
| 根 | `docker-compose.build.yml` | 源码开发/自定义构建覆盖；叠加后才执行本地 `Dockerfile` 多阶段构建 |
| 根 | `.dockerignore` | 缩小上下文（`target/frontend/node_modules/python_worker/.venv/__pycache__/logs/config` 等），与 `.gitignore` 口径一致 |
| `docker/` | `entrypoint.sh` | 容器入口（建目录、同步 `/opt/campus-auth/resources` 到数据目录、root 启动时修正存量卷属主，再经 `setpriv` 降权 exec 主进程），仅被 `Dockerfile` 引用 |
| `docker/` | `docker-compose.override.example.yml` | 宿主机目录挂载示例（`./data:/data`），需 `docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up` 显式叠加 |

发布流程会在原生 x64 与 ARM64 runner 分别构建镜像，再发布统一的多架构 tag。普通部署不再重复编译 Rust、前端和 Chromium；便携包仍携带源码构建上下文，离线或自定义场景可使用构建覆盖文件。

默认部署与宿主机目录挂载：
```bash
docker compose pull
docker compose up -d
docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up -d
```

## 快速开始

```bash
# 拉取预构建镜像并启动（后台）
docker compose pull
docker compose up -d

# 查看日志
docker compose logs -f

# 查看健康状态
curl http://localhost:50721/api/health
```

Web 控制台：`http://localhost:50721`

> 安全提示：默认端口仅绑定宿主机回环地址。内置 token 用于防止本地网页跨站请求，**不是**远程访问认证；不要将容器端口直接暴露到局域网或公网。如确需远程访问，请在反向代理/VPN 层配置独立认证与 TLS。

## 安全模型（管理凭据与暴露面）

鉴权采用「token + 握手凭据」模型（详见 `docs/guides/user-guide.md` 鉴权段）：全部 `/api/*` 与 `/ws/*` 请求必须携带 token，token 来自以下三种途径之一：

- **回环绑定（桌面版）**：`GET /api/auth/local-token` 直接下发 token，界面打开即用；托盘链接仍以 `#auth=<bootstrap>` fragment 携带握手秘钥作为等价入口；
- **默认 Docker compose**：容器内绑 `0.0.0.0`（端口映射要求），但宿主只发布到 `127.0.0.1`，并通过 `CAMPUS_AUTH_ALLOW_UNAUTH_BIND=true` 显式声明该信任边界——效果同回环：打开即用，无需管理密码。**容器里看不到宿主端口映射，服务端无法自行区分宿主回环与局域网流量，这个声明是部署方的责任**；
- **发布到局域网**：必须删除 `CAMPUS_AUTH_ALLOW_UNAUTH_BIND` 并配置**管理密码**，远程用户在界面输入密码换取 token。

**fail-closed 门禁**：绑定非回环地址且未配置管理密码时启动直接失败（错误信息给出三条出路），除非显式 `--allow-unauthenticated-bind`（env `CAMPUS_AUTH_ALLOW_UNAUTH_BIND=true`）豁免。

设置管理密码的两种方式（CLI/env 覆盖值优先于 settings.json 的 `app.management_password`）：

1. 环境变量 / 启动参数：`CAMPUS_AUTH_MANAGEMENT_PASSWORD`（等价 `--management-password`），docker-compose 示例：
   ```yaml
   services:
     campus-auth:
       environment:
         - CAMPUS_AUTH_MANAGEMENT_PASSWORD=你的管理密码
   ```
2. 前端设置页 / `PATCH /api/config` 写入 `app_settings.management_password`（明文存储，与代理地址同档；运行期修改即时生效，无需重启）。

**如实说明的风险**：`CAMPUS_AUTH_ALLOW_UNAUTH_BIND=true` 等于「谁能连上这个端口，谁就能拿到 token 并调用全部 API（含脚本执行）」——它只适合「宿主只发布到 `127.0.0.1`」的部署。把它和 `0.0.0.0` 端口发布叠加，等于把整个控制面连同脚本执行接口裸奔到局域网。容器内服务默认无 TLS，管理密码与 token 在局域网 HTTP 明文传输中可被同网段嗅探；管理密码明文存于 settings.json。公网/不可信网络请务必经反向代理（TLS）或 VPN 隧道接入，并在代理层配置独立认证。

## 容器运行身份（非 root 降权）

容器遵循「root 启动 + 降权 exec」模式（与 postgres 官方镜像一致）：

- 入口脚本以 root 启动：只有 root 能修正**存量 root 属主命名卷**（历史版本容器全程以 root 运行，卷内文件均属 root）；
- 属主修正完成后立即经 `setpriv` 降权（uid/gid 1000，用户名 `campus`）exec 主进程——**主进程、Python Worker、Chromium 实际均以非 root 运行**，Web API 鉴权瑕疵或 Worker 脚本执行不再直接获得 root 权限；
- 降权工具用 `setpriv`：它是基础镜像 Essential 包 util-linux 的组成部分（`/usr/bin/setpriv`），零额外安装；镜像裁剪导致其缺失时入口直接报错退出，绝不以 root 运行主进程。

### 存量卷迁移（自动）

升级到本版本后首次启动，入口脚本检测到数据目录**第一层**存在非 1000 属主条目时，对 `${CAMPUS_AUTH_BASE_PATH}`（默认 `/data`）做一次递归 chown（严格限定在数据目录内部，失败仅告警不阻断）；之后各层均为 1000，每次启动只做浅层探测。曾以 `--user root` 调试写入的深层文件不在探测范围内，如遇个别权限错误可手动执行一次迁移容器：`docker run --rm -u root -v campus-auth-data:/data <镜像> chown -R 1000:1000 /data`。

### 加固选项

数据卷属主全部为 1000 后，可在 compose 中显式 `user: "1000:1000"`（或 `docker run --user 1000:1000`）——此时入口脚本跳过 chown，容器连 entrypoint 都不以 root 运行。注意宿主机目录挂载（`./data:/data`）时需先 `chown -R 1000:1000 ./data`，否则主进程无法写入。

### 镜像内属主与 HOME

- `ENV HOME=/data/home`：加密密钥 `~/.campus_network_auth`（`config/crypto.rs` 按 HOME 解析）与 uv 缓存随 `/data` 数据卷持久化，容器重建不再丢失；
- `/app/python_worker`、`/ms-playwright`（Chromium）、`/data` 在镜像内已预置为 campus 属主：环境引导自愈路径（清单漂移/探针失败时执行 `uv sync`）、ddddocr 安装/卸载、「安装浏览器」Web API 都需要写入这些目录。

### 升级时迁移加密密钥（旧版 root 容器 → 新版）

旧版密钥存于容器临时层 `/root/.campus_network_auth`，容器重建即失（旧版 pull 升级本就会触发）；新版起密钥随数据卷持久化，一次迁移后不再丢失。升级前从旧容器备份、新版启动后恢复：

```bash
# 1) 旧容器还在时备份密钥（容器名以实际为准）
docker cp campus-auth:/root/.campus_network_auth ./keys-backup

# 2) 新版容器启动后恢复密钥到数据卷
docker cp ./keys-backup/. campus-auth:/data/home/.campus_network_auth/

# 3) docker cp 写入的文件属 root，修正属主后重启生效
docker exec -u root campus-auth chown -R 1000:1000 /data/home/.campus_network_auth
docker restart campus-auth
```

若不迁移：新版首次启动会生成新密钥，旧卷里 `ENC:` 密文对应的方案密码需在界面重新录入。

## 持久化

`docker-compose.yml` 默认使用命名卷 `campus-auth-data` 持久化 `/data`（含 `config/` / `tasks/` / `logs/`，以及 `home/`——加密密钥与 uv 缓存，见「容器运行身份」）。

默认镜像为 `ghcr.io/misyra/campus-auth-rs:prerelease`。需要复现指定版本时可覆盖：

```bash
CAMPUS_AUTH_IMAGE=ghcr.io/misyra/campus-auth-rs:v5.1.0 docker compose up -d
```

宿主机目录挂载（便于备份）：

```bash
mkdir -p ./data
docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up -d
```

## 从源码构建

源码开发或需要自定义镜像时，显式叠加构建覆盖文件：

```bash
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

这条路径会重新编译前端、Rust 主程序并安装 Worker 与 Chromium，因此明显慢于默认的预构建镜像部署。

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `CAMPUS_AUTH_HOST` | `0.0.0.0` | 监听地址（Docker 必须 `0.0.0.0`） |
| `CAMPUS_AUTH_PORT` | `50721` | 监听端口 |
| `CAMPUS_AUTH_BASE_PATH` | `/data` | 数据根目录（容器内） |
| `CAMPUS_AUTH_MANAGEMENT_PASSWORD` | 未设置 | 管理密码（非回环绑定必需，见「安全模型」）；等价 CLI `--management-password`，优先于 settings.json |
| `CAMPUS_AUTH_ALLOW_UNAUTH_BIND` | 未设置 | `true` 显式豁免「非回环绑定无管理凭据拒绝启动」的门禁（强烈不推荐） |
| `RUST_LOG` | `info` | 日志级别 |
| `CAMPUS_AUTH_WORKER_DIR` | 未设置 | Worker 源码路径覆盖；镜像默认自动使用 `/app/python_worker` |

> 更新通道（`stable`/`prerelease`/`all`）与检查开关在 `settings.json` 的 `updater` 节配置（前端设置页），非环境变量；镜像内 `update/last_check.json` 为上次检查落盘状态。

> **容器内不支持应用内自更新**：镜像的 Python Worker 位于 `/app/python_worker`，不在数据目录 `/data` 内，应用内更新器会按「外置布局」拒绝覆盖（`UnsupportedSelfUpdateLayout`）。升级请走部署侧：`docker compose pull && docker compose up -d`。

> **定时自重启与容器重启策略**：配置里的 `app.auto_restart_hours` 默认 24 小时，到期后主进程主动优雅退出。compose 已配 `restart: unless-stopped`，容器会自动拉起；手工 `docker run` 需自行加 `--restart unless-stopped`，否则容器会停在 exited。若不需要，可在设置页把该值改为 0。

CLI 参数优先级高于环境变量：`--host` / `--port` / `--base-path` / `--management-password`。

## 端口与网络

容器需能访问校园网认证门户（captive portal）。若门户在宿主机内网：

- Linux：`network_mode: host`（去掉 `ports` 映射，改用宿主机网络）
- 或确保 `docker` 网桥可路由到网关 IP（`monitor` 的 TCP/HTTP 探测）

```yaml
services:
  campus-auth:
    network_mode: host
```

## 单独 Docker 命令

```bash
docker build -t campus-auth .

# --stop-timeout 覆盖默认 10s，容纳 26s 的优雅关闭预算（否则 Bridge 等待 Worker
# 退出时被 SIGKILL，残留孤儿浏览器进程）
docker run -d \
  --name campus-auth \
  --restart unless-stopped \
  --stop-timeout 40 \
  -p 127.0.0.1:50721:50721 \
  -v campus-auth-data:/data \
  -e CAMPUS_AUTH_HOST=0.0.0.0 \
  campus-auth
```

## 健康检查

镜像内置 `HEALTHCHECK`，`docker ps` 可见 `healthy` 状态：

```bash
docker inspect --format='{{json .State.Health}}' campus-auth | jq
```

或手动：`curl -fsS http://localhost:50721/api/health`

## 常见问题

- **首次拉取较大**：镜像已经包含 Worker 依赖和 Chromium，首次需要下载完整镜像；后续通常只下载变化的层。
- **源码构建慢**：只有显式叠加 `docker-compose.build.yml` 才会本地编译并安装依赖；任一环节失败都会终止构建，不产出 Worker 缺包的半成品镜像。
- **托盘**：Docker 环境自动禁用托盘（`--no-tray`），无需配置。
- **迁移数据**：便携版 `config/` / `tasks/` 直接拷贝到 `./data/`（宿主机挂载）或 `docker cp` 到命名卷；密钥目录一并随迁（对应 `home/.campus_network_auth`），否则 `ENC:` 密文无法解密。
