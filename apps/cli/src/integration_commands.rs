//! CLI只输出受控事务摘要；原配置/备份字节留在当前用户安全目录。
use crate::{commands::{Command,Invocation},output};
use agentnotify_core::contracts::{CommandReport,read_bounded};
use agentnotify_integration::{IntegrationManager,IntegrationRequest};
use agentnotify_integration::migration;
use agentnotify_platform::paths::PlatformPaths;
use std::{fs::File,path::Path};
use uuid::Uuid;
fn success(value:impl serde::Serialize)->i32 {
    match serde_json::to_string(&CommandReport::success(value)) {
        Ok(s)=>{println!("{s}");0},
        Err(_)=>output::report(4,"report_serialization_failed","无法输出接入报告。",None),
    }
}
pub fn execute(invocation:Invocation)->i32 {
    let paths=match invocation.data_dir {
        Some(p)=>PlatformPaths::override_data(&p),
        None=>PlatformPaths::discover(),
    };
    let Ok(paths)=paths else {return output::report(3,"unsafe_data_dir","无法确认接入状态目录。",None)};
    let needs_write=matches!(invocation.command,Command::Plan(_)|Command::Apply(_)|Command::Rollback(_));
    if (paths.data.exists() || needs_write) && paths.secure(needs_write).is_err() {
        return output::report(3,"secure_directory_unavailable","无法确认当前用户安全目录。",None);
    }
    // 预演执行外部--version探测，不能占用Hook的700ms入站锁；只读检查维护后写独立plan。
    if matches!(invocation.command,Command::Plan(_)) {
        match agentnotify_platform::update::maintenance_active(&paths) {
            Ok(false)=>{}, Ok(true)=>return output::report(3,"maintenance_in_progress","维护期间不能生成接入预演。",None),
            Err(e)=>return output::report(3,e.0,"无法确认维护状态。",None),
        }
    }
    let _admission = if matches!(invocation.command,Command::Apply(_)|Command::Rollback(_)) {
        let security=match paths.secure(false) { Ok(s)=>s,Err(_)=>return output::report(3,"secure_directory_unavailable","无法确认安全目录。",None) };
        match agentnotify_platform::update::configuration_guard(&paths, security) {
            Ok(g)=>Some(g), Err(e)=>return output::report(3,e.0,"维护期间不能更改接入。",None)
        }
    } else { None };
    let manager=IntegrationManager::new(paths.data);
    match invocation.command {
        Command::Inspect=>success(manager.inspect()),
        Command::MigrationInspect(root)=>match migration::inspect_legacy(&root) {
            Ok(report)=>success(report),
            Err(e)=>output::report(2,e.0,"旧版只读发现未完成；未读取历史与凭据。",None),
        },
        Command::Plan(path)=>{
            let raw=File::open(path).map_err(|_|"input_unavailable").and_then(|f|read_bounded(f).map_err(|_|"input_too_large"));
            let request=raw.and_then(|b|serde_json::from_slice::<IntegrationRequest>(&b).map_err(|_|"invalid_request"));
            match request.and_then(|r|manager.plan(r).map_err(|e|e.code)) {
                Ok(report)=>success(report),
                Err(code)=>output::report(2,code,"接入计划未生成；检查显式路径、精确版本和配置格式。",None),
            }
        }
        Command::Apply(path)=>{
            let id=path.file_stem().and_then(|s|s.to_str()).and_then(|s|Uuid::parse_str(s).ok());
            let Some(id)=id else {return output::report(2,"invalid_plan_path","计划文件名无效。",None)};
            if !same_file(&path,&manager.operation_path(id)) {
                return output::report(2,"plan_not_owned","仅能应用本机安全目录中的计划。",None);
            }
            match manager.apply(id) {
                Ok(report)=>success(report),
                Err(e)=>output::report(4,e.code,"应用未完成；请重新检查计划与配置文件。",None),
            }
        }
        Command::Verify(id)=>run_id(&manager,&id,false),
        Command::Rollback(id)=>run_id(&manager,&id,true),
        _=>output::unavailable(),
    }
}
fn run_id(manager:&IntegrationManager,id:&str,rollback:bool)->i32 {
    let Ok(id)=Uuid::parse_str(id) else {return output::report(2,"invalid_identity","操作ID无效。",None)};
    let result=if rollback {manager.rollback(id)} else {manager.verify(id)};
    match result {
        Ok(report)=>success(report),
        Err(e)=>output::report(4,e.code,"接入操作未完成。",None),
    }
}
fn same_file(a:&Path,b:&Path)->bool {
    a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a,b)|a==b)
}
