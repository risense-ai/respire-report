//! 候选发现只读取路径与版本，不把配置文件存在、信任或SMTP成功混作真实入站。
use crate::{IntegrationManager, compatibility, binding_active};
use agentnotify_core::contracts::IntegrationStatus;
use serde::{Serialize,Deserialize};
use std::{env,path::{Path,PathBuf},process::{Command,Stdio},time::{Duration,Instant},thread};
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryOverride { pub adapter_id:String,pub config_dir:PathBuf,pub executable:Option<PathBuf> }
#[derive(Debug,Serialize)]
pub struct InspectedClient {
    pub adapter_id:String,pub config_path:Option<PathBuf>,pub status:IntegrationStatus,pub reason:String,
    pub candidate_id:String,pub instance_id:Option<uuid::Uuid>,pub executable:Option<PathBuf>,pub version:Option<String>,pub supported:bool,
    pub completion_channel:String,pub configured:bool,pub event_count:u64,pub last_event_at:Option<i64>,pub smtp_accepted_count:u64,pub live_verified:bool,
}
#[derive(Debug,Serialize)]
pub struct InspectionReport {pub schema_version:u8,pub clients:Vec<InspectedClient>,pub checked_at:String}
/// 只执行已找到的绝对EXE和固定参数，三秒截止；不通过shell执行用户路径。
fn capture(exe:&Path,args:&[&str])->Option<Vec<u8>> {
    use std::io::Read;
    // 临时输出文件避免后代继承pipe使wait_with_output越过deadline；只读至64KiB。
    let mut output=tempfile::NamedTempFile::new().ok()?;
    let mut command=Command::new(exe);command.args(args).stdin(Stdio::null()).stdout(Stdio::from(output.reopen().ok()?)).stderr(Stdio::null());
    #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
    let mut child=command.spawn().ok()?;let deadline=Instant::now()+Duration::from_secs(3);
    loop {if output.as_file().metadata().ok()?.len()>65536 {let _=child.kill();let _=child.wait();return None;}match child.try_wait(){Ok(Some(s)) if s.success()=>break,Ok(Some(_))=>return None,Ok(None) if Instant::now()<deadline=>thread::sleep(Duration::from_millis(20)),_=>{let _=child.kill();let _=child.wait();return None;}}}
    let mut bytes=Vec::new();output.as_file_mut().take(65537).read_to_end(&mut bytes).ok()?;if bytes.len()>65536{return None;}Some(bytes)
}
fn version(exe:&Path,adapter:&str)->Option<String>{
    if adapter == "workbuddy" {
        return crate::workbuddy::version(exe);
    }
    if !exe.is_absolute() || !exe.is_file() || exe.extension()?.to_str()?.to_ascii_lowercase()!="exe" {return None;}
    agentnotify_core::queue::reject_links(exe).ok()?;
    let raw=capture(exe,&["--version"])?;if raw.len()>512{return None;}
    let text=std::str::from_utf8(&raw).ok()?;
    if !text.contains(if adapter=="codex"{"codex-cli"}else{"Claude Code"}){return None;}
    text.split_whitespace().map(|s|s.trim_matches(|c:char|matches!(c,'('|')'|','))).find(|s|s.as_bytes().first().is_some_and(u8::is_ascii_digit)).map(str::to_owned)
}
fn candidates(adapter:&str)->Vec<PathBuf>{
    let name=match adapter {"codex"=>"codex.exe","workbuddy"=>"WorkBuddy.exe",_=>"claude.exe"};let mut paths=Vec::new();
    if let Some(path)=env::var_os("PATH"){for part in env::split_paths(&path){let p=part.join(name);if p.is_absolute()&&p.is_file(){paths.push(p);}}}
    if let Some(profile)=env::var_os("USERPROFILE"){let p=PathBuf::from(profile).join(".local/bin").join(name);if p.is_file(){paths.push(p);}}
    #[cfg(windows)] {
        if adapter == "workbuddy" {
            // 标准安装路径优先；自定义位置也可从运行进程、卸载登记或高级选项识别。
            for key in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(root) = env::var_os(key) {
                    let root = PathBuf::from(root);
                    for relative in ["WorkBuddy", "Programs/WorkBuddy", "Tencent/WorkBuddy"] {
                        let candidate = root.join(relative).join(name);
                        if candidate.is_file() { paths.push(candidate); }
                    }
                }
            }
        }
        // 桌面进程和安装登记提供候选，随后验证产品与精确版本。
        let script=if adapter=="workbuddy" {
            "$p=@(Get-CimInstance Win32_Process -Filter \"Name='WorkBuddy.exe'\" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty ExecutablePath); foreach($key in @('HKCU:/Software/Microsoft/Windows/CurrentVersion/Uninstall/*','HKLM:/Software/Microsoft/Windows/CurrentVersion/Uninstall/*','HKLM:/Software/WOW6432Node/Microsoft/Windows/CurrentVersion/Uninstall/*')) { Get-ItemProperty -Path $key -ErrorAction SilentlyContinue | Where-Object DisplayName -eq 'WorkBuddy' | ForEach-Object { if($_.InstallLocation){$p+=Join-Path $_.InstallLocation 'WorkBuddy.exe'} } }; @($p | Where-Object {$_ -and (Test-Path -LiteralPath $_ -PathType Leaf)} | Select-Object -Unique -First 16) | ConvertTo-Json -Compress"
        } else if adapter=="codex" {"$p=@(Get-CimInstance Win32_Process -Filter \"Name='codex.exe'\" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty ExecutablePath); Get-AppxPackage '*Codex*' -ErrorAction SilentlyContinue | ForEach-Object { $r=$_.InstallLocation; foreach($s in @('app/resources/codex.exe','resources/codex.exe','codex.exe')) { $f=Join-Path $r $s; if(Test-Path -LiteralPath $f){$p+=$f} } }; @($p | Where-Object {$_} | Select-Object -Unique -First 16) | ConvertTo-Json -Compress"} else {"@(Get-CimInstance Win32_Process -Filter \"Name='claude.exe'\" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty ExecutablePath -Unique -ErrorAction SilentlyContinue | Where-Object {$_} | Select-Object -First 16) | ConvertTo-Json -Compress"};
        if let Some(system)=env::var_os("SystemRoot"){if let Some(raw)=capture(&PathBuf::from(system).join("System32/WindowsPowerShell/v1.0/powershell.exe"),&["-NoProfile","-NonInteractive","-Command",script]) {if let Ok(value)=serde_json::from_slice::<serde_json::Value>(&raw){match value{serde_json::Value::String(s)=>paths.push(s.into()),serde_json::Value::Array(a)=>paths.extend(a.into_iter().filter_map(|v|v.as_str().map(PathBuf::from))),_=>{}}}}}
    }
    paths.sort();paths.dedup();paths.truncate(16);paths
}
pub fn inspect(manager:&IntegrationManager)->InspectionReport{inspect_with(manager,&[])}
pub fn inspect_with(manager:&IntegrationManager,overrides:&[DiscoveryOverride])->InspectionReport{
    let bindings=manager.bindings();let mut clients=Vec::new();
    for adapter in ["codex","claude_code","workbuddy"] {
        let registered=bindings.as_ref().ok().map(|b|b.iter().filter(|b|b.adapter_id==adapter).collect::<Vec<_>>()).unwrap_or_default();
        let mut inputs:Vec<(PathBuf,Option<PathBuf>,Option<uuid::Uuid>)>=registered.iter().map(|b|(b.config_path.clone(),Some(b.client_executable.clone()),Some(b.instance_id))).collect();
        for o in overrides.iter().filter(|o|o.adapter_id==adapter){inputs.push((o.config_dir.join(if adapter=="codex"{"hooks.json"}else{"settings.json"}),o.executable.clone(),None));}
        if inputs.is_empty() {
            // 配置目录由对应产品环境变量覆盖；WorkBuddy 不借用 Claude 或 CodeBuddy 的目录。
            let (variable, default, file) = match adapter {
                "codex" => ("CODEX_HOME", ".codex", "hooks.json"),
                "workbuddy" => ("WORKBUDDY_CONFIG_DIR", ".workbuddy", "settings.json"),
                _ => ("CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
            };
            let home = env::var_os(variable).map(PathBuf::from)
                .or_else(|| env::var_os("USERPROFILE").map(|path| PathBuf::from(path).join(default)));
            if let Some(home) = home { inputs.push((home.join(file),None,None)); }
        }
        let discovered=if inputs.iter().any(|(_,exe,_)|exe.is_none()){candidates(adapter)}else{Vec::new()};
        for (config,exe,id) in inputs {let choices=exe.map(|p|vec![Some(p)]).unwrap_or_else(||if discovered.is_empty(){vec![None]}else{discovered.iter().cloned().map(Some).collect()});
            for executable in choices {let version=executable.as_deref().and_then(|p|version(p,adapter));let supported=version.as_deref().is_some_and(|v|compatibility::find(adapter,v).is_ok());
                let version_matches=id.and_then(|id|registered.iter().find(|b|b.instance_id==id)).is_none_or(|b|version.as_deref()==Some(b.product_version.as_str()));
                let supported=supported && version_matches;
                let configured=id.and_then(|id|registered.iter().find(|b|b.instance_id==id)).is_some_and(|b|binding_active(b).unwrap_or(false));
                let detected=config.exists()||executable.is_some();let status=if !detected{IntegrationStatus::NotDetected}else if version.is_none(){IntegrationStatus::Detected}else if !supported{IntegrationStatus::UnsupportedVersion}else if configured{IntegrationStatus::Configured}else{IntegrationStatus::Detected};
                let reason=if bindings.is_err(){"binding_state_unavailable"}else if !detected{"not_detected"}else if version.is_none(){"version_unavailable"}else if !supported{"unsupported_version"}else if configured{"configured_event_pending"}else{"ready_to_configure"};
                let candidate_id=crate::hash(format!("{adapter}|{}|{}",config.display(),executable.as_deref().unwrap_or(Path::new("")).display()).as_bytes());
                clients.push(InspectedClient{adapter_id:adapter.into(),config_path:Some(config.clone()),status,reason:reason.into(),candidate_id,instance_id:id,executable,version,supported,completion_channel:if configured{"hook"}else{"unavailable"}.into(),configured,event_count:0,last_event_at:None,smtp_accepted_count:0,live_verified:false});
            }
        }
    }
    for (adapter,status,reason,channel) in [("generic",IntegrationStatus::Detected,"explicit_emit_only","explicit_emit")] {clients.push(InspectedClient{adapter_id:adapter.into(),config_path:None,status,reason:reason.into(),candidate_id:adapter.into(),instance_id:None,executable:None,version:None,supported:false,completion_channel:channel.into(),configured:false,event_count:0,last_event_at:None,smtp_accepted_count:0,live_verified:false});}
    InspectionReport{schema_version:1,clients,checked_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().to_string()}
}