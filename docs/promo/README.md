# 认证喵 · 宣传片放映页

一个用 HTML / CSS / JS / SVG 制作的项目宣传片：9 个镜头、约 1 分 02 秒，直接双击即可自动播放，也能像 PPT 一样手动翻页。

## 内容结构

| 页 | 主题 | 时长 | 传播重点 |
|---|---|---:|---|
| 01 | 开场 | 6.4s | 校园网掉了，它自己连回来 |
| 02 | 全天守护 | 6.5s | 断线 → 匹配 → 登录 → 复检 |
| 03 | 核心特点 | 6.5s | 解压即用 / 共享任务 / 24h 重连 / 低内存 |
| 04 | 控制台 | 7.0s | 状态、日志和历史都看得见 |
| 05 | 共享任务 | 7.0s | GitHub / Gitee 仓库导入与分享适配 |
| 06 | 两种登录方式 | 6.5s | HTTP 直连 + 浏览器自动化 |
| 07 | 低内存 | 6.5s | 常态后台 `< 10 MB`，重活按需启动 |
| 08 | 开箱即用 | 7.0s | 下载、解压、填一次、启动检测 |
| 09 | 收尾 | 9.0s | 官网与 GitHub 地址 |

总时长 62.4 秒，属于 1–3 分钟宣传片范围。每页时长由 `index.html` 的 `data-dur` 控制。

## 怎么用

直接双击 `index.html`。页面使用经典脚本而非 ES module，因此 `file://` 打开也能完整运行。

| 操作 | 效果 |
|---|---|
| 点击画面任意处 | 下一页 |
| `←` `→` / `PageUp` `PageDown` | 上一页 / 下一页 |
| `空格` / `k` | 播放 / 暂停 |
| `Home` / `End` | 第一页 / 最后一页 |
| `f` | 全屏 |
| 点击底部刻度 | 跳到指定页 |
| 地址栏 `#5` | 直接打开第 5 页 |

## 视觉说明

- **统一主题**：全片固定暖白纸张底色 `#F4EFE6`，以柔和蓝灰与琥珀光晕建立层次，不再从白底突然跳到黑底。琥珀黄 `#D79346` 承担品牌强调，低饱和绿色只表达在线与成功。
- **干净背景**：背景不使用方格；只保留暖色渐变、柔和光晕、同心弧与极淡的网络连接线。不同页面只改变这些纹理的密度，不改变曝光基线。
- **每页都有对应实机界面**：用隔离运行目录启动真实 `campus-auth.exe`，再从 `/`、`/settings/monitor`、`/settings/system`、`/tasks`、`/profiles`、`/settings/browser`、`/settings/tasks` 与 `/about` 等实际路由逐页截图。9 个镜头不再重复使用同一张仪表盘图。
- **截图承担叙事**：全天守护页使用网络检测设置，核心特点页使用系统设置与网络更新，仓库页使用浏览器任务列表，双引擎页使用方案认证设置与浏览器设置，低内存页使用任务与环境，开箱页使用方案顶部，收尾页使用关于 / 任务 / 方案产品矩阵。
- **镜头运动**：产品截图只做 2%–7% 的缓慢推近；断线重连时间线、内存短柱和双引擎切换承担信息动画，其他元素只做一次入场。
- **减少动态效果**：系统开启 `prefers-reduced-motion` 时，推近、脉冲和循环动画全部关闭。

## 文件结构

```text
docs/promo/
├── index.html          # 9 页放映页
├── script.html         # 对应分镜与旁白稿
├── css/
│   ├── tokens.css      # 统一色板、字体与阴影
│   ├── stage.css       # 固定舞台、进度与控件
│   ├── backdrop.css    # 背景气候
│   ├── slides.css      # 基础组件
│   ├── motion.css      # 入场动效
│   └── campaign.css    # 本版宣传片的 9 页构图
└── js/
    ├── backdrop.js     # 网络节点与数据包背景
    ├── fx.js           # 计数、逐条入场
    └── deck.js         # 播放、翻页、键盘与缩放
```

页面额外引用：

- `docs/assets/logo.png`
- `docs/assets/preview-dashboard.webp`
- `docs/assets/promo-dashboard-live.png`
- `docs/assets/promo-monitor-settings.png`
- `docs/assets/promo-system-settings.png`
- `docs/assets/promo-network-settings.png`
- `docs/assets/promo-tasks-browser.png`
- `docs/assets/promo-repo-browser.png`（用户提供的云端仓库导入界面）
- `docs/assets/promo-direct-request.png`（用户提供的直连请求配置界面）
- `docs/assets/promo-profiles-auth.png`
- `docs/assets/promo-profiles-live.png`
- `docs/assets/promo-browser-settings.png`
- `docs/assets/promo-environment-settings.png`
- `docs/assets/promo-about.png`

## 录成视频

1. 用 Chromium / Edge 打开 `index.html`，按 `f` 全屏。
2. 以 1600×900 或 1920×1080 录制自动播放，完整成片约 62 秒。
3. 需要纯画面时，可在后期裁掉设计稿底部约 118px 的进度轨与控制栏。
4. 字体从 jsDelivr 按需加载；正式录制前先打开一次并确认字体加载完成。断网时会回落系统字体，不会白屏。

## 关键数据口径

- 常态后台 `< 10 MB` 是保守上界；页面里的 `5.8 MB / CPU 0%` 来自 Windows 任务管理器实测。
- 浏览器与 OCR 仅在需要登录时按需启动，空闲超时后关闭。
- 任务仓库提供 GitHub 与 Gitee 来源；远程任务由社区成员分享，导入前仍应核对内容。
- 凭据加密保存在本机，不随方案导出。
