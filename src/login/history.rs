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

    /// 查询日期区间 `[from, to]` 内**最新的 `limit` 条**记录，按时间升序排列。
    ///
    /// 语义等价于 `query(from, to)` 后保留末尾 `limit` 条，但实现方**应**利用
    /// 「只需最近 N 条」这一约束做短路读取，避免为取几条而解析整个区间的历史。
    ///
    /// 刻意不提供默认实现：该方法存在的唯一理由就是省掉全量读取，若给一个
    /// 「先全量再截断」的默认实现，实现方漏覆写即静默退化为原始性能问题
    /// （见 `docs/reports` 中 `/api/history` 全量读取的实测记录），故由编译器强制实现。
    async fn query_latest(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
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

    async fn query_latest(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
    ) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
        LoginHistoryService::query_latest(self, from, to, limit).await
    }

    async fn clear(&self) -> Result<(), std::io::Error> {
        LoginHistoryService::clear(self).await
    }
}

/// 登录历史保留天数（LOG-2）：与 Web 查询窗口（`/api/history` 固定最近 30 天）
/// 对齐，更早的文件没有消费路径，由每日 housekeeping 删除以防无限累积。
pub const HISTORY_RETENTION_DAYS: u32 = 30;

impl LoginHistoryService {
    /// 构造历史服务（基准路径通常为 exe 所在目录）
    pub fn new(base_path: &Path) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
        }
    }

    /// 历史文件目录 `<base_path>/logs/login_history`（路径经 `utils::paths` 统一）
    fn history_dir(&self) -> PathBuf {
        crate::utils::paths::login_history_dir(&self.base_path)
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

    /// 查询日期区间 `[from, to]` 内最新的 `limit` 条记录（升序返回）
    ///
    /// 与 [`Self::query`] 的区别只在**枚举范围**：不读取整个区间的全部日期文件，
    /// 而是**按天倒序**（最新在前）逐天读取与解析，累计条数达到 `limit` 即停止，
    /// 因此不需触碰更早的日期文件。
    ///
    /// 与 `query(from, to)` 后取末尾 `limit` 条语义等价（`query` 按升序返回，
    /// 末尾即最新），且与之同样**显式按时间排序**、不假设物理文件顺序。
    ///
    /// 为什么不能像「从文件尾部倒读」那样在文件内早停：追加写**不保证**物理顺序
    /// 等于时间顺序——`record` 每条新开 tokio `File`，而 tokio `File` 内含写缓冲、
    /// drop 时仅**尽力**异步 flush，紧密连续写入时落盘先后可能不同于调用顺序
    /// （并行测试下已实际复现非确定性乱序）。`query` 正是因此显式排序而非
    /// 依赖顺序（见其末尾 `sort_by_key`），本函数沿用同一口径。
    /// 因此单日文件必须整读后排序，不可依赖「尾部即最新」。
    ///
    /// 天与时间的顺序是可靠的：`record` 以 `entry.timestamp.date_naive()` 选择
    /// 文件，故某日文件内的记录时间戳必落于该日，跨天的先后关系与日期顺序一致。
    ///
    /// 空行与损坏行沿用 `query` 的静默跳过策略，**不占用** `limit` 配额——
    /// 否则损坏行会挤占返回条数，与「全量后截断」的结果不一致。
    ///
    /// `limit == 0` 时不读任何文件，直接返回空。
    pub async fn query_latest(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
    ) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let dir = self.history_dir();
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(Vec::new());
        }
        // 按天倒序（最新在前）
        let mut days = date_range(from.date_naive(), to.date_naive());
        days.reverse();

        // 逐天收集（新的日期在前），凑够 limit 即停
        let mut groups: Vec<Vec<LoginHistoryEntry>> = Vec::new();
        let mut total = 0usize;
        for day in days {
            let path = self.file_for(day);
            let mut entries = match read_all_entries(&path).await {
                Ok(e) => e,
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
            if entries.is_empty() {
                continue;
            }
            // 单日内部按时间升序：不假设物理追加顺序
            entries.sort_by_key(|e| e.timestamp);
            total += entries.len();
            groups.push(entries);
            if total >= limit {
                break;
            }
        }

        // groups 为「新日期在前」，逐组反序拼接得到全局升序
        let mut result: Vec<LoginHistoryEntry> = Vec::with_capacity(limit.min(total.max(1)));
        for group in groups.into_iter().rev() {
            result.extend(group);
        }
        // 末尾即最新：超出 limit 的部分是最早的，从头丢弃
        if result.len() > limit {
            result.drain(..result.len() - limit);
        }
        Ok(result)
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

    /// 删除早于保留期的历史文件（LOG-2），返回删除数量
    ///
    /// Web 查询窗口固定为最近 30 天，更早的文件没有消费路径；按文件名日期
    /// 判定（`%Y-%m-%d.jsonl`），文件名非日期格式（外部放置）的文件不清理。
    pub async fn clear_older_than(&self, keep_days: u32) -> Result<usize, std::io::Error> {
        let dir = self.history_dir();
        if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
            return Ok(0);
        }
        // 保留最近 keep_days 个自然日（含今天）：keep_days=30 → 删 d < 今天-29
        let cutoff =
            Local::now().date_naive() - chrono::Days::new(u64::from(keep_days).saturating_sub(1));
        let mut removed = 0usize;
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if !p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
                Ok(d) if d < cutoff => match tokio::fs::remove_file(&p).await {
                    Ok(()) => removed += 1,
                    Err(e) => {
                        tracing::debug!("删除过期登录历史文件失败 {}: {e}", p.display())
                    }
                },
                Ok(_) => {}
                Err(_) => {
                    tracing::debug!(path = %p.display(), "登录历史文件名非日期格式，跳过清理")
                }
            }
        }
        Ok(removed)
    }
}

/// 读取单个历史文件内的全部有效记录（顺序即物理顺序，调用方须自行排序）
///
/// 与 [`LoginHistoryService::query`] 相同的解析口径：空行跳过、损坏行静默丢弃
/// 并留 `debug` 痕迹。文件不存在时返回 `NotFound` 错误，由调用方决定是否忽略。
///
/// 单文件是「一天」的粒度，规模有界（正常使用远小于日志文件），因此整读即可；
/// **不可**改为从尾部倒读早停——追加写不保证物理顺序等于时间顺序
/// （原因见 [`LoginHistoryService::query_latest`] 文档）。
async fn read_all_entries(path: &Path) -> Result<Vec<LoginHistoryEntry>, std::io::Error> {
    let file = tokio::fs::File::open(path).await?;
    let reader = tokio::io::BufReader::new(file);
    let mut lines = reader.lines();
    let mut entries = Vec::new();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<LoginHistoryEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(_) => {
                // 损坏行静默丢弃会让用户误以为记录丢失，留 debug 痕迹
                tracing::debug!(path = %path.display(), "登录历史行解析失败，已跳过");
            }
        }
    }
    Ok(entries)
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

    /// LOG-2：clear_older_than 按文件名日期删除超期文件，保留期内与
    /// 非日期文件名的文件一律不动
    #[tokio::test]
    async fn test_clear_older_than_by_filename_date() {
        let tmp = TempDir::new().unwrap();
        let svc = LoginHistoryService::new(tmp.path());
        let dir = tmp.path().join("logs").join("login_history");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let today = Local::now().date_naive();
        let fmt = |d: chrono::NaiveDate| d.format("%Y-%m-%d").to_string();
        // 今天（保留）、29 天前（保留）、31 天前（删除）、非法名（不动）
        for name in [
            fmt(today),
            fmt(today - chrono::Days::new(29)),
            fmt(today - chrono::Days::new(31)),
            "not-a-date".to_string(),
        ] {
            tokio::fs::write(
                dir.join(format!("{name}.jsonl")),
                b"{}
",
            )
            .await
            .unwrap();
        }

        let removed = svc.clear_older_than(HISTORY_RETENTION_DAYS).await.unwrap();
        assert_eq!(removed, 1, "仅 31 天前的文件应被删除");
        assert!(dir.join(format!("{}.jsonl", fmt(today))).exists());
        assert!(
            dir.join(format!("{}.jsonl", fmt(today - chrono::Days::new(29))))
                .exists()
        );
        assert!(
            !dir.join(format!("{}.jsonl", fmt(today - chrono::Days::new(31))))
                .exists()
        );
        assert!(dir.join("not-a-date.jsonl").exists(), "非日期文件名不清理");
    }

    // ============ query_latest（按天倒序 + 单日整读排序，凑够即停） ============

    /// 逐条写入 `count` 条记录（时间递增），返回服务
    async fn seed(svc: &LoginHistoryService, day: NaiveDate, count: usize) {
        for i in 0..count {
            let mut e = sample_entry();
            e.timestamp = chrono::Local
                .from_local_datetime(
                    &(day.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(i as i64)),
                )
                .unwrap();
            e.message = format!("m{i}");
            svc.record(&e).await.unwrap();
        }
    }

    /// query_latest 与「query 全量后取末尾 limit 条」语义等价（核心不变式）
    #[tokio::test]
    async fn test_query_latest_matches_full_query_tail() {
        let (_dir, svc) = make_service();
        let day = d(2025, 7, 9);
        seed(&svc, day, 50).await;

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();

        let full = svc.query(from, to).await.unwrap();
        assert_eq!(full.len(), 50);
        for limit in [1usize, 7, 50, 999] {
            let latest = svc.query_latest(from, to, limit).await.unwrap();
            let expect: Vec<_> = full
                .iter()
                .skip(full.len().saturating_sub(limit))
                .map(|e| e.message.clone())
                .collect();
            let got: Vec<_> = latest.iter().map(|e| e.message.clone()).collect();
            assert_eq!(got, expect, "limit={limit} 时应与全量取尾一致");
        }
    }

    /// 返回结果必须按时间升序（与 query 契约一致）
    #[tokio::test]
    async fn test_query_latest_returns_ascending_order() {
        let (_dir, svc) = make_service();
        seed(&svc, d(2025, 7, 9), 20).await;
        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();
        let rows = svc.query_latest(from, to, 5).await.unwrap();
        assert_eq!(rows.len(), 5);
        assert!(
            rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
            "应按时间升序"
        );
        // 最新一条应是 m19
        assert_eq!(rows.last().unwrap().message, "m19");
    }

    /// 跨天取数：limit 跨越文件边界时，须按「天倒序」优先取更近的日期
    #[tokio::test]
    async fn test_query_latest_spans_days_newest_first() {
        let (_dir, svc) = make_service();
        seed(&svc, d(2025, 7, 8), 3).await; // 较早
        seed(&svc, d(2025, 7, 9), 3).await; // 较晚

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 8, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();

        // 取 4 条：应是 7-8 的最后 1 条 + 7-9 的 3 条
        let rows = svc.query_latest(from, to, 4).await.unwrap();
        assert_eq!(rows.len(), 4);
        let days: Vec<_> = rows.iter().map(|e| e.timestamp.date_naive()).collect();
        assert_eq!(
            days,
            vec![d(2025, 7, 8), d(2025, 7, 9), d(2025, 7, 9), d(2025, 7, 9)]
        );
        assert!(rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
    }

    /// 损坏行与空行不占用 limit 配额（否则会挤占返回条数）
    #[tokio::test]
    async fn test_query_latest_malformed_lines_do_not_consume_limit() {
        let (_dir, svc) = make_service();
        let day = d(2025, 7, 9);
        seed(&svc, day, 5).await;
        let path = svc
            .history_dir()
            .join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        f.write_all(b"not json\n\n").await.unwrap();
        f.sync_all().await.unwrap();

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();
        let rows = svc.query_latest(from, to, 5).await.unwrap();
        assert_eq!(rows.len(), 5, "损坏行不得挤占 limit 配额");
        assert_eq!(rows.last().unwrap().message, "m4");
    }

    /// limit=0 返回空；目录不存在返回空；limit 超过总量返回全部
    #[tokio::test]
    async fn test_query_latest_boundaries() {
        let (_dir, svc) = make_service();
        let day = d(2025, 7, 9);
        seed(&svc, day, 3).await;
        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();

        assert!(svc.query_latest(from, to, 0).await.unwrap().is_empty());
        assert_eq!(svc.query_latest(from, to, 99).await.unwrap().len(), 3);

        // 空目录
        let empty = TempDir::new().unwrap();
        let svc2 = LoginHistoryService::new(empty.path());
        assert!(svc2.query_latest(from, to, 10).await.unwrap().is_empty());
    }

    /// 单日大文件（远超读缓冲）下仍须正确取到最新记录
    ///
    /// 该用例是「单日文件规模较大」的回归锚点：早期实现曾从文件尾部倒读早停，
    /// 而追加写不保证物理顺序等于时间顺序，会表现为非确定性乱序。
    #[tokio::test]
    async fn test_query_latest_large_single_day_file() {
        let (_dir, svc) = make_service();
        let day = d(2025, 7, 9);
        // 每条 message 约 200B，1200 条约 240 KB，远超 BufReader 默认缓冲
        for i in 0..1200 {
            let mut e = sample_entry();
            e.timestamp = chrono::Local
                .from_local_datetime(
                    &(day.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(i as i64)),
                )
                .unwrap();
            e.message = format!("{i:0>180}");
            svc.record(&e).await.unwrap();
        }
        let file = svc
            .history_dir()
            .join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        let size = tokio::fs::metadata(&file).await.unwrap().len();
        assert!(size > 64 * 1024, "用例前提：文件须超过 64 KiB，实际 {size}");

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();

        let full = svc.query(from, to).await.unwrap();
        assert_eq!(full.len(), 1200);

        for limit in [1usize, 5, 100, 1199, 1200] {
            let rows = svc.query_latest(from, to, limit).await.unwrap();
            assert_eq!(rows.len(), limit.min(1200), "limit={limit}");
            assert!(
                rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
                "limit={limit} 必须升序"
            );
            let expect: Vec<_> = full
                .iter()
                .skip(1200 - limit.min(1200))
                .map(|e| e.timestamp)
                .collect();
            let got: Vec<_> = rows.iter().map(|e| e.timestamp).collect();
            assert_eq!(got, expect, "limit={limit} 结果应与全量取尾一致");
        }
    }

    /// 多天 × 每天多条：limit 跨越多个日期文件边界的等价性与升序性
    ///
    /// 真实场景（30 天保留期、每天上百条）下 limit 通常跨多个文件，
    /// 该用例是「天倒序 + 天内倒读」拼接顺序的回归锚点：跨文件累积顺序
    /// 一旦写错，会表现为整体非升序、且与全量取尾不一致。
    #[tokio::test]
    async fn test_query_latest_multi_day_matches_tail_and_ascending() {
        let (_dir, svc) = make_service();
        let base_day = d(2025, 7, 1);
        let days_n = 12usize;
        let per_day = 40usize;
        for offset in 0..days_n {
            let day = base_day + chrono::Days::new(offset as u64);
            for k in 0..per_day {
                let mut e = sample_entry();
                e.timestamp = chrono::Local
                    .from_local_datetime(
                        &(day.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(k as i64)),
                    )
                    .unwrap();
                e.message = format!("d{offset}-k{k}");
                svc.record(&e).await.unwrap();
            }
        }

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 1, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 31, 23, 59, 59)
            .unwrap();

        let full = svc.query(from, to).await.unwrap();
        let total = days_n * per_day;
        assert_eq!(full.len(), total);

        for limit in [1usize, 3, 39, 40, 41, 100, 250, 479, 480, 999] {
            let rows = svc.query_latest(from, to, limit).await.unwrap();
            let want = limit.min(total);
            assert_eq!(rows.len(), want, "limit={limit} 条数不符");
            assert!(
                rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
                "limit={limit} 结果必须按时间升序"
            );
            let expect: Vec<_> = full
                .iter()
                .skip(total - want)
                .map(|e| e.message.clone())
                .collect();
            let got: Vec<_> = rows.iter().map(|e| e.message.clone()).collect();
            assert_eq!(got, expect, "limit={limit} 应等于全量取尾");
        }
    }

    /// 复现线上量级：30 天 × 每天 167 条（≈单文件 18 KB，单块内）
    ///
    /// 与 HTTP 实测同构的场景，用于排查「多天 + 单文件内多次取数」下的
    /// 顺序与等价性，覆盖 limit 落在当天文件内部、跨天、超总量三类边界。
    #[tokio::test]
    async fn test_query_latest_realistic_30days_167perday() {
        let (_dir, svc) = make_service();
        let today = Local::now().date_naive();
        let days_n = 30usize;
        let per_day = 167usize;
        // 按时间升序写入（d=29 最早 → d=0 最新），与真实追加顺序一致
        for offset in (0..days_n).rev() {
            let day = today - chrono::Days::new(offset as u64);
            for k in 0..per_day {
                let mut e = sample_entry();
                e.timestamp = chrono::Local
                    .from_local_datetime(
                        &(day.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(k as i64)),
                    )
                    .unwrap();
                e.message = format!("d{offset}-k{k}");
                svc.record(&e).await.unwrap();
            }
        }

        let to = Local::now();
        let from = to - chrono::Duration::days(30);
        let full = svc.query(from, to).await.unwrap();
        let total = days_n * per_day;
        assert_eq!(full.len(), total, "落盘总数应为 {total}");

        for limit in [
            1usize,
            30,
            100,
            166,
            167,
            168,
            250,
            500,
            2000,
            total,
            total + 100,
        ] {
            let rows = svc.query_latest(from, to, limit).await.unwrap();
            let want = limit.min(total);
            assert_eq!(rows.len(), want, "limit={limit} 条数不符");
            assert!(
                rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
                "limit={limit} 结果必须按时间升序"
            );
            let expect: Vec<_> = full
                .iter()
                .skip(total - want)
                .map(|e| e.message.clone())
                .collect();
            let got: Vec<_> = rows.iter().map(|e| e.message.clone()).collect();
            assert_eq!(got, expect, "limit={limit} 应等于全量取尾");
        }
    }

    /// 物理行序与时间序不一致时，仍须返回正确的最新 N 条
    ///
    /// 这是**乱序追加**的确定性回归锚点（不依赖并发/时序碰运气）：
    /// `record` 每条新开 tokio `File`，而 tokio `File` 含写缓冲、drop 时仅尽力
    /// 异步 flush，因此真实运行中同一文件内的物理顺序并不保证与时间顺序一致。
    /// 早期实现「从文件尾部倒读早停」正是建立在该错误假设上：它会把物理上
    /// 靠后的行当作「更新」，从而漏掉或错选记录（并行测试下曾非确定性复现）。
    ///
    /// 本用例直接以乱序写入文件，把该假设钉死为可确定复现的失败条件。
    #[tokio::test]
    async fn test_query_latest_handles_physically_unsorted_file() {
        let tmp = TempDir::new().unwrap();
        let svc = LoginHistoryService::new(tmp.path());
        let day = d(2025, 7, 9);
        let dir = svc.history_dir();
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // 构造 20 条：时间戳 0..19，但物理写入顺序刻意打乱（逆序 + 交错）
        let mut order: Vec<i64> = (0..20).collect();
        order.reverse();
        let mut jsonl = String::new();
        for sec in &order {
            let mut e = sample_entry();
            e.timestamp = chrono::Local
                .from_local_datetime(
                    &(day.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(*sec)),
                )
                .unwrap();
            e.message = format!("s{sec:02}");
            jsonl.push_str(&serde_json::to_string(&e).unwrap());
            jsonl.push('\n');
        }
        tokio::fs::write(dir.join(format!("{}.jsonl", day.format("%Y-%m-%d"))), jsonl)
            .await
            .unwrap();

        let from = chrono::Local.with_ymd_and_hms(2025, 7, 9, 0, 0, 0).unwrap();
        let to = chrono::Local
            .with_ymd_and_hms(2025, 7, 9, 23, 59, 59)
            .unwrap();

        // query 的既有口径：全量 + 排序
        let full = svc.query(from, to).await.unwrap();
        assert_eq!(full.len(), 20);
        assert!(
            full.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
            "query 应自行排序"
        );

        for limit in [1usize, 3, 10, 19, 20, 50] {
            let rows = svc.query_latest(from, to, limit).await.unwrap();
            let want = limit.min(20);
            assert_eq!(rows.len(), want, "limit={limit} 条数不符");
            assert!(
                rows.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
                "limit={limit} 必须升序（不得依赖物理行序）"
            );
            // 应为时间上最新的 want 条，且 message 与时间戳对应
            let expect: Vec<_> = full
                .iter()
                .skip(20 - want)
                .map(|e| e.message.clone())
                .collect();
            let got: Vec<_> = rows.iter().map(|e| e.message.clone()).collect();
            assert_eq!(got, expect, "limit={limit} 应等于按时间取尾");
            // 交叉校验：message 编号与秒数一致
            for r in &rows {
                let sec = r.timestamp.format("%S").to_string().parse::<u32>().unwrap();
                assert_eq!(r.message, format!("s{sec:02}"), "message 与时间戳须对应");
            }
        }
    }
}
