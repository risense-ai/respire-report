//! 当前用户服务管理：仅精确归属入口，安装/启动只由显式调用触发。
use crate::{
    PlatformError, Result,
    ipc::{self, Request, Response},
    paths::PlatformPaths,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
static UNIFIED_PROCESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// 仅统一程序入口注册自身能力；参数、环境和 Cargo feature 合并都不能冒充此身份。
pub fn register_unified_process() {
    UNIFIED_PROCESS.store(true, std::sync::atomic::Ordering::Relaxed);
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceReport {
    pub installed: bool,
    pub running: bool,
    pub unsupported: bool,
    pub failed: bool,
    pub code: String,
}
/// 缺字段为旧三程序布局；未知布局拒绝，不能按路径猜测并绕过清单完整性。
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeLayout { #[default] Legacy, Unified }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Installation {
    pub(crate) schema_version: u8,
    #[serde(default)]
    pub(crate) runtime_layout: RuntimeLayout,
    pub(crate) id: uuid::Uuid,
    pub(crate) launcher: PathBuf,
    pub(crate) daemon: PathBuf,
    pub(crate) launcher_hash: String,
    pub(crate) daemon_hash: String,
    #[serde(default)]
    pub(crate) cli_hash: String,
    #[serde(default)]
    pub(crate) desktop_hash: String,
}
pub struct ServiceManager {
    paths: PlatformPaths,
}
impl ServiceManager {
    pub fn new(paths: PlatformPaths) -> Self {
        Self { paths }
    }
    pub fn install(&self) -> Result<ServiceReport> {
        self.execute("install")
    }
    pub fn start(&self) -> Result<ServiceReport> {
        self.execute("start")
    }
    pub fn stop(&self) -> Result<ServiceReport> {
        self.execute("stop")
    }
    pub fn status(&self) -> Result<ServiceReport> {
        self.execute("status")
    }
    pub fn uninstall(&self) -> Result<ServiceReport> {
        self.execute("uninstall")
    }
    fn execute(&self, action: &str) -> Result<ServiceReport> {
        #[cfg(not(windows))]
        {
            let _ = action;
            return Ok(ServiceReport {
                installed: false,
                running: false,
                unsupported: true,
                failed: false,
                code: "user_service_native_pending".into(),
            });
        }
        #[cfg(windows)]
        {
            // 1. status无安装时保持只读，不顺带初始化目录。
            if !self.paths.data.exists() && action == "status" { return uninstalled_status(&self.paths); }
            if !self.paths.data.exists() && action != "install" {
                return Ok(ServiceReport {
                    installed: false,
                    running: false,
                    unsupported: false,
                    failed: false,
                    code: "not_installed".into(),
                });
            }
            let security = self.paths.secure(action == "install")?;
            let _lock = management_lock(&self.paths.data)?;
            let manifest = self.paths.data.join("installation.json");
            if action == "install" && !manifest.exists() {
                prepare_install(&self.paths)?;
            }
            if !manifest.exists() && action == "status" { return uninstalled_status(&self.paths); }
            if !manifest.exists() {
                return Err(PlatformError("installation_identity_missing"));
            }
            let install = read_installation(&self.paths)?;
            security
                .verify(&self.paths.data)
                .map_err(|_| PlatformError("unsafe_data_dir"))?;
            // 2. 先验证任务所有权；任何同名陌生任务都不能停止/替换/删除。
            let prior = windows_task(&self.paths, &install, "status")?;
            if prior.failed {
                return Ok(prior);
            }
            if matches!(action, "stop" | "uninstall") {
                match ipc::call(&self.paths, &Request::Stop) {
                    Ok(Response::Ok) => {
                        let end = Instant::now() + Duration::from_secs(45);
                        while sender_locked(&self.paths.data)? {
                            if Instant::now() > end {
                                return Err(PlatformError("shutdown_pending"));
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    Err(_) => {
                        if sender_locked(&self.paths.data)? {
                            return Err(PlatformError("daemon_control_unavailable"));
                        }
                    }
                    _ => return Err(PlatformError("shutdown_rejected")),
                }
            }
            let mut report = windows_task(&self.paths, &install, action)?;
            if action == "install" && !report.failed {
                report = windows_task(&self.paths, &install, "status")?;
            }
            if action == "start" && !report.failed {
                let end = Instant::now() + Duration::from_secs(8);
                while !daemon_running(&self.paths) && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(100));
                }
                if !daemon_running(&self.paths) {
                    report.failed = true;
                    report.code = "daemon_start_unconfirmed".into();
                }
            }
            report.running = daemon_running(&self.paths);
            if !report.running && sender_locked(&self.paths.data)? {
                report.failed = true;
                report.code = "daemon_health_unconfirmed".into();
            }
            Ok(report)
        }
    }
}
/// 首次配置可以先于安装；只有无安装残留、无发送者、无同名任务才报告未安装。
#[cfg(windows)]
fn uninstalled_status(paths: &PlatformPaths) -> Result<ServiceReport> {
    // 1. 缺清单而有安装文件或活动发送者属于损坏/归属未知，不能让UI重新安装覆盖。
    if paths.data.join("launcher.exe").exists() || paths.data.join("AgentNotify.exe").exists() || (paths.data.join("versions").exists() && !completed_uninstall(paths)?) {
        return Err(PlatformError("installation_identity_missing"));
    }
    if daemon_running(paths) || sender_locked(&paths.data)? {
        return Err(PlatformError("daemon_identity_missing"));
    }
    // 2. 只读探测当前用户同名任务；nil标识不用于认可、启动或修改任何任务。
    let probe = Installation {
        schema_version: 1, runtime_layout: RuntimeLayout::Legacy, id: uuid::Uuid::nil(), launcher: paths.data.join("launcher.exe"),
        daemon: PathBuf::new(), launcher_hash: String::new(), daemon_hash: String::new(),
        cli_hash: String::new(), desktop_hash: String::new(),
    };
    windows_task(paths, &probe, "probe-uninstalled")
}
pub fn daemon_running(paths: &PlatformPaths) -> bool {
    matches!(
        ipc::call(paths, &Request::Health),
        Ok(Response::Health { .. })
    )
}
/// wake不会创建安装任务；仅启动已存在且归属匹配的入口。
pub fn wake(paths: &PlatformPaths) -> Result<ServiceReport> {
    ServiceManager::new(paths.clone()).start()
}
/// Hook不等待计划任务启动；独立当前程序执行精确start，不修改注册入口。
pub fn wake_detached(paths: &PlatformPaths) -> Result<()> {
    if !paths.data.join("installation.json").exists() {
        return Err(PlatformError("not_installed"));
    }
    let mut cmd =
        Command::new(std::env::current_exe().map_err(|_| PlatformError("launcher_unavailable"))?);
    cmd.arg("--data-dir")
        .arg(&paths.data)
        .args(["service", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.spawn().map_err(|_| PlatformError("wake_failed"))?;
    Ok(())
}
pub(crate) fn management_lock(root: &Path) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("service.lock"))
        .map_err(|_| PlatformError("service_lock_failed"))?;
    f.try_lock().map_err(|_| PlatformError("service_busy"))?;
    Ok(f)
}
fn sender_locked(root: &Path) -> Result<bool> {
    let path = root.join("sender.lock");
    if !path.exists() {
        return Ok(false);
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| PlatformError("sender_status_unavailable"))?;
    match f.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(_) => Err(PlatformError("sender_status_unavailable")),
    }
}
pub(crate) fn hash(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|_| PlatformError("binary_unavailable"))?;
    let mut digest = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| PlatformError("binary_unavailable"))?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn prepare_install(paths: &PlatformPaths) -> Result<()> {
    // 独立开发 CLI 不包含 GUI/daemon，不能登记成统一发行程序。
    if !UNIFIED_PROCESS.load(std::sync::atomic::Ordering::Relaxed) { return Err(PlatformError("unified_runtime_required")); }
    // 1. 统一发行物本身即运行程序；新安装仅落一个活动 EXE，不寻找或解包相邻程序。
    let source = std::env::current_exe().map_err(|_| PlatformError("launcher_unavailable"))?;
    let runtime = paths.data.join("AgentNotify.exe");
    if runtime.exists() { return Err(PlatformError("unowned_installation_files")); }
    let digest = hash(&source)?;
    let record = Installation {
        schema_version: 1, runtime_layout: RuntimeLayout::Unified,
        id: uuid::Uuid::new_v4(), launcher: runtime.clone(), daemon: runtime.clone(),
        launcher_hash: digest.clone(), daemon_hash: digest, cli_hash: String::new(), desktop_hash: String::new(),
    };
    let mut input = std::fs::File::open(&source).map_err(|_| PlatformError("binary_unavailable"))?;
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&runtime)
        .map_err(|_| PlatformError("install_conflict"))?;
    std::io::copy(&mut input, &mut output).map_err(|_| PlatformError("install_copy_failed"))?;
    output.sync_all().map_err(|_| PlatformError("install_copy_failed"))?;
    // 2. create_new 不接管未登记程序；清单失败保留明确残留，供人工安全恢复。
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .open(paths.data.join("installation.json")).map_err(|_| PlatformError("install_conflict"))?;
    file.write_all(&serde_json::to_vec(&record).map_err(|_| PlatformError("installation_invalid"))?)
        .map_err(|_| PlatformError("install_write_failed"))?;
    file.sync_all().map_err(|_| PlatformError("install_write_failed"))?;
    // 新安装不能沿用上次卸载完成凭证；不以旧身份放行未来损坏状态。
    let receipt=paths.data.join("uninstall-complete.receipt");agentnotify_core::queue::reject_links(&receipt).map_err(|_|PlatformError("unsafe_uninstall_receipt"))?;
    if receipt.exists(){std::fs::remove_file(receipt).map_err(|_|PlatformError("install_write_failed"))?;}Ok(())
}
/// 每种布局限定精确根下路径，旧升级唯一额外别名为历史 launcher.exe。
fn valid_layout(paths: &PlatformPaths, i: &Installation) -> bool {
    match i.runtime_layout {
        RuntimeLayout::Legacy => i.launcher == paths.data.join("launcher.exe") && valid_daemon_path(paths, &i.daemon),
        RuntimeLayout::Unified => i.daemon == paths.data.join("AgentNotify.exe")
            && (i.launcher == i.daemon || i.launcher == paths.data.join("launcher.exe"))
            && i.launcher_hash == i.daemon_hash && i.cli_hash.is_empty() && i.desktop_hash.is_empty(),
    }
}
pub(crate) fn read_installation(paths: &PlatformPaths) -> Result<Installation> {
    let raw = std::fs::read(paths.data.join("installation.json"))
        .map_err(|_| PlatformError("installation_missing"))?;
    if raw.len() > 16384 {
        return Err(PlatformError("installation_invalid"));
    }
    agentnotify_core::queue::reject_links(&paths.data.join("installation.json")).map_err(|_| PlatformError("unsafe_installation"))?;
    let i: Installation =
        serde_json::from_slice(&raw).map_err(|_| PlatformError("installation_invalid"))?;
    agentnotify_core::queue::reject_links(&i.launcher).map_err(|_| PlatformError("unsafe_installation"))?;
    agentnotify_core::queue::reject_links(&i.daemon).map_err(|_| PlatformError("unsafe_installation"))?;
    if i.schema_version != 1
        || !valid_layout(paths, &i)
        || hash(&i.launcher)? != i.launcher_hash
        || hash(&i.daemon)? != i.daemon_hash
        || (!i.cli_hash.is_empty() && hash(&i.daemon.with_file_name("agentnotify.exe"))? != i.cli_hash)
        || (!i.desktop_hash.is_empty() && hash(&i.daemon.with_file_name("agentnotify-desktop.exe"))? != i.desktop_hash)
    {
        return Err(PlatformError("installation_modified"));
    }
    Ok(i)
}
/// 稳定Launcher等待独立daemon结束，使调度器生命周期不脱离真正后台。
pub fn launch(paths: PlatformPaths) -> Result<i32> {
    let _security = paths.secure(false)?;
    let i = read_installation(&paths)?;
    let mut cmd = Command::new(&i.daemon);
    cmd.arg("--data-dir").arg(&paths.data);
    if i.runtime_layout == RuntimeLayout::Unified { cmd.arg("daemon"); }
    cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    Ok(cmd
        .status()
        .map_err(|_| PlatformError("daemon_launch_failed"))?
        .code()
        .unwrap_or(4))
}
#[cfg(windows)]
fn windows_task(paths: &PlatformPaths, i: &Installation, action: &str) -> Result<ServiceReport> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    let script = include_str!("service_windows.ps1");
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let sid = crate::windows::current_sid()?;
    // 路径通过JSON stdin传入脚本，不插值到PowerShell代码。
    let input = serde_json::json!({"action":action,"sid":sid,"id":i.id,"launcher":i.launcher,"root":paths.data,"arguments":format!("--data-dir \"{}\" launch-daemon",paths.data.display())});
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|_| PlatformError("scheduler_unavailable"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&input).unwrap().as_bytes())
        .map_err(|_| PlatformError("scheduler_input_failed"))?;
    let output = child
        .wait_with_output()
        .map_err(|_| PlatformError("scheduler_failed"))?;
    if !output.status.success() {
        return Err(PlatformError("scheduler_failed"));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| PlatformError("scheduler_report_invalid"))
}

/// 版本目录必须是versions/<数字三段版本>/agentnotifyd.exe，不能靠清单指定任意执行路径。
fn valid_daemon_path(paths: &PlatformPaths, path: &Path) -> bool {
    let Some(version) = path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) else { return false; };
    valid_version(version) && path == paths.data.join("versions").join(version).join("agentnotifyd.exe")
}
pub(crate) fn valid_version(version: &str) -> bool {
    version.len() <= 40 && version.split('.').count() == 3 && version.split('.').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()) && p.parse::<u32>().is_ok() && (p == "0" || !p.starts_with('0')))
}
/// 只更新自身安装记录，任务和Hook的Launcher路径/字符串保持不变。
pub(crate) fn switch_installation(paths: &PlatformPaths, record: &Installation) -> Result<()> {
    let _lock = management_lock(&paths.data)?;
    if !valid_layout(paths, record) || hash(&record.launcher)? != record.launcher_hash || hash(&record.daemon)? != record.daemon_hash { return Err(PlatformError("installation_modified")); }
    crate::update::write_json(&paths.data.join("installation.json"), record)
}

/// Hook继续调用稳定launcher.exe，由安装记录定位当前CLI；版本CLI不递归转发。
pub fn dispatch_installed_cli(paths: &PlatformPaths, args: &[String]) -> Result<Option<i32>> {
    if std::env::current_exe().map_err(|_| PlatformError("launcher_unavailable"))? != paths.data.join("launcher.exe") { return Ok(None); }
    let _security = paths.secure(false)?;
    let record = read_installation(paths)?;
    if record.runtime_layout == RuntimeLayout::Unified || record.cli_hash.is_empty() { return Ok(None); }
    let mut command = Command::new(record.daemon.with_file_name("agentnotify.exe"));
    command.args(args);
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    Ok(Some(command.status().map_err(|_| PlatformError("cli_launch_failed"))?.code().unwrap_or(4)))
}

/// 只有已验证归属的安装才能提供持久Hook入口，不能退回临时解压路径。
pub fn owned_launcher(paths: &PlatformPaths) -> Result<PathBuf> {
    let _security = paths.secure(false)?;
    Ok(read_installation(paths)?.launcher)
}
/// GUI从已安装版本启动；未安装时仅允许当前包相邻桌面程序。
pub fn open_desktop(paths: &PlatformPaths) -> Result<()> {
    let desktop = if paths.data.join("installation.json").exists() {
        let _security = paths.secure(false)?;
        let record = read_installation(paths)?;
        if record.runtime_layout == RuntimeLayout::Unified { record.daemon }
        else {
            if record.desktop_hash.is_empty() { return Err(PlatformError("desktop_version_missing")); }
            record.daemon.with_file_name("agentnotify-desktop.exe")
        }
    } else { std::env::current_exe().map_err(|_| PlatformError("launcher_unavailable"))? };
    let mut command = Command::new(&desktop);
    command.arg("--data-dir").arg(&paths.data);
    command.current_dir(desktop.parent().ok_or(PlatformError("desktop_version_missing"))?).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    command.spawn().map_err(|_| PlatformError("desktop_launch_failed"))?;
    Ok(())
}

/// 卸载计划只接收经过安装清单及哈希验证的当前程序文件，不接收任意删除路径。
pub fn uninstall_runtime_files(paths: &PlatformPaths) -> Result<Vec<PathBuf>> {
    let i=read_installation(paths)?;
    let mut files=vec![i.launcher.clone(),i.daemon.clone()];
    if i.runtime_layout==RuntimeLayout::Legacy {
        let parent=i.daemon.parent().ok_or(PlatformError("installation_invalid"))?;
        files.push(parent.join("agentnotify.exe"));files.push(parent.join("agentnotify-desktop.exe"));
    }
    files.sort();files.dedup();Ok(files)
}
/// 只用于已验证本机交付文件的完整性快照，不读取凭据。
pub fn file_digest(path:&Path)->Result<String>{hash(path)}
/// GUI 已退出后报告卸载真实结果；只使用本工具固定文案及本机报告路径。
pub fn show_uninstall_result(completed:bool,report:&Path){
    #[cfg(windows)] {use std::os::windows::ffi::OsStrExt;
        let text=format!("{}\n\n结果报告：{}",if completed{"AgentNotify 已卸载。保留项以卸载预览为准。"}else{"AgentNotify 卸载未完成。请保留数据并交由 Agent 根据结果报告处理；不要重复删除目录。"},report.display());
        let text:Vec<u16>=std::ffi::OsStr::new(&text).encode_wide().chain(Some(0)).collect();let title:Vec<u16>="AgentNotify 卸载".encode_utf16().chain(Some(0)).collect();
        unsafe{windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(std::ptr::null_mut(),text.as_ptr(),title.as_ptr(),windows_sys::Win32::UI::WindowsAndMessaging::MB_OK|windows_sys::Win32::UI::WindowsAndMessaging::MB_SETFOREGROUND);}
    }
    #[cfg(not(windows))] {let _=(completed,report);}
}

/// 仅检查当前用户桌面的固定产品快捷方式；目标和参数均匹配当前安装才归属。
pub fn owned_desktop_shortcut(paths:&PlatformPaths)->Result<Option<PathBuf>> {
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        use std::io::{Read,Seek,SeekFrom};
        let output=tempfile::tempfile().map_err(|_|PlatformError("shortcut_inspect_failed"))?;
        let script=r#"$ErrorActionPreference='Stop'; $r=[Console]::In.ReadToEnd()|ConvertFrom-Json; $p=Join-Path ([Environment]::GetFolderPath('Desktop')) 'AgentNotify.lnk'; if(Test-Path -LiteralPath $p){ $w=New-Object -ComObject WScript.Shell; $s=$w.CreateShortcut($p); $n={param($v) [IO.Path]::GetFullPath($v).TrimEnd('\').ToLowerInvariant()}; $target=&$n $s.TargetPath; $expected=&$n (Join-Path $r.root 'AgentNotify.exe'); $legacy=&$n (Join-Path $r.root 'launcher.exe'); $m=[regex]::Match($s.Arguments,'^--data-dir "([^"\r\n]+)"(?: desktop)?$'); if(($target-eq$expected-or$target-eq$legacy)-and$m.Success-and$m.Groups[1].Value-match'^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+)'-and(&$n $m.Groups[1].Value)-eq(&$n $r.root)){ $p|ConvertTo-Json -Compress } }"#;
        let mut child=Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command",script]).stdin(Stdio::piped()).stdout(output.try_clone().map_err(|_|PlatformError("shortcut_inspect_failed"))?).stderr(Stdio::null()).creation_flags(0x08000000).spawn().map_err(|_|PlatformError("shortcut_inspect_failed"))?;
        if let Some(mut input)=child.stdin.take(){input.write_all(serde_json::json!({"root":paths.data}).to_string().as_bytes()).map_err(|_|PlatformError("shortcut_inspect_failed"))?;}
        let end=Instant::now()+Duration::from_secs(5);loop{match child.try_wait().map_err(|_|PlatformError("shortcut_inspect_failed"))?{Some(s)if s.success()=>break,Some(_)=>return Err(PlatformError("shortcut_inspect_failed")),None if Instant::now()<end=>std::thread::sleep(Duration::from_millis(20)),None=>{let _=child.kill();let _=child.wait();return Err(PlatformError("shortcut_inspect_timeout"));}}}
        let mut output=output;output.seek(SeekFrom::Start(0)).map_err(|_|PlatformError("shortcut_inspect_failed"))?;let mut bytes=Vec::new();output.take(32769).read_to_end(&mut bytes).map_err(|_|PlatformError("shortcut_inspect_failed"))?;
        if bytes.len()>32768{return Err(PlatformError("shortcut_inspect_failed"));}if bytes.iter().all(u8::is_ascii_whitespace){return Ok(None)}
        let text=String::from_utf8(bytes).map_err(|_|PlatformError("shortcut_inspect_failed"))?;let path:PathBuf=serde_json::from_str(text.trim_start_matches('\u{feff}').trim()).map_err(|_|PlatformError("shortcut_inspect_failed"))?;
        agentnotify_core::queue::reject_links(&path).map_err(|_|PlatformError("unsafe_shortcut"))?;Ok(Some(path))
    }
    #[cfg(not(windows))] {let _=paths;Ok(None)}
}

#[cfg(windows)]
fn completed_uninstall(paths:&PlatformPaths)->Result<bool>{
 let Some(value)=crate::uninstall_receipt::installation(paths)?else{return Ok(false)};
 let prior:Installation=serde_json::from_value(value).map_err(|_|PlatformError("uninstall_receipt_invalid"))?;
 if prior.schema_version!=1||prior.id.is_nil()||!valid_layout(paths,&prior)||prior.launcher_hash.len()!=64||prior.daemon_hash.len()!=64{return Err(PlatformError("uninstall_receipt_invalid"));}
 Ok(true)
}
