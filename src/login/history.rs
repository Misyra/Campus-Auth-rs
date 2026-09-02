//! 登录历史持久化：JSONL 追加写入 + 按日期查询 + 清空
//!
//! 每次登录终态（成功/失败/取消）由 [`LoginSession`] 调用 [`LoginHistoryService::record`]
//! 写入 `logs/login_history/YYYY-MM-DD.jsonl`（每天一个文件，每行一条 JSON 对象）。
//! Web 层 `GET /api/history` 与 `DELETE /api/history` 直接调用 [`query`] 与 [`clear`]。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::status::LoginSource;

/// 历史记录的结果分类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryResult {
    /// 登录成功
    Success,
    /// 登录失败
    Failed,
    /// 登录被取消
    Cancelled,
}

/// 单条登录历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginHistoryEntry {
    /// 记录时间戳（本地时区 ISO 8601）
    pub timestamp: DateTime<Local>,
    /// 登录来源
    #[serde(serialize_with = "ser_source", deserialize_with = "de_source")]
    pub source: LoginSource,
    /// 关联 Profile ID
    pub profile_id: String,
    /// 结果分类
    pub result: HistoryResult,
    /// 结果消息
    pub message: String,
    /// 耗时（秒，浮点）
    pub duration_secs: f64,
}

/// `LoginSource` 序列化（复用其 `#[serde(rename_all = "snake_case")]` 实现）
fn ser_source<S: Serializer>(s: &LoginSource, ser: S) -> Result<S::Ok, S::Error> {
    serde::Serialize::serialize(s, ser)
}

/// `LoginSource` 反序列化（上游仅派生 `Serialize`，此处手动映射 snake_case 字符串）
fn de_source<'de, D: Deserializer<'de>>(de: D) -> Result<LoginSource, D::Error> {
    let v = String::deserialize(de)?;
    match v.as_str() {
        "auto" => Ok(LoginSource::Auto),
        "manual" => Ok(LoginSource::Manual),
        "login_once" => Ok(LoginSource::LoginOnce),
        "browser" => Ok(LoginSource::Browser),
        other => Err(serde::de::Error::custom(format!(
            "未知 LoginSource: {other}"
        ))),
    }
}

/// 登录历史持久化服务
///
/// 持有基准路径（通常为 exe 所在目录），历史文件位于 `<base_path>/logs/login_history/`。
pub struct LoginHistoryService {
    /// 基准路径（与 ConfigService 的 base_path 一致）
    base_path: PathBuf,
}

/// Web 层消费的历史存储抽象（M1 细粒度 state 试点）
///
/// handler 通过 `State<Arc<dyn HistoryStore>>` 提取依赖（AppState 实现了
/// [`axum::extract::FromRef`] 委派），测试可用内存实现直接构造 mini Router
/// 做 handler 级单测，无需装配完整 ServiceContainer。
#[async_trait::async_trait]
pub trait HistoryStore: Send + Sync {
    /// 查询日期区间 `[from, to]` 内的全部记录，按时间升序排列
    async fn query(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
    ) -> Result<Vec<LoginHistoryEntry>, std::io::Error>;

    /// 清空全部历史
    async fn clear(&self) -> Result<(), std::io::Error>;
}

#[async_trait::async_trait]
impl HistoryStore for LoginHistoryService {
    async fn query(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
    ) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
        LoginHistoryService::query(self, from, to).await
    }

    async fn clear(&self) -> Result<(), std::io::Error> {
        LoginHistoryService::clear(self).await
    }
}

impl LoginHistoryService {
    /// 构造历史服务（基准路径通常为 exe 所在目录）
    pub fn new(base_path: &Path) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
        }
    }

    /// 历史文件目录 `<base_path>/logs/login_history`
    fn history_dir(&self) -> PathBuf {
        self.base_path.join("logs").join("login_history")
    }

    /// 指定日期对应的 JSONL 文件路径
    fn file_for(&self, date: NaiveDate) -> PathBuf {
        self.history_dir()
            .join(format!("{}.jsonl", date.format("%Y-%m-%d")))
    }

    /// 追加写入一条历史记录（每天一个 JSONL 文件，每行一个 JSON 对象）
    pub async fn record(&self, entry: &LoginHistoryEntry) -> Result<(), std::io::Error> {
        let dir = self.history_dir();
        tokio::fs::create_dir_all(&dir).await?;
        let path = self.file_for(entry.timestamp.date_naive());
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        file.write_all(format!("{line}\n").as_bytes()).await?;
        Ok(())
    }

    /// 查询日期区间 `[from, to]`（闭区间，按天枚举文件）内的全部历史记录，按时间升序排列
    pub async fn query(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
    ) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
        let dir = self.history_dir();
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        let mut results = Vec::new();
        for day in date_range(from.date_naive(), to.date_naive()) {
            let path = self.file_for(day);
            let file = match tokio::fs::File::open(&path).await {
                Ok(f) => f,
                // 文件缺失属正常路径（当天无记录），静默跳过
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %e,
                        "打开登录历史文件失败，跳过该日期"
                    );
                    continue;
                }
            };
            let reader = tokio::io::BufReader::new(file);
            let mut lines = reader.lines();
            while let Some(line) = lines.next_line().await? {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<LoginHistoryEntry>(&line) {
                    results.push(entry);
                } else {
                    // 损坏行静默丢弃会让用户误以为记录丢失，留 debug 痕迹
                    tracing::debug!(path = %path.display(), "登录历史行解析失败，已跳过");
                }
            }
        }
        results.sort_by_key(|e| e.timestamp);
        Ok(results)
    }

    /// 清空全部历史文件（删除目录下所有 `*.jsonl`）
    pub async fn clear(&self) -> Result<(), std::io::Error> {
        let dir = self.history_dir();
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                tokio::fs::remove_file(&p).await?;
            }
        }
        Ok(())
    }
}

/// 枚举 `[start, end]` 闭区间内的每一天（NaiveDate）
fn date_range(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let mut days = Vec::new();
    let mut cur = start;
    while cur <= end {
        days.push(cur);
        cur = match cur.succ_opt() {
            Some(next) => next,
            None => break,
        };
    }
    days
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // ============ date_range 纯函数测试 ============

    #[test]
    fn test_date_range_single_day() {
        // start == end：仅返回一天
        let r = date_range(d(2025, 7, 9), d(2025, 7, 9));
        assert_eq!(r, vec![d(2025, 7, 9)]);
    }

    #[test]
    fn test_date_range_multi_day() {
        // 闭区间 [7-09, 7-11] → 3 天
        let r = date_range(d(2025, 7, 9), d(2025, 7, 11));
        assert_eq!(r, vec![d(2025, 7, 9), d(2025, 7, 10), d(2025, 7, 11)]);
    }

    #[test]
    fn test_date_range_cross_month_boundary() {
        // 跨月边界：7-31 ~ 8-02
        let r = date_range(d(2025, 7, 31), d(2025, 8, 2));
        assert_eq!(r, vec![d(2025, 7, 31), d(2025, 8, 1), d(2025, 8, 2)]);
    }

    #[test]
    fn test_date_range_reversed_is_empty() {
        // start > end → 空集
        let r = date_range(d(2025, 7, 11), d(2025, 7, 9));
        assert!(r.is_empty());
    }

    // ============ LoginHistoryEntry serde 测试 ============

    fn sample_entry() -> LoginHistoryEntry {
        LoginHistoryEntry {
            timestamp: chrono::Local
                .with_ymd_and_hms(2025, 7, 9, 10, 30, 0)
                .unwrap(),
            source: LoginSource::LoginOnce,
            profile_id: "p1".into(),
            result: HistoryResult::Success,
            message: "登录成功".into(),
            duration_secs: 1.5,
        }
    }

    #[test]
    fn test_history_entry_serde_roundtrip() {
        let e = sample_entry();
        let json = serde_json::to_string(&e).unwrap();
        let back: LoginHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.profile_id, "p1");
        assert_eq!(back.source, LoginSource::LoginOnce);
        assert_eq!(back.result, HistoryResult::Success);
        assert_eq!(back.message, "登录成功");
        assert!((back.duration_secs - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_history_entry_source_serializes_snake_case() {
        // login_once → snake_case
        let e = sample_entry();
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"login_once\""));
        // 其余来源
        let mut e2 = e;
        e2.source = LoginSource::Browser;
        assert!(serde_json::to_string(&e2).unwrap().contains("\"browser\""));
        e2.source = LoginSource::Auto;
        assert!(serde_json::to_string(&e2).unwrap().contains("\"auto\""));
        e2.source = LoginSource::Manual;
        assert!(serde_json::to_string(&e2).unwrap().contains("\"manual\""));
    }

    #[test]
    fn test_de_source_unknown_rejected() {
        // 未知来源字符串应反序列化失败
        let json = r#"{
            "timestamp": "2025-07-09T10:30:00+08:00",
            "source": "unknown_source",
            "profile_id": "p1",
            "result": "success",
            "message": "x",
            "duration_secs": 0.0
        }"#;
        let res: Result<LoginHistoryEntry, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }

    #[test]
    fn test_history_result_serde_snake_case() {
        // success / failed / cancelled
        for (hr, expect) in [
            (HistoryResult::Success, "success"),
            (HistoryResult::Failed, "failed"),
            (HistoryResult::Cancelled, "cancelled"),
        ] {
            let s = serde_json::to_string(&hr).unwrap();
            assert!(s.contains(expect), "{s} should contain {expect}");
            let back: HistoryResult = serde_json::from_str(&s).unwrap();
            assert_eq!(back, hr);
        }
    }

    // ============ LoginHistoryService 异步测试（tempfile 隔离） ============

    fn make_service() -> (TempDir, LoginHistoryService) {
        let dir = TempDir::new().unwrap();
        let svc = LoginHistoryService::new(dir.path());
        (dir, svc)
    }

    #[tokio::test]
    async fn test_history_record_and_query() {
        let (_dir, svc) = make_service();
        let mut e = sample_entry();
        e.result = HistoryResult::Success;
        e.message = "ok".into();
        // 写入两条
        svc.record(&e).await.unwrap();
        let mut e2 = e.clone();
        e2.result = HistoryResult::Failed;
        e2.message = "bad".into();
        svc.record(&e2).await.unwrap();

        // 查询同一天区间
        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();
        let rows = svc.query(from, to).await.unwrap();
        assert_eq!(rows.len(), 2);
        // 升序排列
        assert_eq!(rows[0].result, HistoryResult::Success);
        assert_eq!(rows[1].result, HistoryResult::Failed);
    }

    #[tokio::test]
    async fn test_history_query_empty_dir() {
        // 目录不存在时返回空 vec（不报错）
        let (_dir, svc) = make_service();
        let from = chrono::Local.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let to = chrono::Local.with_ymd_and_hms(2025, 1, 2, 0, 0, 0).unwrap();
        let rows = svc.query(from, to).await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_history_clear_removes_files() {
        let (_dir, svc) = make_service();
        let e = sample_entry();
        svc.record(&e).await.unwrap();
        svc.record(&e).await.unwrap();
        // 清空前能查到
        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();
        assert!(!svc.query(from, to).await.unwrap().is_empty());
        // 清空后查不到
        svc.clear().await.unwrap();
        assert!(svc.query(from, to).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_history_clear_empty_dir_is_noop() {
        // 目录不存在时 clear 不报错
        let (_dir, svc) = make_service();
        svc.clear().await.unwrap();
    }

    #[tokio::test]
    async fn test_history_query_skips_malformed_lines() {
        // 损坏行应被跳过而非导致查询失败
        let (_dir, svc) = make_service();
        let e = sample_entry();
        svc.record(&e).await.unwrap();
        // 手动追加损坏行与空行
        let day = e.timestamp.date_naive();
        let path = svc
            .history_dir()
            .join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        f.write_all(b"this is not json\n").await.unwrap();
        f.write_all(b"\n").await.unwrap(); // 空行也应跳过

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();
        let rows = svc.query(from, to).await.unwrap();
        // 仅合法行被解析
        assert_eq!(rows.len(), 1);
    }
}
