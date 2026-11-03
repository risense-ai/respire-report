//! Generic始终是调用方报告；任何正文或来源标签均不能提升信任。
use crate::{Adapter, AdapterContext, AdapterError};
use agentnotify_core::{contracts::{IngressContext, parse_generic}, event::AdapterOutput};
pub struct Generic;
impl Adapter for Generic {
    fn normalize(&self, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError> {
        if context.adapter_id != "generic" { return Err(AdapterError::new("unverified_binding")); }
        parse_generic(raw, &IngressContext {
            instance_id: context.instance_id,
            received_at: context.received_at,
            content_key_available: context.content_key_available,
        }).map_err(|_| AdapterError::new("invalid_input"))
    }
}
