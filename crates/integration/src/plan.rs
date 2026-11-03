//! 计划只修改本工具拥有的handler；记录原字节、hash、版本和备份位置。
use crate::{IntegrationManager, IntegrationRequest, IntegrationPlan, OperationState, PlanSummary, IntegrationError, Result, compatibility, hash};
use serde_json::{json, Value};
use std::{fs, path::Path, process::{Command,Stdio}, thread, time::{Duration,Instant}};
use uuid::Uuid;
fn safe_path(path:&Path)->bool {
    path.is_absolute() && !path.components().any(|c|matches!(c,std::path::Component::ParentDir))
        && !path.ancestors().filter_map(|p|fs::symlink_metadata(p).ok()).any(|m|m.file_type().is_symlink())
}
fn append_hooks(root:&mut Value, adapter:&str, events:&[String], command:&str, id:Uuid, data_dir:&Path)->Result<()> {
    let obj=root.as_object_mut().ok_or(IntegrationError::new("config_invalid"))?;
    let hooks=obj.entry("hooks").or_insert_with(||json!({}))
        .as_object_mut().ok_or(IntegrationError::new("config_invalid"))?;
    for event in events {
        let list=hooks.entry(event.clone()).or_insert_with(||json!([]))
            .as_array_mut().ok_or(IntegrationError::new("config_invalid"))?;
        let handler=if adapter=="codex" {
            json!({"type":"command","command":command,"commandWindows":command,"timeout":2})
        } else {
            json!({"type":"command","command":command,"args":["--data-dir",data_dir.to_string_lossy(),"hook","claude_code","--instance-id",id.to_string()],"timeout":2})
        };
        let group=json!({"hooks":[handler]});
        if list.iter().any(|v|v==&group) { continue; }
        // 相同实例的旧handler被用户改动时停止，不覆盖或制造双发。
        if list.iter().any(|v|v.to_string().contains(&id.to_string())) {
            return Err(IntegrationError::new("owned_hook_conflict"));
        }
        list.push(group);
    }
    Ok(())
}
fn verify_client_version(request:&IntegrationRequest)->Result<()> {
    // 只执行用户显式指定二进制的--version；有界等待，不启动交互会话。
    let mut child=Command::new(&request.client_executable).arg("--version")
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .map_err(|_|IntegrationError::new("version_unavailable"))?;
    let deadline=Instant::now()+Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success()=>break,
            Ok(Some(_))=>return Err(IntegrationError::new("version_unavailable")),
            Ok(None) if Instant::now()<deadline=>thread::sleep(Duration::from_millis(20)),
            Ok(None)=>{ let _=child.kill(); let _=child.wait(); return Err(IntegrationError::new("version_timeout")); },
            Err(_)=>{ let _=child.kill(); let _=child.wait(); return Err(IntegrationError::new("version_unavailable")); },
        }
    }
    let output=child.wait_with_output().map_err(|_|IntegrationError::new("version_unavailable"))?;
    if output.stdout.len()>512 { return Err(IntegrationError::new("version_unavailable")); }
    let text=std::str::from_utf8(&output.stdout).map_err(|_|IntegrationError::new("version_unavailable"))?;
    let product=if request.adapter_id=="codex" { "codex-cli" } else { "Claude Code" };
    if !text.contains(product) || !text.split_whitespace().any(|part|
        part.trim_matches(|c:char| matches!(c,'('|')'|','))==request.product_version
    ) {
        return Err(IntegrationError::new("version_mismatch"));
    }
    Ok(())
}
pub fn create(manager:&IntegrationManager,mut request:IntegrationRequest)->Result<PlanSummary> {
    // 1. 仅显式文件、已列精确版本可写；WorkBuddy和未知版本只读。
    if request.schema_version!=1 || !matches!(request.adapter_id.as_str(),"codex"|"claude_code")
        || !safe_path(&request.config_path) || !safe_path(&request.client_executable)
        || !safe_path(&request.agentnotify_executable) || !safe_path(&manager.data_dir)
    { return Err(IntegrationError::new("invalid_request")); }
    let entry=compatibility::find(&request.adapter_id,&request.product_version)?;
    let expected=if request.adapter_id=="codex" { "hooks.json" } else { "settings.json" };
    if request.config_path.file_name().and_then(|s|s.to_str())!=Some(expected)
        || (request.adapter_id=="codex" && request.codex_home.as_deref()!=request.config_path.parent()) {
        return Err(IntegrationError::new("config_path_mismatch"));
    }
    if !request.client_executable.is_file() || !request.agentnotify_executable.is_file() {
        return Err(IntegrationError::new("executable_missing"));
    }
    verify_client_version(&request)?;
    let self_exe=std::env::current_exe().map_err(|_|IntegrationError::new("executable_unavailable"))?;
    if fs::canonicalize(&self_exe).ok()!=fs::canonicalize(&request.agentnotify_executable).ok() {
        return Err(IntegrationError::new("executable_mismatch"));
    }
    // 包内CLI仅负责预演，写入Hook的必须是归属校验过的稳定Launcher；未安装不降级临时路径。
    let paths=agentnotify_platform::paths::PlatformPaths::override_data(&manager.data_dir).map_err(|_|IntegrationError::new("unsafe_path"))?;
    request.agentnotify_executable=agentnotify_platform::service::owned_launcher(&paths).map_err(|_|IntegrationError::new("needs_install"))?;
    let original=if request.config_path.exists() {
        let raw=fs::read(&request.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?;
        if raw.len()>1024*1024 { return Err(IntegrationError::new("config_too_large")); }
        Some(raw)
    } else { None };
    let mut root=if let Some(raw)=&original {
        serde_json::from_slice::<Value>(raw).map_err(|_|IntegrationError::new("config_invalid"))?
    } else { json!({}) };
    let command=if request.adapter_id=="codex" {
        let path=request.agentnotify_executable.to_string_lossy();
        let data=manager.data_dir.to_string_lossy();
        if path.contains('"') || path.contains(['\r','\n']) || data.contains('"') || data.contains(['\r','\n']) {
            return Err(IntegrationError::new("unsafe_command"));
        }
        format!("\"{}\" --data-dir \"{}\" hook codex --instance-id {}",path,data,request.instance_id)
    } else { request.agentnotify_executable.to_string_lossy().into_owned() };
    append_hooks(&mut root,&request.adapter_id,&entry.events,&command,request.instance_id,&manager.data_dir)?;
    let planned=serde_json::to_vec_pretty(&root).map_err(|_|IntegrationError::new("config_invalid"))?;
    // 2. 私有计划包含原字节，CLI只返回受控摘要；状态文件位于平台安全目录。
    let op=Uuid::new_v4();
    let backup=manager.state_dir().join(format!("{op}.backup"));
    let plan=IntegrationPlan {
        schema_version:1,operation_id:op,request,
        original_hash:original.as_ref().map(|b|hash(b)),
        planned_hash:hash(&planned),
        original_bytes:original,
        planned_bytes:planned,
        backup_path:backup.clone(),
        owned_command:command,
        events:entry.events.clone(),
        state:OperationState::Planned,
    };
    manager.save_operation(&plan)?;
    Ok(PlanSummary { operation_id:op,plan_path:manager.operation_path(op),original_hash:plan.original_hash,
        planned_hash:plan.planned_hash,backup_path:backup,events:entry.events,status:"planned" })
}
