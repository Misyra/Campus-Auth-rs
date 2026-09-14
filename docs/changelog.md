# 更改日志

> 本文件记录每一次代码、配置、接口与文档更改，供开发和问题追溯；面向用户的版本更新摘要见 `docs/updatelog.md`。历史轮次继续保留于本文件，过时规划见 `docs/archive/`，活跃计划见 `docs/plan-next.md` + `docs/known-issues.md`。最新活跃为“v5.0.0-alpha.10”。

## 开发中（2026-09-14 登录方式 UI 提取为可复用组件）

- 新增 `components/common/LoginChannelField.vue`：把「登录方式切换 + 直连请求参数（方法/地址/请求头/请求体/成功失败关键字/凭据变换脚本）+ 发送测试请求与结果面板」从方案编辑器抽出为可复用组件，以 `v-model` 绑定宿主草稿对象（原地修改，宿主脏检测按全量序列化比对可直接感知）。此前该 UI 硬编码在 `ProfilesView` 编辑器内，设置页等其他入口无法切换登录方式。
- 测试结果改为组件自持局部 ref：`useProfiles.testHttpLogin` 由「读写单例 ref」改为无状态函数（接收参数、返回报告），同一页面多实例各自展示结果、互不覆盖；`httpTestRunning` 保留为全局单飞门（后端本就单飞）。
- 新增 `utils/loginChannel.ts`：渠道标签与直连 outcome 文案的单一事实源（含 `HTTP_METHOD_OPTIONS`），避免同一枚举在多处各写一套中文标签而漂移；补纯函数单测。
- 设置页「账号」Tab 新增「登录方式」卡（复用同一组件），与方案编辑器两处均可切换；配套扩展 `GET/PATCH /api/config` 扁平响应与 Profile 域白名单，携带并接收 8 个直连字段（`http_method/http_url/http_headers/http_body/http_success_pattern/http_failure_pattern/http_crypto_script`），`http_url` 走与方案接口同一校验口径（空串=未配置放行，非空须为合法 http/https），枚举字段非法值显式 400。
- 前端 `CredentialsConfig` / `ConfigResponse` / `SaveConfigPayload` 同步直连字段；`useConfig` 据此纳入表单状态（设置页可编辑，故随保存载荷提交），取代上一轮的只读 `activeLoginChannel` 方案。
- 样式随组件迁移：`pages/profiles.css` 中 `.http-*` / `.channel-note` / `.profile-channel-switch` 等规则移入组件 scoped 块（该组件已跨页面复用，页面级 CSS 无法覆盖另一页面）。

