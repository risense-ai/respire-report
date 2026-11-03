//! 版本受控客户端Hook边界；原生字段和插件协议不得进入core。
pub mod claude_code;
pub mod codex;
pub mod codex_observer;
mod codex_metadata;
mod codex_question;
pub mod generic;
pub mod metadata;
pub mod source_classifier;

use agentnotify_core::{
    contracts::InputError,
    event::AdapterOutput,
};
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use uuid::Uuid;

/// 由本机登记和实际调用环境提供，不接受Hook正文声明实例或版本。
#[derive(Debug, Clone)]
pub struct AdapterContext {
    pub instance_id: Uuid,
    pub adapter_id: String,
    pub version: String,
    pub config_path: PathBuf,
    pub executable_path: PathBuf,
    pub registered_command: String,
    pub codex_home: Option<PathBuf>,
    pub received_at: DateTime<Utc>,
    pub content_key_available: bool,
    pub include_question: bool,
    pub supported_version: bool,
}
impl AdapterContext {
    pub fn require_binding(&self, adapter: &str) -> Result<(), AdapterError> {
        if self.adapter_id != adapter || !self.supported_version
            || self.registered_command.is_empty()
            || !self.config_path.is_absolute()
            || !self.executable_path.is_absolute()
        {
            return Err(AdapterError::new("unverified_binding"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct AdapterError { pub code: &'static str }
impl AdapterError {
    pub fn new(code: &'static str) -> Self { Self { code } }
}
impl From<InputError> for AdapterError {
    fn from(_: InputError) -> Self { Self::new("invalid_input") }
}
pub trait Adapter {
    fn normalize(&self, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError>;
}
pub fn normalize(adapter: &str, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError> {
    match adapter {
        "codex" => codex::Codex.normalize(raw, context),
        "claude_code" | "claude-code" => claude_code::ClaudeCode.normalize(raw, context),
        "generic" => generic::Generic.normalize(raw, context),
        _ => Err(AdapterError::new("unsupported_adapter")),
    }
}
pub(crate) fn parse_hook(raw: &[u8]) -> Result<serde_json::Value, AdapterError> {
    if raw.len() > agentnotify_core::contracts::MAX_RAW_BYTES {
        return Err(AdapterError::new("input_too_large"));
    }
    let value: serde_json::Value = serde_json::from_slice(raw).map_err(|_| AdapterError::new("invalid_input"))?;
    if !value.is_object() { return Err(AdapterError::new("invalid_input")); }
    Ok(value)
}
pub(crate) fn field<'a>(v: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    v.get(name).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty())
}
pub(crate) fn required_id(v: &serde_json::Value, name: &str) -> Result<String, AdapterError> {
    let id = field(v, name).ok_or(AdapterError::new("invalid_identity"))?;
    agentnotify_core::contracts::validate_identity(id).map_err(|_| AdapterError::new("invalid_identity"))?;
    Ok(id.to_owned())
}
