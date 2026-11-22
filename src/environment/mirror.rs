//! GitHub 下载源选择偏好（初始化弹窗手动选源）：auto / direct / mirror
//!
//! 默认 `Auto`：每次下载前并发实测吞吐自动选最优源；用户显式选择 `Direct` 或
//! 某镜像前缀后跳过测速、固定顺序。偏好落盘 `environment/mirror.json`，独立于
//! `PythonPreferences`（OCR 偏好），互不影响读写；文件缺失/损坏一律回退 Auto，
//! 偏好损坏不得阻断环境引导。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::environment::{EnvironmentError, EnvironmentManager, GITHUB_MIRRORS};

/// 偏好文件名（位于 `environment/` 下）
const MIRROR_PREF_FILE: &str = "mirror.json";

/// GitHub 下载源选择：auto = 每次实测吞吐自动排序（默认）；direct = 直连优先；
/// mirror = 固定走指定镜像前缀。
///
/// Mirror 携带的前缀在解析入口（文件回读 / API 载荷）已校验属于
/// [`GITHUB_MIRRORS`]，消费方无需重复校验。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", content = "mirror", rename_all = "lowercase")]
pub enum MirrorChoice {
    /// 自动测速（默认）
    #[default]
    Auto,
    /// GitHub 直连优先
    Direct,
    /// 固定镜像前缀
    Mirror(String),
}

impl MirrorChoice {
    /// 从 API 载荷解析（choice 字符串 + 可选 mirror 前缀）
    ///
    /// mirror 必须在 [`GITHUB_MIRRORS`] 名单内，未知值返回 Err（前端可展示纠正）。
    pub fn from_api(choice: &str, mirror: Option<&str>) -> Result<Self, String> {
        match choice.trim() {
            "auto" | "" => Ok(Self::Auto),
            "direct" => Ok(Self::Direct),
            "mirror" => {
                let m = mirror.unwrap_or("").trim().to_string();
                if GITHUB_MIRRORS.contains(&m.as_str()) {
                    Ok(Self::Mirror(m))
                } else {
                    Err(format!(
                        "未知镜像源 {m:?}，可选：{}",
                        GITHUB_MIRRORS.join(", ")
                    ))
                }
            }
            other => Err(format!("未知选择 {other:?}，可选 auto/direct/mirror")),
        }
    }

    /// 镜像前缀（Auto/Direct 为 None）
    pub fn mirror_prefix(&self) -> Option<&str> {
        match self {
            Self::Mirror(m) => Some(m.as_str()),
            _ => None,
        }
    }
}

fn pref_path(mgr: &EnvironmentManager) -> PathBuf {
    mgr.env_path().join(MIRROR_PREF_FILE)
}

/// 读取镜像源选择偏好；文件缺失/损坏一律回退 Auto
pub fn read_mirror_choice(mgr: &EnvironmentManager) -> MirrorChoice {
    let path = pref_path(mgr);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return MirrorChoice::Auto,
        // 读失败（权限等）不阻断引导：Auto 语义下走测速路径
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                "镜像偏好文件读取失败（{error}），回退自动测速"
            );
            return MirrorChoice::Auto;
        }
    };
    match serde_json::from_slice::<MirrorChoice>(&bytes) {
        Ok(choice) => choice,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                "镜像偏好文件损坏（{error}），回退自动测速"
            );
            MirrorChoice::Auto
        }
    }
}

/// 原子保存镜像源选择偏好
pub async fn write_mirror_choice(
    mgr: &EnvironmentManager,
    choice: &MirrorChoice,
) -> Result<(), EnvironmentError> {
    tokio::fs::create_dir_all(mgr.env_path())
        .await
        .map_err(|source| EnvironmentError::StateIo {
            path: mgr.env_path().clone(),
            source: crate::utils::io_human::io_error_reason(source),
        })?;
    let path = pref_path(mgr);
    crate::utils::atomic_write_json(&path, choice).map_err(|source| EnvironmentError::StateIo {
        path,
        source: crate::utils::io_human::io_error_reason(source),
    })
}

/// 判断 URL 是否经镜像前缀构造，是则返回对应镜像前缀（直连返回 None）。
///
/// 供测速结果归源（direct/mirror 分类）与 Python 下载源阶梯映射共用。
pub(crate) fn mirror_prefix_of(url: &str) -> Option<&'static str> {
    GITHUB_MIRRORS
        .iter()
        .copied()
        .find(|mirror| url.starts_with(*mirror))
}

/// 单个下载源的测速结果（初始化弹窗镜像选择行展示用）
#[derive(Debug, Serialize)]
pub struct MirrorProbeEntry {
    /// 源类型：`direct`（GitHub 直连）或 `mirror`（代理镜像）
    pub kind: &'static str,
    /// 镜像前缀（直连为空串；选中偏好即存该值）
    pub prefix: String,
    /// 展示名（host：github.com / gh-proxy.com 等）
    pub label: String,
    /// 是否测得数据（false = 超时/失败/零字节，UI 显示「不可用」）
    pub ok: bool,
    /// 已测字节数
    pub bytes: usize,
    /// 总耗时（毫秒）
    pub elapsed_ms: u64,
    /// 实测吞吐（字节/秒，失败为 0）
    pub throughput: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// API 载荷解析：三种合法选择；mirror 必须在名单内；空 choice 回退 Auto
    #[test]
    fn test_mirror_choice_from_api() {
        assert_eq!(
            MirrorChoice::from_api("auto", None).unwrap(),
            MirrorChoice::Auto
        );
        assert_eq!(
            MirrorChoice::from_api("", None).unwrap(),
            MirrorChoice::Auto
        );
        assert_eq!(
            MirrorChoice::from_api("direct", None).unwrap(),
            MirrorChoice::Direct
        );
        let m = GITHUB_MIRRORS[0];
        assert_eq!(
            MirrorChoice::from_api("mirror", Some(m)).unwrap(),
            MirrorChoice::Mirror(m.to_string())
        );
        // 名单外镜像拒绝
        assert!(MirrorChoice::from_api("mirror", Some("https://evil.example/")).is_err());
        // mirror 缺前缀拒绝
        assert!(MirrorChoice::from_api("mirror", None).is_err());
        // 未知 choice 拒绝
        assert!(MirrorChoice::from_api("fastest", None).is_err());
    }

    /// serde 往返：tagged 形态 {"choice":"mirror","mirror":前缀}，Auto 无 content
    #[test]
    fn test_mirror_choice_serde_roundtrip() {
        let json_auto = serde_json::json!({"choice": "auto"});
        let parsed: MirrorChoice = serde_json::from_value(json_auto).unwrap();
        assert_eq!(parsed, MirrorChoice::Auto);

        let m = GITHUB_MIRRORS[1];
        let json_mirror = serde_json::json!({"choice": "mirror", "mirror": m});
        let parsed: MirrorChoice = serde_json::from_value(json_mirror).unwrap();
        assert_eq!(parsed, MirrorChoice::Mirror(m.to_string()));

        let back = serde_json::to_value(MirrorChoice::Mirror(m.to_string())).unwrap();
        assert_eq!(back, serde_json::json!({"choice": "mirror", "mirror": m}));
    }

    /// 偏好落盘往返：写 Auto/Direct/Mirror 后读回一致；镜像前缀可用
    #[tokio::test]
    async fn test_mirror_choice_persist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
        );
        assert_eq!(
            read_mirror_choice(&mgr),
            MirrorChoice::Auto,
            "缺文件回退 Auto"
        );

        for choice in [
            MirrorChoice::Direct,
            MirrorChoice::Mirror(GITHUB_MIRRORS[0].to_string()),
            MirrorChoice::Auto,
        ] {
            write_mirror_choice(&mgr, &choice).await.unwrap();
            assert_eq!(read_mirror_choice(&mgr), choice);
        }
    }

    /// 偏好文件损坏回退 Auto，不阻断引导
    #[test]
    fn test_mirror_choice_corrupt_file_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = EnvironmentManager::new(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::status::StatusManager::new()),
        );
        let env_dir = dir.path().join("environment");
        std::fs::create_dir_all(&env_dir).unwrap();
        std::fs::write(env_dir.join("mirror.json"), b"not-json{").unwrap();
        assert_eq!(read_mirror_choice(&mgr), MirrorChoice::Auto);
    }
}
