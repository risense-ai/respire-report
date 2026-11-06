//! 首次目录选择和接入证据：只读本工具元数据；不迁移已有安装、不接触客户端信任。
use crate::commands;
use agentnotify_platform::{paths::PlatformPaths,service};
use agentnotify_integration::inspect::InspectionReport;
use rusqlite::{Connection,OpenFlags};
use serde::Serialize;
use std::{path::{Path,PathBuf},process::{Command,Stdio},time::Duration};

#[derive(Serialize)]
pub struct InstallationInfo {data_dir:PathBuf,installed:bool,can_choose_directory:bool,first_use_prompt: &'static str}
fn unused(root:&Path)->bool{ !["config.json","host.json","installation.json","queue.sqlite","integrations"].iter().any(|p|root.join(p).exists()) }
#[tauri::command]
pub fn installation_info()->Result<InstallationInfo,String>{let p=commands::paths()?;if p.data.exists(){p.secure(false).map_err(|e|e.0.to_owned())?;}let installed=p.data.join("installation.json").exists();if installed{service::owned_launcher(&p).map_err(|e|e.0.to_owned())?;}Ok(InstallationInfo{first_use_prompt:include_str!("../../../../resources/prompts/first-use.md"),can_choose_directory:unused(&p.data),data_dir:p.data,installed})}
/// 仅无已保存设置的新机可选根；不复制旧内容，目标必须不存在，避开运行EXE及所有既有目录。
#[tauri::command]
pub fn select_data_directory(directory:PathBuf,app:tauri::AppHandle)->Result<(),String>{
    let current=commands::paths()?;let _current_security=current.secure(true).map_err(|e|e.0.to_owned())?;let _settings=commands::settings_lock(&current.data)?;if !unused(&current.data){return Err("existing_settings_directory_fixed".into());}
    let target=PlatformPaths::override_data(&directory).map_err(|e|e.0.to_owned())?;
    if target.data.exists(){return Err("new_directory_required".into());}
    if let Some(parent)=target.data.parent(){if !parent.is_dir(){return Err("directory_parent_missing".into());}}
    agentnotify_core::queue::reject_links(&target.data).map_err(|_|"unsafe_data_dir")?;
    let _security=target.secure(true).map_err(|e|e.0.to_owned())?;
    let mut child=Command::new(std::env::current_exe().map_err(|_|"executable_unavailable")?);
    child.arg("--data-dir").arg(&target.data).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]{use std::os::windows::process::CommandExt;child.creation_flags(0x08000000);}
    let mut child=child.spawn().map_err(|_|"desktop_restart_failed")?;
    let deadline=std::time::Instant::now()+Duration::from_secs(10);let ready=target.data.join("gui.ready.json");
    loop {
        if child.try_wait().map_err(|_|"desktop_restart_failed")?.is_some(){return Err("desktop_restart_failed".into());}
        if std::fs::read(&ready).ok().filter(|v|v.len()<=32).and_then(|v|serde_json::from_slice::<u32>(&v).ok())==Some(child.id()){app.exit(0);return Ok(());}
        if std::time::Instant::now()>=deadline {let _=child.kill();let _=child.wait();return Err("desktop_ready_timeout".into());}
        std::thread::sleep(Duration::from_millis(50));
    }
}
/// 已启用状态只从本工具ledger读取，不访问Codex私库，也绝不重新初始化游标。
pub fn observer_enabled(root:&Path,id:uuid::Uuid)->Result<bool,String>{
    let path=root.join(format!("codex-observer-{id}.sqlite"));if !path.exists(){return Ok(false);}
    agentnotify_core::queue::reject_links(&path).map_err(|_|"observer_state_unavailable")?;
    let db=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_|"observer_state_unavailable")?;
    db.busy_timeout(Duration::from_millis(300)).map_err(|_|"observer_state_unavailable")?;
    let value:String=db.query_row("SELECT value FROM meta WHERE key='enabled'",[],|r|r.get(0)).map_err(|_|"observer_state_unavailable")?;
    Ok(value=="1")
}
/// 证据按instance和适配器的已有来源标记分组；历史observer事件不能证明Hook已调用。
pub fn enrich(root:&Path,report:&mut InspectionReport,security:Option<&dyn agentnotify_core::queue::DirectorySecurity>){
    let path=root.join("queue.sqlite");
    let db=if path.exists(){Connection::open_with_flags(&path,OpenFlags::SQLITE_OPEN_READ_ONLY).ok()}else{None};
    if let Some(db)=&db {let _=db.busy_timeout(Duration::from_millis(300));}
    for row in &mut report.clients {
        let Some(id)=row.instance_id else{continue;};
        if row.adapter_id=="codex"{match observer_enabled(root,id){Ok(true)=>row.completion_channel="local_observer".into(),Ok(false)=>{},Err(code)=>{row.reason=code;continue;}}}
        let observer = row.completion_channel == "local_observer";
        if observer {
            // 现有 Hook 归属仍须有效。完成监听只核对原实例、数据目录及当前结构。
            let validation = (|| -> Result<(), &str> {
                if !row.configured { return Err("observer_binding_changed"); }
                let binding = agentnotify_integration::IntegrationManager::new(root.to_path_buf())
                    .lookup(id).map_err(|_| "observer_binding_unavailable")?
                    .ok_or("observer_binding_unavailable")?;
                let home = binding.codex_home.as_deref().ok_or("observer_binding_unavailable")?;
                let security = security.ok_or("observer_binding_unavailable")?;
                agentnotify_adapters::codex_observer::validate_binding(root, id, home, security)
                    .map_err(|error| error.code)
            })();
            row.supported = validation.is_ok();
            (row.status, row.reason) = match validation {
                Ok(()) => (agentnotify_core::contracts::IntegrationStatus::Configured, "只读观察绑定已通过检查。Codex 升级后可点「重新检测」，无需重新准备接入方案。".into()),
                Err(code) => (agentnotify_core::contracts::IntegrationStatus::Degraded, code.into()),
            };
        }
        if let Some(db)=&db {
            let evidence=event_evidence(db,&id.to_string(),&row.adapter_id,observer);
            match evidence {Ok((count,last,sent))=>{row.event_count=count;row.last_event_at=last;row.smtp_accepted_count=sent;row.live_verified=row.configured&&row.supported&&count>0;if row.live_verified && !observer {row.reason="event_recorded".into();}},Err(_) if row.supported=>row.reason="event_evidence_unavailable".into(),Err(_)=>{}}
        }else if path.exists(){row.reason="event_evidence_unavailable".into();}
    }
}
/// 输出父目录由用户选择；专用子目录新建即设置用户ACL，既有不安全目录拒绝接管。
pub fn bundle_parent(selected:Option<PathBuf>)->Result<PathBuf,String>{
    let parent=match selected {Some(p)=>p,None=>std::env::current_exe().map_err(|_|"executable_unavailable")?.parent().ok_or("executable_unavailable")?.to_path_buf()};
    if !parent.is_absolute() || !parent.is_dir() || parent.components().any(|c|matches!(c,std::path::Component::ParentDir)){return Err("bundle_destination_invalid".into());}
    agentnotify_core::queue::reject_links(&parent).map_err(|_|"bundle_parent_unsafe")?;
    let root=parent.join("AgentNotify-assistance");let p=PlatformPaths::override_data(&root).map_err(|e|e.0.to_owned())?;p.secure(true).map_err(|e|e.0.to_owned())?;Ok(root)
}
fn event_evidence(db:&Connection,id:&str,adapter:&str,observer:bool)->rusqlite::Result<(u64,Option<i64>,u64)>{
    db.query_row("SELECT count(*),max(i.received_at),coalesce(sum(CASE WHEN n.status='sent' AND n.send_stage='accepted' THEN 1 ELSE 0 END),0) FROM ingress i JOIN notifications n ON n.id=i.id WHERE i.instance_id=?1 AND json_extract(CAST(i.metadata AS TEXT),'$.adapter_output.notification.adapter_id')=?3 AND CASE WHEN ?2 THEN json_extract(CAST(i.metadata AS TEXT),'$.adapter_output.source_evidence.classification_reason')='codex_local_completion' ELSE coalesce(json_extract(CAST(i.metadata AS TEXT),'$.adapter_output.source_evidence.classification_reason'),'')!='codex_local_completion' END",rusqlite::params![id,observer,adapter],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,u64>(2)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_keeps_instance_adapter_and_channel_separate(){
        let db=Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE ingress(id TEXT,instance_id TEXT,received_at INTEGER,metadata BLOB);CREATE TABLE notifications(id TEXT,status TEXT,send_stage TEXT);").unwrap();
        for (id,instance,adapter,reason,status) in [("a","one","codex","codex_local_completion","sent"),("b","one","codex","codex_local_completion","suppressed"),("c","one","codex","hook","sent"),("d","two","codex","codex_local_completion","sent"),("e","one","generic","hook","sent")] {
            let metadata=serde_json::json!({"adapter_output":{"notification":{"adapter_id":adapter},"source_evidence":{"classification_reason":reason}}}).to_string();
            db.execute("INSERT INTO ingress VALUES(?1,?2,1000,?3)",rusqlite::params![id,instance,metadata]).unwrap();
            db.execute("INSERT INTO notifications VALUES(?1,?2,'accepted')",rusqlite::params![id,status]).unwrap();
        }
        assert_eq!(event_evidence(&db,"one","codex",true).unwrap(),(2,Some(1000),1));
        assert_eq!(event_evidence(&db,"one","codex",false).unwrap(),(1,Some(1000),1));
    }
    #[test]
    fn saved_settings_prevent_first_use_directory_switch(){
        let dir=tempfile::tempdir().unwrap();assert!(unused(dir.path()));
        std::fs::write(dir.path().join("gui.lock"),b"").unwrap();assert!(unused(dir.path()));
        std::fs::write(dir.path().join("host.json"),b"{}").unwrap();assert!(!unused(dir.path()));
    }
}