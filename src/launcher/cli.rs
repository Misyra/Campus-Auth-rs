//! CLI 参数定义：`main.rs` 解析后传入 `launcher::run` 的启动参数与枚举

use std::path::PathBuf;

use clap::Parser;

use crate::config::schema::StartupAction;

/// CLI 参数定义
#[derive(Parser, Clone)]
#[command(name = "campus-auth", version, about)]
pub struct CliArgs {
    /// 启动模式：完整 / 轻量 / 单次登录（缺省时读 settings.json 的 app.runtime_mode）
    #[arg(short, long, value_enum)]
    pub mode: Option<RuntimeMode>,

    /// 基准路径（配置文件 / 任务 / 日志 的根目录）
    #[arg(long, env = "CAMPUS_AUTH_BASE_PATH")]
    pub base_path: Option<PathBuf>,
    /// 监听端口（默认从 settings.json 读取）
    #[arg(short, long, env = "CAMPUS_AUTH_PORT")]
    pub port: Option<u16>,

    /// 监听地址（默认 127.0.0.1，Docker 环境默认 0.0.0.0；可用 CAMPUS_AUTH_HOST 环境变量覆盖）
    #[arg(long, env = "CAMPUS_AUTH_HOST")]
    pub host: Option<String>,

    /// 启动后不自动打开浏览器
    #[arg(long)]
    pub no_browser: bool,

    /// 不显示系统托盘图标
    #[arg(long)]
    pub no_tray: bool,

    /// 强制终止已有实例后启动
    #[arg(long)]
    pub force: bool,

    /// 重启标记（新进程等待旧进程释放锁）
    #[arg(long, hide = true)]
    pub restarting: bool,

    /// 查询当前运行实例的状态
    #[arg(long)]
    pub status: bool,

    /// 停止当前运行实例
    #[arg(long)]
    pub stop: bool,

    /// 注册 / 取消开机自启动
    #[arg(long, value_enum)]
    pub autostart: Option<AutostartAction>,

    /// 系统登录启动来源标记，与 --autostart enable/disable 管理命令分开。
    #[arg(long, hide = true)]
    pub startup_launch: bool,

    /// 启动动作（覆盖 settings.json 中的 startup_action）
    #[arg(long, value_enum)]
    pub startup_action: Option<StartupAction>,

    /// 管理密码覆盖值：非回环绑定（Docker/局域网）下经 POST /api/auth/exchange
    /// 的 password 分支换取鉴权 token；非空时优先于 settings.json 的
    /// management_password（env: CAMPUS_AUTH_MANAGEMENT_PASSWORD）
    #[arg(long, env = "CAMPUS_AUTH_MANAGEMENT_PASSWORD")]
    pub management_password: Option<String>,

    /// 显式允许无管理凭据绑定非回环地址（自行承担暴露风险；
    /// env: CAMPUS_AUTH_ALLOW_UNAUTH_BIND，取 true/1 等布尔值）
    #[arg(long, env = "CAMPUS_AUTH_ALLOW_UNAUTH_BIND")]
    pub allow_unauthenticated_bind: bool,

    /// 允许在系统临时目录中运行（临时目录拦截的排查放行，仅在确认已解压但仍被误判时使用）
    #[arg(long, hide = true)]
    pub allow_temp: bool,
}

/// 运行模式
#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum RuntimeMode {
    /// 完整模式（全部服务 + Web API + 托盘）
    Full,
    /// 轻量模式（仅引擎 + 托盘，Axum 按需启动）
    Lightweight,
    /// 单次登录（执行一次登录后退出）
    LoginOnce,
}

/// 自启动操作
#[derive(Clone, Debug, clap::ValueEnum)]
pub enum AutostartAction {
    /// 注册自启动
    Enable,
    /// 取消自启动
    Disable,
}

#[cfg(test)]
mod tests {
    #[test]
    fn autostart_launch_marker_is_separate_from_management_action() {
        use clap::Parser;
        let cli = super::CliArgs::try_parse_from(["campus-auth", "--startup-launch"]).unwrap();
        assert!(cli.startup_launch);
        assert!(cli.autostart.is_none());
        let cli = super::CliArgs::try_parse_from(["campus-auth", "--autostart", "enable"]).unwrap();
        assert!(!cli.startup_launch);
        assert!(matches!(
            cli.autostart,
            Some(super::AutostartAction::Enable)
        ));
    }
}
