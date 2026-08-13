# 更新日志

## v5.0.0-alpha.1

Rust 重写版首个迭代，对齐原项目 v4.2.3 功能并修复历史遗留问题。

### 新增功能

- **goto 步骤类型**：执行过程中显式跳转指定 URL，与 `navigate` 共用处理器，支持 `url` / `value` / `wait_until` 字段
- **assert_text 步骤类型**：等待页面出现指定文本（`document.body.innerText` 包含匹配）
- **success_condition 成功判定**：任务声明变量名后，登录成功以 `eval` 步骤 `store_as` 写入的变量真值判定，替代默认网络检测兜底
- **post_login_delay 配置**：登录后等待 portal 生效的延迟（默认 5s，可配置 0-60）
- **worker 停止端点**：`POST /api/worker/stop` 手动关闭浏览器进程，前端新增「浏览器常驻」开关与「立即关闭浏览器」按钮

### 修复

- **Bridge 并发竞态（F1–F4）**：响应解析失败回收在途请求、cancel 通知可靠发送、shutdown 哨兵 id 隔离、主循环退出优雅回收 Worker
- **调度器（F5/F9/F10）**：重载前触发到期任务避免漏触发、浏览器定时任务加超时上限、到期任务并发限制（上限 4）
- **配置读写**：`has_password` 反映密码可解密、monitor 字段不覆盖存储值、密码加密失败显式报错
- **`.gitignore` 误忽略源码**：运行时目录模式未锚定根目录，导致 `src/config`、`src/environment` 两个核心模块从未进入版本控制，已修复并补入
- **更新器**：校验解压产物存在后再写 pending 更新
- **静态资源缓存**：`index.html` 改 `no-store`，避免引用旧 bundle 名导致前端停在旧版本

### 重构

- Python Worker：StepConfig 补齐 `frame` / `char_range` 字段，OCR 支持字符集限制，一次性命令提取 `_cancel_session` 上下文管理器，删除 `script_runner.py` 死代码
- 前端：`useUi` init 防重入、WebSocket 重连全量刷新、`useProfiles` 未保存改动确认、任务列表拖拽排序
