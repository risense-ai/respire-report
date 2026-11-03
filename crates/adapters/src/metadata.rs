//! 可选元数据补全只能在对应版本的适配器调用；默认不读取客户端私有数据。
use std::{path::PathBuf, time::Duration};
#[derive(Debug, Clone)]
pub struct MetadataRequest {
    pub adapter_id: String,
    pub version: String,
    pub session_id: String,
    pub source_path: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct LookupBudget {
    pub timeout: Duration,
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Default)]
pub struct OptionalMetadata {
    pub project_label: Option<String>,
    pub session_label: Option<String>,
}
#[derive(Debug, Clone)]
pub struct MetadataError { pub code: &'static str }
pub trait MetadataProvider {
    fn lookup(&self, request: &MetadataRequest, budget: &LookupBudget) -> Result<OptionalMetadata, MetadataError>;
}
pub struct DisabledMetadata;
impl MetadataProvider for DisabledMetadata {
    fn lookup(&self, _: &MetadataRequest, _: &LookupBudget) -> Result<OptionalMetadata, MetadataError> {
        Err(MetadataError { code: "metadata_disabled" })
    }
}
