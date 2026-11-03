//! 维护命令不自行寻找/替换信任根；执行失败仅返回受控原因码。
use crate::{commands::{Command, Invocation}, output};
use agentnotify_platform::{PlatformError, Result, paths::PlatformPaths, update::{self, UpdateManager, UninstallManager}};
pub fn execute(i: Invocation) -> i32 {
    let paths = i.data_dir.as_ref().map(|p| PlatformPaths::override_data(p)).unwrap_or_else(PlatformPaths::discover);
    let result: Result<serde_json::Value> = paths.and_then(|paths| {
        let manager = UpdateManager::new(paths.clone());
        match i.command {
            Command::Update { action, manifest } => {
                let report = match action.as_str() {
                    "verify" => { update::verify_release(&update::read_signed_manifest(&manifest.ok_or(PlatformError("manifest_required"))?)?)?; return Ok(serde_json::json!({"verified":true})); },
                    "prepare" => manager.prepare(&update::read_signed_manifest(&manifest.ok_or(PlatformError("manifest_required"))?)?)?,
                    "apply" => manager.apply()?, "rollback" => manager.rollback()?, "resume" => manager.resume()?, "status" => manager.status()?,
                    _ => return Err(PlatformError("invalid_arguments")),
                };
                serde_json::to_value(report).map_err(|_| PlatformError("report_invalid"))
            }
            Command::Uninstall(apply) => {
                let manager = UninstallManager::new(paths);
                let plan = manager.plan()?;
                if apply { serde_json::to_value(manager.apply(&plan)?).map_err(|_| PlatformError("report_invalid")) }
                else { serde_json::to_value(plan).map_err(|_| PlatformError("report_invalid")) }
            }
            _ => Err(PlatformError("invalid_arguments")),
        }
    });
    match result { Ok(value) => output::report(0, "ok", "操作已完成；更新/回滚后按说明启动后台。", Some(value)), Err(e) => output::report(3, e.0, "操作未完成；维护中请保留数据目录，按说明恢复。", None) }
}
