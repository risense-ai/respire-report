//! 显式实例、版本、文件hash与独立操作日志构成可回滚接入事务。
pub mod apply;
pub mod compatibility;
pub mod inspect;
pub mod migration;
pub mod plan;
pub mod rollback;
pub mod remove;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};
use uuid::Uuid;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationRequest {
    pub schema_version: u8,
    pub adapter_id: String,
    pub instance_id: Uuid,
    pub product_version: String,
    pub config_path: PathBuf,
    pub client_executable: PathBuf,
    pub agentnotify_executable: PathBuf,
    pub codex_home: Option<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPlan {
    pub schema_version: u8,
    pub operation_id: Uuid,
    pub request: IntegrationRequest,
    pub original_hash: Option<String>,
    pub planned_hash: String,
    pub original_bytes: Option<Vec<u8>>,
    pub planned_bytes: Vec<u8>,
    pub backup_path: PathBuf,
    pub owned_command: String,
    pub events: Vec<String>,
    pub state: OperationState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum OperationState { Planned, Applied, RolledBack }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub instance_id: Uuid,
    pub adapter_id: String,
    pub product_version: String,
    pub config_path: PathBuf,
    pub client_executable: PathBuf,
    pub data_dir: PathBuf,
    pub codex_home: Option<PathBuf>,
    pub owned_command: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub operation_id: Uuid,
    pub plan_path: PathBuf,
    pub original_hash: Option<String>,
    pub planned_hash: String,
    pub backup_path: PathBuf,
    pub events: Vec<String>,
    pub status: &'static str,
}
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    pub operation_id: Uuid,
    pub config_matches_plan: bool,
    pub binding_matches: bool,
    pub status: &'static str,
    pub live_verified: bool,
}
#[derive(Debug, Clone)]
pub struct IntegrationError { pub code: &'static str }
impl IntegrationError { pub fn new(code: &'static str)->Self { Self { code } } }
pub type Result<T> = std::result::Result<T,IntegrationError>;
pub struct IntegrationManager { pub data_dir: PathBuf }
impl IntegrationManager {
    pub fn new(data_dir: PathBuf)->Self { Self { data_dir } }
    pub fn state_dir(&self)->PathBuf { self.data_dir.join("integrations") }
    pub fn operation_path(&self,id:Uuid)->PathBuf { self.state_dir().join(format!("{id}.json")) }
    pub fn bindings_path(&self)->PathBuf { self.state_dir().join("instances.json") }
    // 同一data_dir所有实例共用instances.json；事务锁也覆盖同配置跨实例写入。
    fn lock_transaction(&self)->Result<fs::File> {
        fs::create_dir_all(self.state_dir()).map_err(|_|IntegrationError::new("state_unavailable"))?;
        let file=fs::OpenOptions::new().read(true).write(true).create(true)
            .open(self.state_dir().join("transaction.lock"))
            .map_err(|_|IntegrationError::new("state_unavailable"))?;
        file.try_lock().map_err(|_|IntegrationError::new("operation_busy"))?;
        Ok(file)
    }

    pub fn plan(&self,request:IntegrationRequest)->Result<PlanSummary> { plan::create(self,request) }
    pub fn apply(&self,id:Uuid)->Result<VerificationReport> { apply::apply(self,id) }
    pub fn rollback(&self,id:Uuid)->Result<VerificationReport> { rollback::rollback(self,id) }
    pub fn verify(&self,id:Uuid)->Result<VerificationReport> { apply::verify(self,id) }
    pub fn inspect(&self)->inspect::InspectionReport { inspect::inspect(self) }
    pub fn lookup(&self,id:Uuid)->Result<Option<Binding>> {
        Ok(self.bindings()?.into_iter().find(|v|v.instance_id==id))
    }
    pub fn bindings(&self)->Result<Vec<Binding>> {
        let path=self.bindings_path();
        if !path.exists() { return Ok(Vec::new()); }
        let raw=fs::read(path).map_err(|_|IntegrationError::new("state_unavailable"))?;
        serde_json::from_slice(&raw).map_err(|_|IntegrationError::new("state_invalid"))
    }
    fn save_bindings(&self,bindings:&[Binding])->Result<()> {
        atomic_write(&self.bindings_path(),&serde_json::to_vec_pretty(bindings).map_err(|_|IntegrationError::new("state_invalid"))?)
    }
    fn load_operation(&self,id:Uuid)->Result<IntegrationPlan> {
        let raw=fs::read(self.operation_path(id)).map_err(|_|IntegrationError::new("operation_missing"))?;
        let plan:IntegrationPlan=serde_json::from_slice(&raw).map_err(|_|IntegrationError::new("operation_invalid"))?;
        if plan.operation_id!=id || plan.schema_version!=1 { return Err(IntegrationError::new("operation_invalid")); }
        Ok(plan)
    }
    fn save_operation(&self,plan:&IntegrationPlan)->Result<()> {
        atomic_write(&self.operation_path(plan.operation_id),&serde_json::to_vec_pretty(plan).map_err(|_|IntegrationError::new("operation_invalid"))?)
    }
}
pub(crate) fn hash(bytes:&[u8])->String {
    Sha256::digest(bytes).iter().map(|v|format!("{v:02x}")).collect()
}
pub(crate) fn atomic_write(path:&Path,bytes:&[u8])->Result<()> {
    let parent=path.parent().ok_or(IntegrationError::new("unsafe_path"))?;
    fs::create_dir_all(parent).map_err(|_|IntegrationError::new("write_failed"))?;
    let temp=parent.join(format!(".agentnotify-{}.tmp",Uuid::new_v4()));
    let result=(||{
        use std::io::Write;
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|_|IntegrationError::new("write_failed"))?;
        file.write_all(bytes).map_err(|_|IntegrationError::new("write_failed"))?;
        file.sync_all().map_err(|_|IntegrationError::new("write_failed"))?;
        fs::rename(&temp,path).map_err(|_|IntegrationError::new("write_failed"))
    })();
    if result.is_err() { let _=fs::remove_file(&temp); }
    result
}



/// 核对登记命令确实存在于当前客户端配置；不把正文或字符串子串当来源证明。
pub fn binding_active(binding:&Binding)->Result<bool> {
    let entry=compatibility::find(&binding.adapter_id,&binding.product_version)?;
    let raw=fs::read(&binding.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?;
    if raw.len()>1024*1024 { return Err(IntegrationError::new("config_too_large")); }
    let value:serde_json::Value=serde_json::from_slice(&raw).map_err(|_|IntegrationError::new("config_invalid"))?;
    let Some(hooks)=value.get("hooks").and_then(serde_json::Value::as_object) else { return Ok(false) };
    for event in &entry.events {
        let Some(groups)=hooks.get(event).and_then(serde_json::Value::as_array) else { return Ok(false) };
        let found=groups.iter().any(|group|
            group.get("hooks").and_then(serde_json::Value::as_array).is_some_and(|handlers|
                handlers.iter().any(|handler| {
                    if handler.get("type").and_then(serde_json::Value::as_str)!=Some("command")
                        || handler.get("command").and_then(serde_json::Value::as_str)!=Some(binding.owned_command.as_str()) {
                        return false;
                    }
                    if binding.adapter_id=="codex" {
                        handler.get("commandWindows").and_then(serde_json::Value::as_str)==Some(binding.owned_command.as_str())
                    } else {
                        let Some(args)=handler.get("args").and_then(serde_json::Value::as_array) else { return false };
                        args.len()==6
                            && args[0].as_str()==Some("--data-dir")
                            && args[1].as_str()==binding.data_dir.to_str()
                            && args[2].as_str()==Some("hook")
                            && args[3].as_str()==Some("claude_code")
                            && args[4].as_str()==Some("--instance-id")
                            && args[5].as_str()==Some(binding.instance_id.to_string().as_str())
                    }
                })
            )
        );
        if !found { return Ok(false); }
    }
    Ok(true)
}
