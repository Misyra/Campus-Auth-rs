//! uv 环境子进程执行封装：取消/超时受控的命令执行与流式输出
//!
//! [`command_output_with_cancel`] 收集完整 `Output`；[`command_output_streaming`]
//! 双管道逐行透出（防全缓冲"假死"与内存膨胀）；[`uv_command`] 统一构造
//! kill_on_drop 的 uv 子进程命令（Windows 上隐藏控制台窗口）。

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// 等待环境子进程，同时响应用户取消与阶段超时。
///
/// `kill_on_drop(true)` 保证 select/timeout 放弃 `output()` future 时，已经 spawn 的
/// 子进程不会脱离 Rust 任务继续占用 `.venv`、缓存目录或安装锁。
/// 三态失败原因：`Cancelled` 为取消令牌先于超时触发（用户主动取消）；
/// `Timeout` 为阶段超时（子进程未在时限内退出）；`Io` 为进程创建/等待本身的 IO 失败。
#[derive(Debug)]
pub(crate) enum CommandOutputError {
    Cancelled,
    Timeout,
    Io(std::io::Error),
}

/// spawn 环境子进程并等待其完整退出，收集 stdout/stderr 与退出码（`Output`）。
///
/// 取消令牌与超时先到者胜（`biased` 下取消优先判定），失败三态见
/// [`CommandOutputError`]；select/timeout 放弃等待时由 `kill_on_drop` 终止子进程。
pub(crate) async fn command_output_with_cancel(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<std::process::Output, CommandOutputError> {
    cmd.kill_on_drop(true);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(CommandOutputError::Cancelled),
        result = tokio::time::timeout(timeout, cmd.output()) => match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(CommandOutputError::Io(error)),
            Err(_) => Err(CommandOutputError::Timeout),
        },
    }
}
/// 流式输出上限：stdout/stderr 各自最多保留字节数（Playwright 安装输出可达
/// 数 MB，全缓冲既"假死"又吃内存；超限后仍透出回调，仅截断最终 `Output`）。
const STREAM_CAPTURE_CAP: usize = 256 * 1024;

/// 有上限累积输出：总量超 [`STREAM_CAPTURE_CAP`] 后丢弃新字节（回调不受影响）。
fn push_capped(buf: &mut Vec<u8>, bytes: &[u8]) {
    let room = STREAM_CAPTURE_CAP.saturating_sub(buf.len());
    if room == 0 {
        return;
    }
    buf.extend_from_slice(&bytes[..bytes.len().min(room)]);
}

/// 单行切段上限：`\r` 进度行无换行，超限即切段透出，防行缓冲无界增长。
const LINE_SPLIT_CAP: usize = 1024 * 1024;

/// 透出无换行尾行（EOF/读错时）：非空才回调并清空，不重复累积。
fn flush_tail(line: &mut String, on_line: &mut impl FnMut(&str)) {
    if !line.trim_end().is_empty() {
        on_line(line.trim_end());
    }
    line.clear();
}

/// 终止子进程整棵树：Windows 先 `taskkill /T`（uv 拉起的 node/下载孙进程否则残留
/// 并咬住缓存锁），再 `start_kill` 兜底；均为 best-effort。
pub(crate) fn kill_process_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(id) = child.id() {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &id.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn();
    }
    let _ = child.start_kill();
}

/// 执行长耗时子进程：双管道流式透出（防全缓冲"假死"与内存膨胀），超时/取消时杀整棵树。
///
/// `on_line` 逐行回调（含 stderr 行，不带行尾换行）；返回的 `Output` 与
/// [`command_output_with_cancel`] 同构（超 [`STREAM_CAPTURE_CAP`] 截断）。
pub(crate) async fn command_output_streaming(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(&str),
) -> Result<std::process::Output, CommandOutputError> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.kill_on_drop(true);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(CommandOutputError::Io)?;

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout 已管道化"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr 已管道化"));
    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    let mut out_line = String::new();
    let mut err_line = String::new();
    let mut out_eof = false;
    let mut err_eof = false;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    // 双管道必须同时排空，否则子进程写满管道缓冲即阻塞；任一 EOF 后只读另一侧。
    while !(out_eof && err_eof) {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                kill_process_tree(&mut child);
                return Err(CommandOutputError::Cancelled);
            }
            _ = &mut deadline => {
                kill_process_tree(&mut child);
                return Err(CommandOutputError::Timeout);
            }
            r = stdout.read_line(&mut out_line), if !out_eof => {
                match r {
                    Ok(0) => {
                        flush_tail(&mut out_line, &mut on_line);
                        out_eof = true;
                    }
                    Ok(_) => {
                        push_capped(&mut out_buf, out_line.as_bytes());
                        // 正常行（`\n` 结尾）即时回调；`\r` 进度行无换行，超限切段透出。
                        if out_line.ends_with('\n') || out_line.len() > LINE_SPLIT_CAP {
                            on_line(out_line.trim_end());
                            out_line.clear();
                        }
                    }
                    Err(_) => {
                        push_capped(&mut out_buf, out_line.as_bytes());
                        flush_tail(&mut out_line, &mut on_line);
                        out_eof = true;
                    }
                }
            }
            r = stderr.read_line(&mut err_line), if !err_eof => {
                match r {
                    Ok(0) => {
                        flush_tail(&mut err_line, &mut on_line);
                        err_eof = true;
                    }
                    Ok(_) => {
                        push_capped(&mut err_buf, err_line.as_bytes());
                        // 正常行（`\n` 结尾）即时回调；`\r` 进度行无换行，超限切段透出。
                        if err_line.ends_with('\n') || err_line.len() > LINE_SPLIT_CAP {
                            on_line(err_line.trim_end());
                            err_line.clear();
                        }
                    }
                    Err(_) => {
                        push_capped(&mut err_buf, err_line.as_bytes());
                        flush_tail(&mut err_line, &mut on_line);
                        err_eof = true;
                    }
                }
            }
        }
    }

    // 子进程可以主动关闭 stdout/stderr 后继续存活；管道 EOF 不代表进程已经退出。
    // `wait` 必须继续受同一个总 deadline 与取消令牌约束，否则会绕过上面的保护永久挂起。
    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            kill_process_tree(&mut child);
            return Err(CommandOutputError::Cancelled);
        }
        _ = &mut deadline => {
            kill_process_tree(&mut child);
            return Err(CommandOutputError::Timeout);
        }
        result = child.wait() => result.map_err(CommandOutputError::Io)?,
    };
    Ok(std::process::Output {
        status,
        stdout: out_buf,
        stderr: err_buf,
    })
}

/// 构造 uv 子进程 Command（Windows 上隐藏控制台窗口，避免环境引导弹黑窗）
pub(crate) fn uv_command(uv_exe: &std::path::Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(uv_exe);
    // Tokio 默认 drop Child 不会结束子进程；环境安装命令一律在 future 被取消/超时
    // 时终止，避免后台残留 uv/playwright 继续修改同一环境。
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellable_command_child_process() {
        let Ok(marker) = std::env::var("CAMPUS_AUTH_CANCEL_CHILD_MARKER") else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_secs(3));
        std::fs::write(marker, b"finished").unwrap();
    }

    #[tokio::test]
    async fn test_command_output_with_cancel_kills_inflight_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child-finished.marker");
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("cancellable_command_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_CANCEL_CHILD_MARKER", &marker);

        let cancel = CancellationToken::new();
        let cancel_later = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel_later.cancel();
        });

        let started = std::time::Instant::now();
        let result =
            command_output_with_cancel(cmd, std::time::Duration::from_secs(10), &cancel).await;
        assert!(matches!(result, Err(CommandOutputError::Cancelled)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "取消应快速结束等待"
        );

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(!marker.exists(), "被取消的子进程不得继续运行到写入完成标记");
    }

    #[test]
    fn streaming_lines_child_process() {
        if let Ok(secs) = std::env::var("CAMPUS_AUTH_STREAM_SLEEP_SECS")
            && let Ok(secs) = secs.parse::<u64>()
        {
            std::thread::sleep(std::time::Duration::from_secs(secs));
        }
        let Ok(count) = std::env::var("CAMPUS_AUTH_STREAM_LINES") else {
            return;
        };
        let Ok(count) = count.parse::<usize>() else {
            return;
        };
        for i in 0..count {
            println!("stream-line-{i}");
        }
        eprintln!("stream-err-done");
        // 注意：此处故意不写无换行尾行——子进程与 libtest harness 共享
        // stdout 时，尾字节曾以约 5% 概率丢失（stdout/stderr、flush、sleep、
        // exit 组合均复现过，属 harness 共享输出的脆弱性）。无换行尾行改由
        // 下面的 flush_tail 纯函数单测确定性覆盖；本用例只断言管道行回调与 Output 组装。
    }

    /// 流式执行：逐行回调（stdout + stderr）与最终 Output 均完整。
    #[tokio::test]
    async fn test_command_output_streaming_captures_lines() {
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("streaming_lines_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_STREAM_LINES", "50");

        let cancel = CancellationToken::new();
        let mut lines: Vec<String> = Vec::new();
        let result =
            command_output_streaming(cmd, std::time::Duration::from_secs(30), &cancel, |line| {
                lines.push(line.to_string())
            })
            .await;
        let output = result.expect("流式执行应成功");
        assert!(output.status.success());
        // 子进程即测试二进制自身，harness 自身输出也会进管道，只断言超集。
        for i in 0..50 {
            assert!(
                lines.iter().any(|l| l == &format!("stream-line-{i}")),
                "缺 stdout 行 stream-line-{i}"
            );
        }
        assert!(lines.iter().any(|l| l == "stream-err-done"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("stream-line-49"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("stream-err-done"));
    }

    /// 无换行尾行透出（`flush_tail`，`command_output_streaming` 的 EOF 兜底）：
    /// 子进程侧的尾行覆盖曾因 harness 共享输出 flake，现改由纯函数单测锁定。
    #[test]
    fn test_flush_tail_emits_nonempty_remainder() {
        let mut got = Vec::new();
        let mut pending = String::from("stream-tail-nonewline");
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got, vec!["stream-tail-nonewline"]);
        assert!(pending.is_empty(), "透出后必须清空，避免 EOF 重复累积");
        // 二次调用不再重复回调
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn test_flush_tail_skips_blank_remainder() {
        let mut got = Vec::new();
        for blank in ["", "   ", "\n", "  \n  "] {
            let mut pending = blank.to_string();
            flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
            assert!(pending.is_empty());
        }
        assert!(got.is_empty(), "空白尾行不得回调");
    }

    #[test]
    fn test_flush_tail_trims_trailing_whitespace() {
        let mut got = Vec::new();
        let mut pending = String::from("tail-with-cr\r");
        flush_tail(&mut pending, &mut |line| got.push(line.to_string()));
        assert_eq!(got, vec!["tail-with-cr"]);
    }

    /// 流式执行超时：快速返回 Timeout（超时路径同样走进程树 kill）。
    #[tokio::test]
    async fn test_command_output_streaming_timeout() {
        let test_exe = std::env::current_exe().unwrap();
        let mut cmd = uv_command(&test_exe);
        cmd.arg("streaming_lines_child_process")
            .arg("--nocapture")
            .env("CAMPUS_AUTH_STREAM_SLEEP_SECS", "30")
            .env("CAMPUS_AUTH_STREAM_LINES", "1");

        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let result =
            command_output_streaming(cmd, std::time::Duration::from_secs(2), &cancel, |_| {}).await;
        assert!(matches!(result, Err(CommandOutputError::Timeout)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "超时应快速结束等待"
        );
    }

    /// 即使子进程提前关闭两个输出管道，后续 `wait` 仍受总超时约束。
    #[tokio::test]
    async fn test_command_output_streaming_timeout_after_both_pipes_close() {
        #[cfg(windows)]
        let cmd = {
            let mut cmd = uv_command(std::path::Path::new("powershell.exe"));
            cmd.args([
                "-NoProfile",
                "-Command",
                "[Console]::Out.Close(); [Console]::Error.Close(); Start-Sleep -Seconds 30",
            ]);
            cmd
        };
        #[cfg(not(windows))]
        let cmd = {
            let mut cmd = uv_command(std::path::Path::new("sh"));
            cmd.args(["-c", "exec 1>&-; exec 2>&-; sleep 30"]);
            cmd
        };

        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let result =
            command_output_streaming(cmd, std::time::Duration::from_secs(1), &cancel, |_| {}).await;
        assert!(matches!(result, Err(CommandOutputError::Timeout)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "管道 EOF 后的进程等待必须受总超时约束"
        );
    }
}
