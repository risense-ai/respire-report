//! 回滚只恢复本操作拥有且字节完全匹配的配置；用户后续编辑保留。
use crate::{IntegrationError, IntegrationManager, OperationState, Result, VerificationReport, atomic_write, hash};
use std::fs;
use uuid::Uuid;
pub fn rollback(manager:&IntegrationManager,id:Uuid)->Result<VerificationReport> {
    let _lock=manager.lock_transaction()?;
    let mut plan=manager.load_operation(id)?;
    if plan.state==OperationState::RolledBack {
        return Ok(VerificationReport {operation_id:id,config_matches_plan:false,binding_matches:false,status:"rolled_back",live_verified:false});
    }
    let current=if plan.request.config_path.exists() {
        Some(fs::read(&plan.request.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?)
    } else { None };
    // apply中途失败可留下已写配置；只按计划的after hash归属回滚。
    if current.as_ref().map(|b|hash(b))!=Some(plan.planned_hash.clone()) {
        return Err(IntegrationError::new("rollback_user_edit_preserved"));
    }
    if let Some(original)=&plan.original_bytes {
        if fs::read(&plan.backup_path).ok().as_deref()!=Some(original.as_slice()) {
            return Err(IntegrationError::new("backup_conflict"));
        }
    }
    // 备份复核后恢复/删除前再次核对after hash；本工具事务由数据目录全局锁串行。
    let latest=fs::read(&plan.request.config_path).map_err(|_|IntegrationError::new("config_unavailable"))?;
    if hash(&latest)!=plan.planned_hash {
        return Err(IntegrationError::new("rollback_user_edit_preserved"));
    }
    if let Some(original)=&plan.original_bytes {
        atomic_write(&plan.request.config_path,original)?;
    } else {
        fs::remove_file(&plan.request.config_path).map_err(|_|IntegrationError::new("rollback_failed"))?;
    }
    let mut bindings=manager.bindings()?;
    bindings.retain(|b|!(b.instance_id==plan.request.instance_id
        && b.config_path==plan.request.config_path && b.owned_command==plan.owned_command));
    manager.save_bindings(&bindings)?;
    plan.state=OperationState::RolledBack;
    manager.save_operation(&plan)?;
    Ok(VerificationReport {operation_id:id,config_matches_plan:false,binding_matches:false,status:"rolled_back",live_verified:false})
}
