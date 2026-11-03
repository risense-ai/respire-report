//! 兼容表只登记明确版本与证据级别，未知版本不得生成写入计划。
use crate::{IntegrationError, Result};
use serde::Deserialize;
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub adapter_id: String,
    pub product: String,
    pub version: String,
    pub os: String,
    pub architecture: String,
    pub config_mode: String,
    pub events: Vec<String>,
    pub metadata: String,
    pub evidence_level: String,
    pub evidence_date: String,
    pub references: Vec<String>,
}
#[derive(Deserialize)]
struct Catalog { schema_version: u8, entries: Vec<Entry> }
pub fn entries()->Result<Vec<Entry>> {
    let catalog:Catalog=serde_json::from_str(include_str!("../../../resources/compatibility/compatibility.json"))
        .map_err(|_|IntegrationError::new("compatibility_invalid"))?;
    if catalog.schema_version!=1 { return Err(IntegrationError::new("compatibility_invalid")); }
    Ok(catalog.entries)
}
pub fn find(adapter:&str,version:&str)->Result<Entry> {
    entries()?.into_iter().find(|e|e.adapter_id==adapter && e.version==version && e.os=="windows" && e.architecture=="x86_64")
        .ok_or(IntegrationError::new("unsupported_version"))
}
