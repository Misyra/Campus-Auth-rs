# 缺陷清单独立复核报告（v2 终版）

- 日期：2026-09-06
- 对象：76 条审查清单（批次①~④ + P3）
- 方法：5 组并行源码取证 → 主代理逐行复核 → **v1 独立复核 → 双方争议条目逐行对质**（本文档为对质后的 v2 终版）
- 结论口径：`属实` / `部分成立（定性收敛为优化建议/体验项）` / `不成立`

## 〇、v2 修订说明

v1 曾将 21 条判为「不成立」。经第二轮逐行对质，其中 **14 条反驳站不住**（反驳错位 / 搜索关键词失误 / 反证自相矛盾 / 遗漏代码路径），已恢复为成立并附对质证据（见 §四）；另有 2 处 v1 抓到主复核的失误予以采纳（§五）。批次①（9 条 P0/P1）与批次②主体两侧结论一致，无争议。

## 一、总体结论（v2）

| 判定 | 数量 | 说明 |
|------|------|------|
| 属实 | 59 | 含机制/位置/严重度微调 |
| 部分成立（定性收敛） | 8 | 行为真实，但应归「优化建议/体验项/设计确认」而非缺陷 |
| 不成立（终版） | 9 | 见 §六，勿据以整改 |

**批次① 9 条 P0/P1 全部属实**，可放心作为止血依据。问题集中在 P2/P3 的契约漂移与守卫覆盖不全。

---

## 二、批次① P0/P1（9 条，全部属实，建议优先修）

| # | 条目 | 判定 | 关键证据 |
|---|------|------|---------|
| 1 | 登录会话 panic → `await_result` 永挂 → 自动登录静默失效 | 属实 | `src/login/mod.rs:733-744` spawn 无 panic 保护（notify/清槽位均会被跳过，JoinHandle drop 吞 panic）；handle 自持 `LoginHandleInner { result_tx }`，channel 永不关闭 → `wait_for` 永挂；`src/engine/run_loop.rs:645,652` `auto_login_in_flight` 恒 true |
| 2 | Worker exit 0 自退 → Bridge 假 Ready 变砖 | 属实 | `src/bridge/mod.rs:1417-1426` 无 `process.take()` / 不清 `current_session`/`debug_session_open`/cancel_registry；对比崩溃分支 `:1446-1481` 完整清理；`is_worker_ready:1204` 恒 true → 快速路径写死管道 |
| 3 | 并发命令覆盖槽位并删除在途登录取消令牌 | 行为属实（supersede 为有意设计，但在途登录不可取消是真实后果） | `src/bridge/mod.rs:923-927` `cancel_registry.remove(&old_cancel_id)`；`check_session_compat:1526-1528` InLogin 允许新命令插入。建议：多活跃 token（按 request_id 归属）或显式拒绝 |
| 4 | 录制器多 frame 共用存储 key 互相覆盖/清空 | 属实 | `task-recorder.user.js` 无 `@noframes`（注入所有 frame）；`saveState:188` 单 key 全量覆写；`loadState:206-207` URL 不匹配即 `clearSavedState()` 删共享存储。跨域 iframe 录制实际不可用 |
| 5 | `POST /api/tasks` browser 分支 100% 失败 | 属实 | `src/web/routes/tasks.rs:66-70` `..Default::default()` → steps 空；`src/tasks/loader.rs:470` 「steps 不能为空」 |
| 6 | 前端 `pythonNotReady` 未定义，横幅永不渲染 | 属实 | `BrowserSettings.vue:123` 模板引用，脚本区零定义（重命名残留） |
| 7 | 仓库导入/模板加载绕过 dirty 确认覆盖草稿 | 属实 | `useRepoImport.ts:105` 直接 `setTaskDraft`；`useTasks.ts:298` 直接覆写 json/name/description；`confirmDiscardTaskIfDirty`（`useTasks.ts:136`）两处均未调用 |
| 8 | unix `is_own_process` 漏 return → `--force` 失效 | 属实 | `src/utils/lock.rs:203` `s.contains("campus-auth");` 分号结尾丢弃，末语句 `false` 生效 |
| 9 | 密钥首次生成双写竞态 → 密码静默丢失 | 属实 | `src/config/crypto.rs:246-270` try_lock 失败降级并发生成；`:229` `let _ = set()` 丢败者 → 磁盘/内存密钥可能不一致 |

## 三、批次② 契约对齐（8 条全部属实）

| 条目 | 判定 | 证据 |
|------|------|------|
| wait 三方矛盾（比原描述更严重） | 属实 | `loader.rs:489` 把 `wait` 归入必填 selector 组；`prompt.rs:47` 与 `step_handlers.py:676-685` 均支持无 selector 休眠 → AI 生成的休眠步骤被 Rust 校验直接拒绝，自纠轮同败 |
| browser 任务 timeout 无钳制 | 属实（毫秒口径） | `executor.rs:187` `Duration::from_millis(cfg.timeout.max(1))`：0 → 1ms 永超时；86400000 → 会话槽位占 24h，期间所有浏览器任务/登录被拒 |
| select 空 value 静默 no-op | 属实 | `step_handlers.py:536-538` `if not value: return`，步骤「成功」但未操作 |
| LLM 不查 `finish_reason` + `max_tokens` 8192 + 传输零重试 | 属实 | `llm.rs:35` 硬编码；`parse_chat_response:74-102` 无 finish_reason 分支；`generate.rs:84-86` chat Err 直接 `?` 终止 |
| evaluate 超时/取消关闭共享页 | 属实 | `step_handlers.py:762-768` `page.close()`；`playwright_worker.py:928` 会话全程复用 `self._page` → 后续步骤（含失败截图）全灭 |
| 步骤别名漂移 | 属实 | `models.rs:35-52` 无 `evaluate`/`custom`；`step_handlers.py:904-907` 两个别名均在 → 录进去 Rust 侧保存不了 |
| 更新后不触发 `uv sync` | 属实 | `python.rs:64-70` 解释器完好即快速返回；`helper_main.rs:276` overlay 覆盖 pyproject/uv.lock → 新增依赖对已装用户不生效 |
| Esc 双重处理脏状态（v2 恢复成立） | 属实 | 弹窗打开路径（`handleElementSelected:1580` → `showCustomStepModal:2102`）**不置** `state.recording=false`（仅 onSubmit 成功后 :2133 置 false）→ 打开时 recording=true；全局 onKeyDown（先注册 :3089）Esc 分支清 `currentStepType` 并 `stopPropagation`——**stopPropagation 挡不住同节点后注册的 modal onKey**（:2035-2043，需 stopImmediatePropagation）→ `defaultCancel` 复位 recording=true。终态 recording=true + currentStepType=null → 下次点击 `showCustomStepModal(null,…)` 弹「📝 null」 |

---

## 四、v1 误判修正（14 条，经对质恢复成立）

| v1 判定 | 对质证据（恢复成立） |
|---------|---------------------|
| Esc 脏状态「不成立」 | 见 §三末行，完整证据链闭合 |
| Auto 落盘「行为不存在」 | grep 关键词与条目无关。`login/mod.rs:958-961` `modify_settings(\|s\| s.global.browser.browser_channel = next)` 实证，每次 submit 预检触发（:427 注释「落盘持久化」） |
| 错误映射 500「不成立」 | 反驳的是 `ApiError::status()`（枚举内映射，无问题）；原条目是 `From<UpdaterError> for ApiError` **恒 `Internal`**（`error.rs:241-245`）→ `UpdateInProgress`/`LoginInProgress` 业务冲突返回 500 |
| 404 uniform「不成立」 | 原条目在 handler 层：`execute_task` 把 `load_task` 的所有 TaskError 映射 NotFound（`routes/tasks.rs` 实证） |
| 凭证非原子「已修复」 | `modify_settings_tx` 只解决全局设置单段 TOCTOU；原条目是**两段写入半成功**：`save_profile`（:197）在事务之前落盘，事务失败时凭证已写 |
| pathSegment「确有 encode」 | 误读命题。函数存在且 encode ✓，但 `scheduledTasksApi` 六方法裸拼 `${id}` 未走它（`index.ts:293-299`） |
| notifier「不成立」 | 反证自相矛盾：文档自己引用「scan_count 仅 `on_profile_switch` 重置」——这正是原条目主张（切换后归零 → 首次失败通知被抑制，`notifier.rs:56-58`）。机制属实 |
| metrics CAS「注释与实现一致」 | `compare_exchange` 只护 `probe_total`；`avg` 是 CAS 成功后的普通 `store`（`metrics.rs:79-86`）→ 注释「保证 total 与 avg 原子性」言过其实，存在偏差窗口 |
| OCR 安装轮询「不成立」 | `TasksSettings.vue:67-79` 是 async while + setTimeout 链（非 setInterval），无卸载取消，离开页面继续最长 5 分钟 |
| 账号快照「无法定位」 | 搜索词问题。`AccountSettings.vue:24-27`：`currentProfileName = ref("")` + onMounted 一次性拷贝，直链进入永久显示 fallback |
| ipconfig「已双语」 | 反驳错位：原命题是**非中英文**语言系统（法/德/日）解析为空，中英双语覆盖不了（`detect.rs:146-150` 双语属实）。降 P3 |
| WS 截图「前端不成立」 | 误归档：原条目即后端条目，v1 自己承认「后端 `prepare_bridge_event` 确实每连接独立读盘+编码」——实质属实 |
| logging「每连接重复初始化」/ pause「无快照」/ 3s「配置驱动」 | 反驳空炮（三个命题原清单均未主张）。原条目分别为：时间戳双格式（`logging.rs:315` RFC3339 vs 文件层）、`run_loop.rs:756` 用 `manual_paused` 忽略定时窗口、`INTERFACE_CHECK_TIMEOUT=3s` 硬编码（`monitor/mod.rs:34`，settings.json 的 10s/5s 是 tcp/http 探测超时） |
| browser.rs「无缺陷」 | 漏看：custom 分支只查 `Path::exists()` 不查 `is_file()`（`browser.rs:156-159`），目录也算「可用」 |
| 录制器 version「两点均错」 | 前半采纳（是字面量 `const VERSION = "4.2.1"`，:22）；后半不成立：Python extras 只在 Python 侧，任务保存链路走 Rust serde，`TaskConfig` 字段封闭无顶层 extras → `version` 保存时被丢弃，实质成立 |

## 五、v1 采纳的主复核修正（2 处）

1. `useProfiles.ts`「零 busy」表述错误：`busy` 在 :29/:251/:260 存在（`busy[busyKey]`，用于检测）。核心条目不变：`saveProfile` 无连点守卫。
2. 录制器 VERSION 是字面量定义而非「常量引用」（见 §四末行）。

## 六、不成立（终版，9 条，勿据以整改）

| 条目 | 结论 |
|------|------|
| OCR 加载超时重复建模、内存翻倍 | **撤销正确**。`_ocr_lock = threading.Lock()`，构建与缓存写入同一临界区，超时后并发调用在锁上排队，只建一个实例 |
| 三处固定 `time.sleep`（阻塞事件循环指控） | 不存在阻塞，全是 asyncio.sleep / 可取消封装 |
| 后端 cron 解析回落 8:00 | 后端无此逻辑（`cron_loop.rs` 解析失败记 `invalid_cron_ids`）；**前端属实**（见 §七） |
| 鉴权豁免列表多排路由 | 豁免项逐行有注释，无危险接口（debug 截图免鉴权有 `<img>` 豁免理由 + 路径穿越防护，仅与 ws 侧 magic bytes 口径不一致，P3） |
| `check_interval_hours=0` 永久挂死（缺陷定性） | 0=禁用是注释明示设计；但热改回 24h 需重启（v1 也承认）→ 收敛为 P3 体验项 |
| 会话级 `playwright.stop()`（缺陷定性） | docstring 明确的有意会话级生命周期 → 归「优化建议」（context 复用），非缺陷 |
| AI key 无 return（硬缺陷定性） | 文案「部分服务商要求 API Key」属有意软提示 → 收敛为 P3 体验项 |
| 事件循环内固定等待（「阻塞」定性） | 非 bug；「延迟税」优化建议有效（`step_delay 0.5s` / `navigation_wait 1.0s` / `select_delay 500ms` 默认值实证） |
| 配置保存 TOCTOU（单段读改写） | `modify_settings_tx` 已修复；遗留的是两段半成功问题（§四） |

## 七、部分成立（定性收敛，8 条摘要）

- 探测失败丢 pending：真实但轻微（`probe_pending` 已复位，仅丢「立即补发」语义，周期定时器补偿）→ P2 边缘
- `close_browser` 双端 8s 竞速：属实但 Rust 侧仅 warn 不阻塞收尾 → P2（日志误报）
- debug_start 无取消检查：步骤层（`step_handlers.py:533/754`）有取消检查，start 导航窗口（`playwright_worker.py:1105-1146`）确无 → 保留
- 调试会话无 TTL：属实（`debug_guard_cleanup` 注释自认不回收）→ P2
- debug 截图免鉴权：免鉴权属实 + 防护存在，仅口径不一致 → P3
- 定时任务 Shell 类型静默失效：属实且比预期严重（`ScheduledTasksView.vue:21/103` 暴露选项，`:108` 对 shell 仍只列 browser 目标 → 后端推导 browser）→ P1
- 录制器 XPath：缺 `/html/body` 前缀（绝对路径不可执行）+ 位置索引脆弱 → P2/P3
- `useProfiles` busy：表述修正后核心条目（保存无防连点）成立

## 八、建议圈选范围（v2）

**必修（P0）**：§二 9 条全部。
**强烈建议（P1）**：wait 三方矛盾 · timeout 钳制 · evaluate 关页 · 别名漂移 · select 空值 · LLM 三件套 · resync · 500/404 映射 · Shell 类型 · Esc 脏状态 · 前端保存防连点/validateConfig/cron。
**可选（P2/P3）**：性能组（context 复用、同步 IO、固定 sleep、WS 截图、健康检查缓存）、体验组（post_login_delay clamp、pause 快照、Auto 落盘、OCR 轮询、账号快照、拖拽）、质量组（dirty-draft 提取、schema 表驱动、404 映射、死状态清理等）。
**功能升级（单独立项）**：HTTP 直连快路径 · 302 学习 auth_url · 录制器 iframe 支持 · step_result 事件。

## 九、复核流程改进（v1 §7 采纳 + 补充）

1. P3 条目必须带可定位的文件:行号。
2. 区分「缺陷」与「有意设计」：先读注释与 docstring 再定性。
3. 跨端定位要标清（Rust / Python Worker / Vue 三端）。
4. 并发条目需确认锁类型与临界区范围（OCR 条目教训）。
5. **（v2 补充）反驳「不成立」前必须验证状态时序与调用路径**（Esc 条目教训：v1 未验证弹窗打开时 `state.recording` 的值，仅凭互斥假设定性）。
6. **（v2 补充）反驳搜索要用条目自身的标识符**（Auto 落盘、账号快照、interpolation 三条均因搜索词与条目无关而误判「不存在/无法定位」）。
