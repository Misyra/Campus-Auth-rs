//! EngineCommand 枚举定义

use serde::Serialize;
use tokio::sync::oneshot;

use crate::engine::EngineError;
use crate::status::NetworkStatus;

/// 调度引擎接收的全部命令
#[derive(Debug)]
pub enum EngineCommand {
    /// 启动监测循环（幂等：已运行时忽略）
    Start,

    /// 停止监测循环（Engine task 保持存活，可再次 Start）
    Stop,

    /// 重新加载配置（settings.json + profiles/*.json）
    Reload,

    /// 切换到指定 Profile
    ApplyProfile {
        /// 目标 Profile ID
        profile_id: String,
        /// 来源标记（手动 / 自动）
        source: ProfileSwitchSource,
    },

    /// 执行一次网络探测并通过 oneshot 返回结果
    TestNetwork {
        /// 结果回传通道
        reply: oneshot::Sender<Result<TestNetworkResult, EngineError>>,
    },

    /// 暂停监测（Engine 保持运行，跳过检测循环）
    Pause,

    /// 恢复监测，立即执行一次检测
    Resume,

    /// 终止引擎 task，清理资源
    Shutdown,
}

/// Profile 切换来源标记
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSwitchSource {
    /// 用户通过 Web API 手动切换
    Manual,
    /// Engine 低频检测到网关/SSID 变化自动切换
    AutoSwitch,
}

/// 单次网络探测结果
#[derive(Debug, Serialize)]
pub struct TestNetworkResult {
    /// 探测结论
    pub status: NetworkStatus,
    /// 各探测方法的详细结果
    pub details: ProbeDetails,
    /// 探测耗时（毫秒）
    pub duration_ms: u64,
}

/// 各探测方法的详细结果
#[derive(Debug, Serialize)]
pub struct ProbeDetails {
    /// TCP 探测结果描述
    pub tcp: Vec<String>,
    /// HTTP 探测结果描述
    pub http: Vec<String>,
    /// URL 探测结果描述
    pub url: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ TestNetworkResult / ProbeDetails 序列化测试 ============

    #[test]
    fn test_test_network_result_serialize_snake_case() {
        // NetworkStatus 应序列化为 snake_case（online / captive_portal / offline / paused）
        for (status, expect) in [
            (NetworkStatus::Online, "online"),
            (NetworkStatus::CaptivePortal, "captive_portal"),
            (NetworkStatus::Offline, "offline"),
            (NetworkStatus::Paused, "paused"),
        ] {
            let r = TestNetworkResult {
                status,
                details: ProbeDetails {
                    tcp: vec![],
                    http: vec![],
                    url: vec![],
                },
                duration_ms: 0,
            };
            let json = serde_json::to_string(&r).unwrap();
            assert!(
                json.contains(&format!("\"{expect}\"")),
                "status {json} 应包含 {expect}"
            );
        }
    }

    #[test]
    fn test_test_network_result_serialize_fields() {
        // 验证 details 子字段与 duration_ms 完整序列化
        let r = TestNetworkResult {
            status: NetworkStatus::Online,
            details: ProbeDetails {
                tcp: vec!["Pass".into()],
                http: vec!["Pass".into()],
                url: vec!["Pass".into()],
            },
            duration_ms: 123,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"tcp\":[\"Pass\"]"));
        assert!(json.contains("\"http\":[\"Pass\"]"));
        assert!(json.contains("\"url\":[\"Pass\"]"));
        assert!(json.contains("\"duration_ms\":123"));
    }

    // ============ EngineCommand 构造与 oneshot 回传测试 ============

    #[tokio::test]
    async fn test_engine_command_test_network_reply_roundtrip() {
        // 构造 TestNetwork 命令，通过 oneshot 回传结果，验证 reply 通道可用
        let (tx, rx) = oneshot::channel();
        let cmd = EngineCommand::TestNetwork { reply: tx };
        // 模拟处理方发送探测结果
        let sent = Ok(TestNetworkResult {
            status: NetworkStatus::CaptivePortal,
            details: ProbeDetails {
                tcp: vec!["Timeout".into()],
                http: vec!["Pass".into()],
                url: vec!["Disabled".into()],
            },
            duration_ms: 50,
        });
        match cmd {
            EngineCommand::TestNetwork { reply } => {
                let _ = reply.send(sent);
            }
            _ => panic!("应为 TestNetwork 变体"),
        }
        let got = rx.await.unwrap().unwrap();
        assert_eq!(got.status, NetworkStatus::CaptivePortal);
        assert_eq!(got.duration_ms, 50);
        assert_eq!(got.details.tcp, vec!["Timeout".to_string()]);
    }

    // ============ ProfileSwitchSource 相等性测试 ============

    #[test]
    fn test_profile_switch_source_equality() {
        assert_eq!(ProfileSwitchSource::Manual, ProfileSwitchSource::Manual);
        assert_ne!(ProfileSwitchSource::Manual, ProfileSwitchSource::AutoSwitch);
    }
}
