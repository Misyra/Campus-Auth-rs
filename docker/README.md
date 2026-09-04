# Campus-Auth Docker 部署

> **根 vs docker/ 职责**（`docker compose up` 开箱即用，无需 `-f`）

| 位置 | 文件 | 职责 |
|---|---|---|
| 根 | `Dockerfile` | 多阶段构建（Node 前端 → Rust → Python 3.12-slim 预装 `python_worker`+Chromium），`COPY python_worker` 与 `.dockerignore: python_worker/.venv` 联动，产物预装 `/app/python_worker` |
| 根 | `docker-compose.yml` | 默认编排（`campus-auth:50721→50721`、`VOLUME /data`、`CAMPUS_AUTH_BASE_PATH=/data`、`HEALTHCHECK /api/health`），`docker compose up -d` 直接发现 |
| 根 | `.dockerignore` | 缩小上下文（`target/frontend/node_modules/python_worker/.venv/__pycache__/logs/config` 等），与 `.gitignore` 口径一致 |
| `docker/` | `entrypoint.sh` | 容器入口（`mkdir -p $DATA_DIR/{config,tasks,logs,environment}` 后 `exec campus-auth`），仅被 `Dockerfile` 引用 |
| `docker/` | `docker-compose.override.example.yml` | 宿主机目录挂载示例（`./data:/data`），需 `docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up` 显式叠加 |

两条等价启动：
```bash
docker compose up -d --build                # 命名卷 campus-auth-data:/data
docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up -d --build  # 宿主机 ./data:/data
```

## 快速开始
# 构建并启动（后台）
docker compose up -d --build

# 查看日志
docker compose logs -f

# 查看健康状态
curl http://localhost:50721/api/health
```

Web 控制台：`http://localhost:50721`

## 持久化

`docker-compose.yml` 默认使用命名卷 `campus-auth-data` 持久化 `/data`（含 `config/` / `tasks/` / `logs/`）。

宿主机目录挂载（便于备份）：

```bash
mkdir -p ./data
docker compose -f docker-compose.yml -f docker/docker-compose.override.example.yml up -d --build
```

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `CAMPUS_AUTH_HOST` | `0.0.0.0` | 监听地址（Docker 必须 `0.0.0.0`） |
| `CAMPUS_AUTH_PORT` | `50721` | 监听端口 |
| `CAMPUS_AUTH_BASE_PATH` | `/data` | 数据根目录（容器内） |
| `RUST_LOG` | `info` | 日志级别 |
| `CAMPUS_AUTH_WORKER_DIR` | `/app/python_worker` | Worker 源码路径覆盖 |

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
  -p 50721:50721 \
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

- **首次启动慢**：若构建时未预装 Chromium，首次触发登录时会自动下载（需外网）。建议构建时保持网络畅通，使 `playwright install --with-deps chromium` 成功。
- **托盘**：Docker 环境自动禁用托盘（`--no-tray`），无需配置。
- **迁移数据**：便携版 `config/` / `tasks/` 直接拷贝到 `./data/`（宿主机挂载）或 `docker cp` 到命名卷。
