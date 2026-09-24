//! 自定义脚本登录渠道：把登录动作整个交给方案绑定的脚本任务
//!
//! 与直连渠道（[`crate::login::http_login`]）同构：都在 Rust 进程内完成，不经
//! Bridge/Worker，因此**不要求 Python 环境与浏览器就绪**，也不占用浏览器会话槽位。
//! 差别在"谁来登录"——直连用内置的请求构造逻辑，本模块把决定权交给用户写的脚本。
//!
//! # 脚本契约（写入 `docs/guides/custom-script-guide.md`，改这里必须同步改那里）
//!
//! - **凭据经环境变量下发**。脚本任务本身不做模板替换（`{{USERNAME}}` 那一套是浏览器
//!   任务专属，由 Worker 的变量解析器处理），所以登录渠道另行注入 [`LOGIN_ENV_KEYS`]：
//!   [`ENV_USERNAME`] / [`ENV_PASSWORD`] / [`ENV_ISP`] / [`ENV_AUTH_URL`]。变量在最小
//!   环境变量之上叠加，主进程的 Web token、代理密码等**仍然不继承**（`env_clear` 语义
//!   不变，见 `TaskExecutor::execute_script_with_env`）。
//! - **成败按子进程退出码**：`0` = 脚本自称成功，随后仍走登录后网络验证兜底（与浏览器
//!   渠道一致，避免"脚本说成功但实际没登上"被误报成功）；非 `0` = 本次尝试失败，按
//!   方案的重试策略重试（与直连「未命中成功标识」同属可重试），重试预算耗尽才判终态失败。
//! - **超时取脚本任务自己的 `timeout`**（1~3600 秒，钳制），超时按平台强杀整棵进程树。
//! - 脚本正文、解释器、参数、工作目录全部来自脚本任务（`tasks/scripts/<id>.json`）——
//!   与任务页的「立即运行」是同一条执行路径（[`TaskExecutor::execute_script_with_env`]），
//!   不另起一套实现，否则两条路径的行为迟早分叉。
//! - 脚本 `stdout` 会进入登录历史消息（截断后），故**密码会被从输出里抹掉**
//!   （见 [`redact_secret`]）：脚本打印含凭据的 URL 是常见写法，历史要落盘。

use std::sync::Arc;

use serde_json::json;

use crate::bridge::{Outcome, StructuredResult};
use crate::config::runtime::ProfileSnapshot;
use crate::tasks::{ScriptRunnerApi, ScriptTaskConfig, TaskError};

/// 注入给登录脚本的环境变量名（改这里必须同步用户指南）
pub(crate) const ENV_USERNAME: &str = "CAMPUS_USERNAME";
/// 当前方案密码（明文；脚本跑在用户自己的机器上，与配置文件同信任级）
pub(crate) const ENV_PASSWORD: &str = "CAMPUS_PASSWORD";
/// 当前方案运营商
pub(crate) const ENV_ISP: &str = "CAMPUS_ISP";
/// 当前方案认证地址（可为空串：脚本渠道不要求填它）
pub(crate) const ENV_AUTH_URL: &str = "CAMPUS_AUTH_URL";

/// 契约里承诺的全部变量名（顺序即文档顺序）。
///
/// 只用于测试对账：生产路径由 [`login_env`] 显式列出四个键，本列表把它声明成"契约"
/// 并钉住（改名 / 增删 / 换顺序都会让 `login_env_exposes_documented_keys` 失败，
/// 而那时指南与前端帮助文案必须同步改）。
#[cfg(test)]
pub(crate) const LOGIN_ENV_KEYS: [&str; 4] = [ENV_USERNAME, ENV_PASSWORD, ENV_ISP, ENV_AUTH_URL];

/// 进入登录历史消息的输出上限（字符数）。
///
/// 历史会落盘并展示在界面列表里，整段 stdout 放进去既撑爆消息也淹没关键行；只留尾部
/// ——脚本的报错通常写在最后几行（`TaskExecutor` 已把合并输出截到 500 字符）。
const MESSAGE_OUTPUT_LIMIT: usize = 200;

/// 一次脚本登录尝试的执行计划（由登录编排器构造，随会话参数传入）
///
/// `Clone` 是会话参数的需要（每次尝试 clone 一份，重试时重跑同一个脚本）。
#[derive(Clone)]
pub struct ScriptLoginPlan {
    /// 方案绑定的脚本任务（加载时已按类型校验）
    pub task: ScriptTaskConfig,
    /// 注入给脚本的凭据环境变量（**含明文密码**，故不进日志、不参与 Debug 派生）
    pub extra_env: Vec<(String, String)>,
}

impl std::fmt::Debug for ScriptLoginPlan {
    /// 手写 Debug：环境变量里躺着明文密码，派生的 Debug 会在任何一次
    /// `tracing::debug!("{plan:?}")` 里把它打进日志（与 `ProfileSnapshot` 同款处理）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.extra_env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("ScriptLoginPlan")
            .field("task_id", &self.task.common.task_id)
            .field("env_keys", &keys)
            .finish()
    }
}

/// 构造注入给登录脚本的环境变量（纯函数，便于单测）。
///
/// 值一律为字符串（空值也照样注入）：脚本据此可区分"方案没填"与"变量不存在"，
/// 不必在脚本里到处写 `os.environ.get(...) or ""`。
pub(crate) fn login_env(profile: &ProfileSnapshot) -> Vec<(String, String)> {
    vec![
        (ENV_USERNAME.to_string(), profile.username.clone()),
        (ENV_PASSWORD.to_string(), profile.password.to_string()),
        (ENV_ISP.to_string(), profile.isp.clone()),
        (ENV_AUTH_URL.to_string(), profile.auth_url.clone()),
    ]
}

/// 执行一次脚本登录尝试（不发网络验证，验证由会话状态机负责，与直连一致）。
pub(crate) async fn run_once(
    runner: &Arc<dyn ScriptRunnerApi>,
    plan: &ScriptLoginPlan,
) -> StructuredResult {
    let task_id = plan.task.common.task_id.clone();
    tracing::info!(task_id = %task_id, "脚本登录：执行登录脚本");

    match runner
        .run_script_with_env(&plan.task, plan.extra_env.clone())
        .await
    {
        Ok(result) => {
            let secret = plan
                .extra_env
                .iter()
                .find(|(k, _)| k == ENV_PASSWORD)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            to_structured(&task_id, &result, secret)
        }
        Err(e) => {
            // 连"跑起来"都没做到：缺脚本文件、扩展名不支持、解释器缺失、spawn 失败
            // 属配置/环境问题，重试不会变好 → 终态失败；超时则是一次性波动，可重试
            let (outcome, message) = match e {
                TaskError::ExecutionTimeout(secs) => (
                    Outcome::NavigationTimeout,
                    format!("登录脚本 {task_id} 执行超时（{secs} 秒）"),
                ),
                _ => (
                    Outcome::UnknownError,
                    format!("登录脚本 {task_id} 无法执行: {e}"),
                ),
            };
            tracing::warn!(task_id = %task_id, outcome = ?outcome, "脚本登录执行失败");
            StructuredResult {
                outcome,
                message,
                data: json!({ "script_task": task_id }),
                screenshot_url: None,
                duration_ms: 0,
            }
        }
    }
}

/// 把脚本执行结果映射为登录尝试结果（纯函数，便于单测）。
///
/// `secret` 是本次尝试注入的密码：输出里出现它一律替换为 `***` 后才进消息。
pub(crate) fn to_structured(
    task_id: &str,
    result: &crate::tasks::TaskResult,
    secret: &str,
) -> StructuredResult {
    let output = redact_secret(&result.output, secret);
    let tail = tail_snippet(&output);
    let detail = if tail.is_empty() {
        String::new()
    } else {
        format!("：{tail}")
    };
    let (outcome, message) = if result.success {
        (
            Outcome::Success,
            format!("登录脚本 {task_id} 退出码 0{detail}"),
        )
    } else {
        // 与直连「未命中成功标识」同类：本次尝试失败但成因未知（网络/门户改版/凭据），
        // 交重试预算处理，不直接判死，也不回收 Worker（脚本渠道本就没有 Worker）
        (
            Outcome::AssertionFailed,
            format!("登录脚本 {task_id} 退出码 {}{detail}", result.exit_code),
        )
    };
    tracing::info!(
        task_id = %task_id,
        exit_code = result.exit_code,
        duration_ms = result.duration_ms,
        outcome = ?outcome,
        "脚本登录执行完成"
    );
    StructuredResult {
        outcome,
        message,
        data: json!({
            "script_task": task_id,
            "script_exit_code": result.exit_code,
        }),
        screenshot_url: None,
        duration_ms: result.duration_ms,
    }
}

/// 短到可能到处命中子串的密码（低于此长度只按**词边界**替换，见下）
///
/// 真机实测踩过一次：用一位密码的验证方案跑通脚本渠道后，脚本打印的 `isp=` 被替换成
/// `is***=`——`replace` 是纯子串替换，密码里的每个字符都会命中无关字段名。这属于
/// "脱敏把消息改烂"，用户看到的失败原因连字段名都不再是原样。
///
/// 判据用**词边界**而不是长度阈值：`PW=<任何长度的密码>` 这种最常见形态前后都是
/// 分隔符，长密码短密码一律命中；而 `isp` 里的 `p` 前后都是字母数字，不受影响。
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 把 `secret` 从文本里抹掉（空 `secret` 原样返回）。
///
/// **只在词边界上替换**：命中的子串前后若还有字母/数字/下划线，说明它只是更长标识符的
/// 一段，不替换（见 [`is_word_char`] 的实测背景）。逐段扫描而非一次 `replace`，是为了
/// 能在 byte 索引上判断边界；不做正则（本仓不引 `regex` 依赖）。
pub(crate) fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(secret) {
        // `pos` 由 `find` 给出，必在 char 边界；`secret.len()` 是它的 byte 长度
        let after = &rest[pos + secret.len()..];
        let left_boundary = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let right_boundary = after.chars().next().is_none_or(|c| !is_word_char(c));
        out.push_str(&rest[..pos]);
        if left_boundary && right_boundary {
            out.push_str("***");
        } else {
            out.push_str(secret);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// 取输出尾部并压成单行（换行折成空格，超出截断）
fn tail_snippet(output: &str) -> String {
    let flat: String = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= MESSAGE_OUTPUT_LIMIT {
        return flat;
    }
    let skip = flat.chars().count() - MESSAGE_OUTPUT_LIMIT;
    let tail: String = flat.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::models::{CommonFields, ScriptTaskConfig};
    use zeroize::Zeroizing;

    fn profile(username: &str, password: &str) -> ProfileSnapshot {
        ProfileSnapshot {
            id: "dorm".to_string(),
            name: "宿舍".to_string(),
            username: username.to_string(),
            password: Zeroizing::new(password.to_string()),
            auth_url: "http://10.100.51.1/login".to_string(),
            trigger_url: String::new(),
            isp: "移动".to_string(),
            gateway_ip: String::new(),
            wifi_ssid: String::new(),
            active_task: String::new(),
            login_channel: crate::config::LoginChannel::Script,
            active_http_task: String::new(),
            active_script_task: "portal-login".to_string(),
        }
    }

    fn task(id: &str) -> ScriptTaskConfig {
        ScriptTaskConfig {
            common: CommonFields {
                task_id: id.to_string(),
                name: "门户登录".to_string(),
                description: String::new(),
            },
            script_path: None,
            content: Some("print(1)".to_string()),
            args: Vec::new(),
            work_dir: None,
            timeout: 30,
            binary_path: None,
        }
    }

    fn result(success: bool, exit_code: i32, output: &str) -> crate::tasks::TaskResult {
        crate::tasks::TaskResult {
            success,
            output: output.to_string(),
            exit_code,
            duration_ms: 12,
            error: None,
        }
    }

    /// 契约：四个变量名与值都必须原样出现（脚本靠它们取凭据）
    #[test]
    fn login_env_exposes_documented_keys() {
        let env = login_env(&profile("20230001", "hunter2"));
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, LOGIN_ENV_KEYS, "变量名与契约列表必须一致");
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or("<缺失>")
        };
        assert_eq!(get(ENV_USERNAME), "20230001");
        assert_eq!(get(ENV_PASSWORD), "hunter2");
        assert_eq!(get(ENV_ISP), "移动");
        assert_eq!(get(ENV_AUTH_URL), "http://10.100.51.1/login");
    }

    /// 认证地址留空的方案（脚本渠道允许）也要注入空串而非漏掉该键
    #[test]
    fn login_env_keeps_empty_values() {
        let mut p = profile("u", "p");
        p.auth_url = String::new();
        p.isp = String::new();
        let env = login_env(&p);
        for key in [ENV_AUTH_URL, ENV_ISP] {
            assert!(
                env.iter().any(|(k, v)| k == key && v.is_empty()),
                "{key} 必须以空串注入"
            );
        }
    }

    /// 退出码 0 → 成功；非 0 → 可重试（与直连「未命中成功标识」同类）
    #[test]
    fn exit_code_decides_outcome() {
        let ok = to_structured("portal-login", &result(true, 0, "login ok"), "pw");
        assert_eq!(ok.outcome, Outcome::Success);
        assert!(ok.message.contains("退出码 0"), "{}", ok.message);
        assert_eq!(ok.data["script_exit_code"], 0);

        let bad = to_structured("portal-login", &result(false, 3, "boom"), "pw");
        assert_eq!(bad.outcome, Outcome::AssertionFailed);
        assert_eq!(
            crate::login::session::classify(bad.outcome),
            crate::login::session::ResultAction::Retry,
            "脚本失败必须走重试预算，而不是直接终态"
        );
        assert!(bad.message.contains("退出码 3"), "{}", bad.message);
        assert!(bad.message.contains("boom"), "{}", bad.message);
    }

    /// 脚本失败**不得**触发 Worker 回收（脚本渠道没有 Worker；误判会去杀别人的浏览器会话）
    #[test]
    fn script_failure_does_not_force_worker_recycle() {
        let bad = to_structured("x", &result(false, 1, ""), "");
        assert!(!crate::login::session::should_force_recycle(bad.outcome));
    }

    /// 输出里的密码必须被抹掉：脚本打印含凭据的 URL 是常见写法，历史要落盘
    #[test]
    fn password_is_redacted_from_message() {
        let s = to_structured(
            "x",
            &result(false, 1, "GET http://u/hunter2@portal/login"),
            "hunter2",
        );
        assert!(!s.message.contains("hunter2"), "{}", s.message);
        assert!(s.message.contains("***"), "{}", s.message);
    }

    /// 短密码不得把无关字段名切碎（真机实测：一位密码让 `isp=` 变成 `is***=`）
    #[test]
    fn short_password_only_redacted_on_word_boundaries() {
        let s = to_structured("x", &result(false, 1, "PW=p isp= auth="), "p");
        assert!(s.message.contains("PW=***"), "{}", s.message);
        assert!(
            s.message.contains("isp="),
            "命名字段不得被切碎: {}",
            s.message
        );
        assert!(!s.message.contains("is***"), "{}", s.message);
    }

    /// 词边界之外不替换：更长标识符里的一段不算"密码出现"
    #[test]
    fn redact_respects_identifier_boundaries() {
        assert_eq!(redact_secret("xpass2", "pass"), "xpass2");
        assert_eq!(redact_secret("password=pass", "pass"), "password=***");
        assert_eq!(redact_secret("pass", "pass"), "***");
        // 多字节密码（中文）同样按边界替换
        assert_eq!(redact_secret("pw=密码;", "密码"), "pw=***;");
        // 空密码：原样返回（方案密码为空时登录前就被拦，这里是防御）
        assert_eq!(redact_secret("nothing", ""), "nothing");
    }

    /// 长输出只留尾部、压成单行
    #[test]
    fn message_keeps_tail_only() {
        let long = format!("{}\n最后一行在这里", "行".repeat(500));
        let s = to_structured("x", &result(true, 0, &long), "");
        assert!(s.message.contains("最后一行在这里"));
        assert!(s.message.chars().count() < MESSAGE_OUTPUT_LIMIT + 60);
        assert!(!s.message.contains('\n'), "消息必须单行");
    }

    /// Debug 不得泄漏密码（它可能被 `?plan` 打日志）
    #[test]
    fn debug_redacts_password() {
        let plan = ScriptLoginPlan {
            task: task("portal-login"),
            extra_env: login_env(&profile("u", "SUPER_SECRET")),
        };
        let dbg = format!("{plan:?}");
        assert!(!dbg.contains("SUPER_SECRET"), "{dbg}");
        assert!(dbg.contains("portal-login"), "{dbg}");
    }
}
