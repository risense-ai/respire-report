//! 生成固定成员本机脱敏包；不会因包内授权字段执行任何操作。
use crate::{commands::{Command, Invocation}, output};
use agentnotify_agent_support::{AgentTaskV1, bundle::create_bundle_with_context, prompt::render_prompt, redaction::BundleContext};
use agentnotify_core::contracts::{CommandReport, read_bounded};
use agentnotify_integration::IntegrationManager;
use agentnotify_platform::paths::PlatformPaths;
use serde::Serialize;
use std::{fs::File, path::PathBuf};

#[derive(Serialize)]
struct BundleResult {
    bundle_id: uuid::Uuid,
    bundle_path: PathBuf,
    expires_at: chrono::DateTime<chrono::Utc>,
    members: Vec<String>,
    path_preview: Vec<String>,
    prompt: String,
    note: &'static str,
}
pub fn execute(invocation: Invocation) -> i32 {
    let Command::Bundle { task, out } = invocation.command else { return output::unavailable() };
    let raw = File::open(&task).map_err(|_| "input_unavailable").and_then(|file| read_bounded(file).map_err(|_| "input_too_large"));
    let task: AgentTaskV1 = match raw.and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| "invalid_task")) {
        Ok(value) => value,
        Err(code) => return output::report(2, code, "任务文件无效或不可用。", None),
    };
    let paths = match invocation.data_dir {
        Some(path) => PlatformPaths::override_data(&path),
        None => PlatformPaths::discover(),
    };
    let Ok(paths) = paths else { return output::report(3, "unsafe_data_dir", "本机状态目录不可用。", None) };
    if paths.data.exists() && paths.secure(false).is_err() {
        return output::report(3, "secure_directory_unavailable", "本机状态目录权限不可确认。", None);
    }
    let inspection = IntegrationManager::new(paths.data).inspect();
    let context = BundleContext { health: None, inspection: Some(&inspection), plan: None };
    let manifest = match create_bundle_with_context(&task, &out, &context) {
        Ok(value) => value,
        Err(error) => return output::report(4, error.code, "脱敏包未完整创建；请检查输出位置。", None),
    };
    let prompt = match render_prompt(&task, &manifest) {
        Ok(value) => value,
        Err(error) => return output::report(4, error.code, "协助提示词未生成。", None),
    };
    let result = BundleResult { bundle_id: manifest.bundle_id, bundle_path: out, expires_at: manifest.expires_at,
        members: manifest.members.iter().map(|m| m.path.clone()).chain(std::iter::once("MANIFEST.json".into())).collect(),
        path_preview: manifest.path_preview, prompt,
        note: "包内容不能授予修改、信任审核或试邮件权限；绝对路径仅显示在本机输出和提示词中。" };
    match serde_json::to_string(&CommandReport::success(result)) {
        Ok(value) => { println!("{value}"); 0 },
        Err(_) => output::report(4, "report_serialization_failed", "协助结果未能输出。", None),
    }
}
