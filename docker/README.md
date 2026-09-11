# Campus-Auth Docker 部署

> **根 vs docker/ 职责**（`docker compose up` 开箱即用，无需 `-f`）

| 位置 | 文件 | 职责 |
|---|---|---|
| 根 | `Dockerfile` | 多阶段构建（Node 前端 → Rust → Python 3.12-slim 预装 `python_worker`+Chromium），`COPY python_worker` 与 `.dockerignore: python_worker/.venv` 联动，产物预装 `/app/python_worker` |
| 根 | `docker-compose.yml` | 默认拉取 GHCR 多架构测试镜像并编排（`127.0.0.1:50721→50721`、`VOLUME /data`、`HEALTHCHECK /api/health`） |
| 根 | `docker-compose.build.yml` | 源码开发/自定义构建覆盖；叠加后才执行本地 `Dockerfile` 多阶段构建 |
| 根 | `.dockerignore` | 缩小上下文（`target/frontend/node_modules/python_worker/.venv/__pycache__/logs/config` 等），与 `.gitignore` 口径一致 |
| `docker/` | `entrypoint.sh` | 容器入口（`mkdir -p $DATA_DIR/{config,tasks,logs,environment}` 后 `exec campus-auth`），仅被 `Dockerfile` 引用 |
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

## 持久化

`docker-compose.yml` 默认使用命名卷 `campus-auth-data` 持久化 `/data`（含 `config/` / `tasks/` / `logs/`）。

默认镜像为 `ghcr.io/misyra/campus-auth-rs:prerelease`。需要复现指定版本时可覆盖：

```bash
CAMPUS_AUTH_IMAGE=ghcr.io/misyra/campus-auth-rs:v5.0.0-alpha.10 docker compose up -d
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
| `RUST_LOG` | `info` | 日志级别 |
| `CAMPUS_AUTH_WORKER_DIR` | 未设置 | Worker 源码路径覆盖；镜像默认自动使用 `/app/python_worker` |

> 更新通道（`stable`/`prerelease`/`all`）与检查开关在 `settings.json` 的 `updater` 节配置（前端设置页），非环境变量；镜像内 `update/last_check.json` 为上次检查落盘状态。

CLI 参数优先级高于环境变量：`--host` / `--port` / `--base-path`。

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

docker run -d \
  --name campus-auth \
  -p 127.0.0.1:50721:50721 \
  -v campus-auth-data:/data \
  -e CAMPUS_AUTH_HOST=0.0.0.0 \
  --restart unless-stopped \
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
- **迁移数据**：便携版 `config/` / `tasks/` 直接拷贝到 `./data/`（宿主机挂载）或 `docker cp` 到命名卷。
