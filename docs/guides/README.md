# 指南索引

> 本目录为面向用户的操作指南（与 `docs/changelog.md` / `docs/known-issues.md` / `docs/plan-next.md` 的面向开发者文档区分）。版本更新摘要单独维护在 `docs/updatelog.md`。

## 入门

- [README](../../README.md) — 项目概述、特性、快速开始（含 Docker）
- [用户指南](user-guide.md) — 启动参数、运行时目录、Web 控制台、Profile、任务、录制器、OCR、托盘、更新通道与手动放置安装包、FAQ

## 功能指南

- [直连请求登录使用指南](http-login-guide.md) — 免 Python / 浏览器的 HTTP 直连登录：怎么抓门户请求、占位符、成功/失败判定、凭据变换脚本、常见问题（软件内「直连配置向导」是本指南的分步版本）
- [任务使用手册](task-manual.md) — 日常管理、录制器、调试、API 一览
- [任务编写指南](task-writing-guide.md) — 浏览器任务 JSON 详解（步骤类型、变量、frame、success_condition、选择器）
- [自定义脚本指南](custom-script-guide.md) — `script` 任务（`tasks/scripts/`）与 `POST /api/scripts/run` 直跑

## 相关链接

- [更新日志](../updatelog.md) — 面向用户的版本更新摘要
- [更改日志](../changelog.md) — 每项开发变动与问题修复记录
- [已知问题](../known-issues.md) — 仍有效的未修复项
- [后续计划](../plan-next.md) — 活跃计划入口
- [架构与开发规范](../../AGENTS.md) — ServiceContainer、Bridge、Updater 通道等
