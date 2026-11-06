use crate::{IntegrationError, Result};
use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

const HOOK_TIMEOUT_SECONDS: u8 = 5;
const HOOK_MARKER: &str = " hook workbuddy --instance-id ";

/// 从受管 Launcher 与私有数据目录生成 WorkBuddy 的 Windows 命令行。
pub(crate) fn command(launcher: &Path, data_dir: &Path, instance: Uuid) -> Result<String> {
    let system = std::env::var_os("SystemRoot").ok_or(IntegrationError::new("unsafe_command"))?;
    let shell = Path::new(&system).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !shell.is_absolute() || !shell.is_file() {
        return Err(IntegrationError::new("unsafe_command"));
    }

    // WorkBuddy 会在解析 argv 前展开 ${...}；含该字面量的路径不能无歧义地接入。
    let command_path = |path: &Path| -> Result<String> {
        let path = path.to_str().ok_or(IntegrationError::new("unsafe_command"))?;
        if path.contains(['"', '\r', '\n']) || path.contains("${") {
            return Err(IntegrationError::new("unsafe_command"));
        }
        Ok(path.replace('\\', "/").replace('\'', "''"))
    };
    let shell = shell.to_str().ok_or(IntegrationError::new("unsafe_command"))?;
    if shell.contains(['"', '\r', '\n']) || shell.contains("${") {
        return Err(IntegrationError::new("unsafe_command"));
    }
    // 5.9.3 直接执行 Windows PowerShell，保留回调环境，不经默认 Git Bash。
    let launcher = command_path(launcher)?;
    let data_dir = command_path(data_dir)?;
    // GUI 子系统 Launcher 可能让 PowerShell 提前退出；显式等待以保留可核验的父链。
    // 直接复制输入字节，避免 PowerShell 文本管道转换 JSON 的编码。
    Ok(format!(concat!(
        r#""{0}" -NoProfile -NonInteractive -Command "$ErrorActionPreference='Stop'; "#,
        r#"$start=[Diagnostics.ProcessStartInfo]::new('{1}'); $quote=[char]34; "#,
        r#"$start.Arguments='--data-dir '+$quote+'{2}'+$quote+'{3}{4}'; "#,
        r#"$start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.RedirectStandardInput=$true; "#,
        r#"$child=[Diagnostics.Process]::Start($start); try {{ [Console]::OpenStandardInput().CopyTo($child.StandardInput.BaseStream) }} "#,
        r#"finally {{ $child.StandardInput.BaseStream.Close(); $child.WaitForExit() }}; exit $child.ExitCode""#,
    ), shell, launcher, data_dir, HOOK_MARKER, instance))
}

/// 生成和检查同一条正式回调；超时单位为 WorkBuddy 配置的秒。
pub(crate) fn handler(command: &str) -> Value {
    json!({"type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECONDS})
}

/// 识别同一设置文件内的 AgentNotify WorkBuddy 回调，用于拒绝同一来源多次接入。
pub(crate) fn owned_count(groups: &[Value]) -> usize {
    groups.iter()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter(|item| item.get("command").and_then(Value::as_str)
            .is_some_and(|command| command.contains(HOOK_MARKER)))
        .count()
}


/// 只读检查 WorkBuddy 桌面 EXE 的 PE 产品资源，不启动客户端。
#[cfg(windows)]
pub(crate) fn version(exe: &Path) -> Option<String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr, slice};

    #[link(name = "version")]
    unsafe extern "system" {
        fn GetFileVersionInfoSizeW(filename: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(filename: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
        fn VerQueryValueW(data: *const c_void, key: *const u16, value: *mut *mut c_void, len: *mut u32) -> i32;
    }

    if !exe.is_absolute() || !exe.is_file()
        || !exe.file_name()?.to_str()?.eq_ignore_ascii_case("WorkBuddy.exe")
    {
        return None;
    }
    agentnotify_core::queue::reject_links(exe).ok()?;

    // 1. 资源来自指定的普通 EXE，查询缓冲区限制为 256 KiB。
    let filename: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: 文件名为 NUL 结尾的 Windows 宽字符串，大小回填位置有效。
    let size = unsafe { GetFileVersionInfoSizeW(filename.as_ptr(), &mut 0) };
    if size == 0 || size > 256 * 1024 {
        return None;
    }
    let mut data = vec![0u32; (size as usize).div_ceil(4)];
    // SAFETY: data 已按请求大小分配，API 不会越过传入长度。
    if unsafe { GetFileVersionInfoW(filename.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }

    // 2. 从随包的英语资源读取产品标识与版本；不能把其他产品的同名 EXE 当作 WorkBuddy。
    let read_string = |translation: &str, field: &str| -> Option<String> {
        let key: Vec<u16> = format!(r"\StringFileInfo\{translation}\{field}").encode_utf16().chain([0]).collect();
        let mut value = ptr::null_mut();
        let mut len = 0;
        // SAFETY: 原缓冲区在查询期间保持不变，两个回填参数均有效。
        if unsafe { VerQueryValueW(data.as_ptr().cast(), key.as_ptr(), &mut value, &mut len) } == 0 {
            return None;
        }
        let start = data.as_ptr().addr();
        let end = value.addr().checked_add((len as usize).checked_mul(2)?)?;
        if value.addr() < start || end > start + size as usize || !value.addr().is_multiple_of(2) {
            return None;
        }
        // SAFETY: 已核对回传地址和长度完全位于有效缓冲区并且宽字符对齐。
        let chars = unsafe { slice::from_raw_parts(value.cast::<u16>(), len as usize) };
        String::from_utf16(chars.strip_suffix(&[0]).unwrap_or(chars)).ok()
    };
    for translation in ["040904b0", "040904e4"] {
        // ProductName 识别产品；ProductVersion 给出版本号，仅去掉 Electron 尾部的构建号 .0。
        if read_string(translation, "ProductName").as_deref() == Some("WorkBuddy") {
            let version = read_string(translation, "ProductVersion")?;
            let components: Vec<_> = version.split('.').collect();
            if !components.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())) {
                return None;
            }
            return Some(match components.as_slice() {
                [major, minor, patch, "0"] => format!("{major}.{minor}.{patch}"),
                _ => version,
            });
        }
    }
    None
}

/// 非 Windows 暂未验证 WorkBuddy 安装识别。
#[cfg(not(windows))]
pub(crate) fn version(_exe: &Path) -> Option<String> {
    None
}
