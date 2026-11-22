//! 人话化 IO 错误文案构造（原 tasks 域范式，T9-1 起全域复用）
//!
//! 把「裸 `std::io` 原文直接抛给用户」统一改为「动作 + 路径 + 失败类别 + 原始错误」
//! 的中文文案：`ErrorKind` 原样保留（按类别编程分支的调用方不受影响），原始错误
//! 文本追加在文案末尾（排障链路仍能拿到 `std::io` 原文）。
//!
//! 本模块只做文案包装，不定义错误类型；各域错误枚举（`TaskError::IoError` 等）
//! 的构造点必须经 `io_error_at` / `io_error` 包装，不得把裸 `std::io` 原文
//! 直接抛给用户。

use std::path::Path;

/// IO 失败类别 → 中文短语（供 [`io_error_at`] / [`io_error`] 拼接用户可读文案）
///
/// 覆盖文件/进程/网络场景会真实出现的类别，未列出的走兜底「系统 IO 异常」；原始
/// 错误文本始终追加在文案末尾，按 `ErrorKind` 编程分支的调用方不受影响。
pub(crate) fn io_kind_label(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::NotFound => "文件或目录不存在",
        K::PermissionDenied => "没有访问权限",
        K::AlreadyExists => "目标已存在",
        K::InvalidInput => "路径或参数不合法",
        K::InvalidData => "数据格式不合法",
        K::UnexpectedEof => "数据意外截断",
        K::WriteZero => "写入失败",
        K::Interrupted => "操作被中断",
        K::TimedOut => "操作超时",
        K::StorageFull => "磁盘空间不足",
        K::ReadOnlyFilesystem => "存储介质只读",
        K::IsADirectory => "目标是目录而非文件",
        K::NotADirectory => "路径中混有非目录项",
        K::DirectoryNotEmpty => "目录非空",
        // 网络类别（monitor/engine 的 TCP 探测、网卡出口等场景真实出现）
        K::ConnectionRefused => "对方拒绝连接",
        K::ConnectionReset => "连接被重置",
        K::ConnectionAborted => "连接被对方中止",
        K::NotConnected => "连接未建立",
        K::BrokenPipe => "连接已断开",
        K::AddrInUse => "地址已被占用",
        K::AddrNotAvailable => "地址不可用",
        K::NetworkUnreachable => "网络不可达",
        K::HostUnreachable => "主机不可达",
        K::NetworkDown => "网络已断开",
        _ => "系统 IO 异常",
    }
}

/// 把底层 IO 错误包装成用户可读文案：动作 + 路径 + 失败类别 + 原始错误
///
/// `ErrorKind` 原样保留（按类别分支的代码不受影响），原始错误文本追加在末尾
/// （日志/调试链路仍能拿到 `std::io` 原文）。
pub(crate) fn io_error_at(action: &str, path: &Path, e: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        e.kind(),
        format!(
            "{action}失败（{}）：{}（{e}）",
            path.display(),
            io_kind_label(e.kind())
        ),
    )
}

/// 同 [`io_error_at`]，用于无固定路径的场景（临时文件、子进程管道等）
pub(crate) fn io_error(action: &str, e: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        e.kind(),
        format!("{action}失败：{}（{e}）", io_kind_label(e.kind())),
    )
}

/// 只产出「失败类别（原始错误）」原因短语的包装，不含动作前缀
///
/// 供错误枚举 Display 已自带动作标签的变体（如 `EnvironmentError::StateIo`、
/// `UpdaterError::StagingDirCreateFailed`）在构造点包装载荷：动作由枚举 Display
/// 表达，这里只补失败类别与原始错误，避免文案出现两次动作。`ErrorKind` 原样保留。
pub(crate) fn io_error_reason(e: std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), format!("{}（{e}）", io_kind_label(e.kind())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文案形状：动作 + 路径 + 类别 + 原始错误；`ErrorKind` 必须原样保留。
    #[test]
    fn test_io_error_at_shape_and_kind_preserved() {
        // ErrorKind 构造出的原始文案跨平台一致（与具体 OS 无关）
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let wrapped = io_error_at("读取任务文件", Path::new("/data/tasks/a.json"), e);
        assert_eq!(
            wrapped.to_string(),
            "读取任务文件失败（/data/tasks/a.json）：没有访问权限（permission denied）"
        );
        // ErrorKind 透传：按类别分支的调用方不受文案包装影响
        assert_eq!(wrapped.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// 无路径形状：动作 + 类别 + 原始错误。
    #[test]
    fn test_io_error_shape() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let wrapped = io_error("写入脚本临时文件", e);
        assert_eq!(
            wrapped.to_string(),
            "写入脚本临时文件失败：文件或目录不存在（entity not found）"
        );
        assert_eq!(wrapped.kind(), std::io::ErrorKind::NotFound);
    }

    /// 常见类别中文标签全覆盖（文件 + 网络，未列出的走兜底）。
    #[test]
    fn test_io_kind_label_covers_common_file_kinds() {
        use std::io::ErrorKind;
        assert_eq!(io_kind_label(ErrorKind::NotFound), "文件或目录不存在");
        assert_eq!(io_kind_label(ErrorKind::PermissionDenied), "没有访问权限");
        assert_eq!(io_kind_label(ErrorKind::IsADirectory), "目标是目录而非文件");
        assert_eq!(io_kind_label(ErrorKind::ConnectionRefused), "对方拒绝连接");
        assert_eq!(io_kind_label(ErrorKind::NetworkUnreachable), "网络不可达");
        assert_eq!(io_kind_label(ErrorKind::Other), "系统 IO 异常");
    }
}
