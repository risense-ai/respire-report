//! apply/verify不写信任记录；隔离探针不能提升到live_verified。
use crate::{Binding, IntegrationError, IntegrationManager, IntegrationPlan, OperationState, Result, VerificationReport, atomic_write, hash, binding_active};
use std::fs;
use uuid::Uuid;
fn register(manager:&IntegrationManager,plan:&IntegrationPlan)->Result<()> {
    let binding=Binding {
        instance_id:plan.request.instance_id,
        adapter_id:plan.request.adapter_id.clone(),
        product_version:plan.request.product_version.clone(),
        config_path:plan.request.config_path.clone(),
        client_executable:plan.request.client_executable.clone(),
        data_dir:manager.data_dir.clone(),
        codex_home:plan.request.codex_home.clone(),
        owned_command:plan.owned_command.clone(),
    };
    let mut bindings=manager.bindings()?;
    if let Some(existing)=bindings.iter().find(|b|b.instance_id==binding.instance_id) {
        if existing.owned_command!=binding.owned_command || existing.config_path!=binding.config_path
            || existing.product_version!=binding.product_version {
            return Err(IntegrationError::new("binding_conflict"));
        }
    } else { bindings.push(binding); }
    manager.save_bindings(&bindings)
}
pub fn apply(manager:&IntegrationManager,id:Uuid)->Result<VerificationReport> {
    let _lock=manager.lock_transaction()?;
    let mut plan=manager.load_operation(id)?;
    if plan.state==OperationState::RolledBack { return Err(IntegrationError::new("operation_rolled_back")); }
    if plan.request.adapter_id == "workbuddy" {
        // 预演后若客户端更新或配置路径变成链接，先拒绝写入。
        crate::plan::verify_client_version(&plan.request)?;
        agentnotify_core::queue::reject_links(&plan.request.config_path)
            .map_err(|_| IntegrationError::new("unsafe_path"))?;
    }
    let current=if plan.request.config_path.exists() {
        Some(fs::read(&plan.request.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?)
    } else { None };
    let current_hash=current.as_ref().map(|b|hash(b));
    // 1. 已写配置但状态记录中断时补登记；已有实例冲突先拒绝，避免先改客户端配置。
    let bindings=manager.bindings()?;
    if let Some(existing)=bindings.iter().find(|b|b.instance_id==plan.request.instance_id) {
        if existing.owned_command!=plan.owned_command || existing.config_path!=plan.request.config_path
            || existing.product_version!=plan.request.product_version {
            return Err(IntegrationError::new("binding_conflict"));
        }
    }
    if current_hash==Some(plan.planned_hash.clone()) {
        register(manager,&plan)?;
        plan.state=OperationState::Applied;
        manager.save_operation(&plan)?;
        return verify(manager,id);
    }
    if plan.state!=OperationState::Planned || current_hash!=plan.original_hash {
        return Err(IntegrationError::new("apply_hash_conflict"));
    }
    // 2. 在写配置前固定原字节备份；原文件不存在时不存在备份。
    if let Some(original)=&plan.original_bytes {
        if plan.backup_path.exists() {
            if fs::read(&plan.backup_path).ok().as_deref()!=Some(original.as_slice()) {
                return Err(IntegrationError::new("backup_conflict"));
            }
        } else {
            use std::io::Write;
            let mut backup=fs::OpenOptions::new().write(true).create_new(true).open(&plan.backup_path)
                .map_err(|_|IntegrationError::new("backup_failed"))?;
            backup.write_all(original).and_then(|_|backup.sync_all()).map_err(|_|IntegrationError::new("backup_failed"))?;
        }
    }
    // 3. 备份后再次核对原字节；发生外部编辑即停止，不覆盖用户改动。
    let latest=if plan.request.config_path.exists() {
        Some(fs::read(&plan.request.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?)
    } else { None };
    if latest.as_ref().map(|b|hash(b))!=plan.original_hash {
        return Err(IntegrationError::new("apply_hash_conflict"));
    }
    atomic_write(&plan.request.config_path,&plan.planned_bytes)?;
    register(manager,&plan)?;
    plan.state=OperationState::Applied;
    manager.save_operation(&plan)?;
    verify(manager,id)
}
pub fn verify(manager:&IntegrationManager,id:Uuid)->Result<VerificationReport> {
    let plan=manager.load_operation(id)?;
    let current=fs::read(&plan.request.config_path).ok();
    let config_matches=current.as_ref().is_some_and(|b|hash(b)==plan.planned_hash);
    let binding_matches=manager.lookup(plan.request.instance_id)?
        .is_some_and(|b|b.adapter_id==plan.request.adapter_id && b.config_path==plan.request.config_path
            && b.owned_command==plan.owned_command && binding_active(&b).unwrap_or(false));
    let status=if plan.state==OperationState::RolledBack { "rolled_back" }
        else if !config_matches || !binding_matches { "degraded" }
        else if plan.request.adapter_id=="codex" { "trust_pending" }
        else { "reload_pending" };
    Ok(VerificationReport {operation_id:id,config_matches_plan:config_matches,binding_matches,status,live_verified:false})
}
