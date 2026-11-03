//! 卸载仅摘除绑定记录证明归属、且仍完整匹配的 Hook；其他插件与用户修改保留。
use crate::{Binding, IntegrationError, IntegrationManager, Result, atomic_write, hash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf};
#[derive(Clone, Serialize, Deserialize)]
pub struct RemovalFile {
    pub path: PathBuf,
    pub before_hash: String,
    pub after: Vec<u8>,
}
/// 只做预演，发生归属冲突时在任何服务变更之前拒绝。
pub fn preview(manager: &IntegrationManager) -> Result<Vec<RemovalFile>> {
    let _guard = manager.lock_transaction()?;
    let mut files: BTreeMap<PathBuf, Vec<Binding>> = BTreeMap::new();
    let owner =
        fs::canonicalize(&manager.data_dir).map_err(|_| IntegrationError::new("unsafe_path"))?;
    for binding in manager.bindings()? {
        if fs::canonicalize(&binding.data_dir)
            .map_err(|_| IntegrationError::new("binding_root_conflict"))?
            != owner
        {
            return Err(IntegrationError::new("binding_root_conflict"));
        }

        files
            .entry(binding.config_path.clone())
            .or_default()
            .push(binding);
    }
    let mut result = Vec::new();
    for (path, bindings) in files {
        agentnotify_core::queue::reject_links(&path)
            .map_err(|_| IntegrationError::new("unsafe_path"))?;
        if !path.exists() {
            continue;
        }
        let raw = fs::read(&path).map_err(|_| IntegrationError::new("config_unavailable"))?;
        if raw.len() > 1048576 {
            return Err(IntegrationError::new("config_too_large"));
        }
        let mut root: Value =
            serde_json::from_slice(&raw).map_err(|_| IntegrationError::new("config_invalid"))?;
        let hooks = root
            .get_mut("hooks")
            .and_then(Value::as_object_mut)
            .ok_or(IntegrationError::new("config_invalid"))?;
        for groups in hooks.values_mut() {
            let groups = groups
                .as_array_mut()
                .ok_or(IntegrationError::new("config_invalid"))?;
            let mut removed_groups = Vec::new();
            for (index, group) in groups.iter_mut().enumerate() {
                let handlers = group
                    .get_mut("hooks")
                    .and_then(Value::as_array_mut)
                    .ok_or(IntegrationError::new("config_invalid"))?;
                let original_count = handlers.len();
                let expected:Vec<Value>=bindings.iter().map(|b|if b.adapter_id=="codex" { json!({"type":"command","command":b.owned_command,"commandWindows":b.owned_command,"timeout":2}) } else { json!({"type":"command","command":b.owned_command,"args":["--data-dir",b.data_dir.to_string_lossy(),"hook","claude_code","--instance-id",b.instance_id.to_string()],"timeout":2}) }).collect();
                if bindings
                    .iter()
                    .any(|b| !matches!(b.adapter_id.as_str(), "codex" | "claude_code"))
                {
                    return Err(IntegrationError::new("unsupported_adapter"));
                }
                for h in handlers.iter() {
                    if expected.contains(h) {
                        continue;
                    }
                    for b in &bindings {
                        let executable = if b.adapter_id == "codex" {
                            b.owned_command.split('"').nth(1).unwrap_or("")
                        } else {
                            &b.owned_command
                        };
                        let normalized = executable.replace('\\', "/").to_lowercase();
                        let refers_runtime = ["command", "commandWindows"]
                            .iter()
                            .filter_map(|key| h.get(key).and_then(Value::as_str))
                            .any(|v| {
                                !normalized.is_empty()
                                    && v.replace('\\', "/").to_lowercase().contains(&normalized)
                            });
                        if h.to_string().contains(&b.instance_id.to_string()) || refers_runtime {
                            return Err(IntegrationError::new("owned_hook_conflict"));
                        }
                    }
                }
                handlers.retain(|h| !expected.contains(h));
                if original_count > 0 && handlers.is_empty() {
                    removed_groups.push(index);
                }
            }
            for index in removed_groups.into_iter().rev() {
                groups.remove(index);
            }
        }
        let after = serde_json::to_vec_pretty(&root)
            .map_err(|_| IntegrationError::new("config_invalid"))?;
        if serde_json::from_slice::<Value>(&raw).ok() != Some(root) {
            result.push(RemovalFile {
                path,
                before_hash: hash(&raw),
                after,
            });
        }
    }
    Ok(result)
}
/// 全部先复核，再逐文件原子写入；外部并发编辑拒绝覆盖，部分完成由上层报告。
pub fn apply(manager: &IntegrationManager, files: &[RemovalFile]) -> Result<()> {
    let _guard = manager.lock_transaction()?;
    for f in files {
        verify(f)?;
    }
    for f in files {
        verify(f)?;
        atomic_write(&f.path, &f.after)?;
    }
    Ok(())
}
fn verify(f: &RemovalFile) -> Result<()> {
    agentnotify_core::queue::reject_links(&f.path)
        .map_err(|_| IntegrationError::new("unsafe_path"))?;
    let raw = fs::read(&f.path).map_err(|_| IntegrationError::new("config_unavailable"))?;
    if hash(&raw) != f.before_hash {
        return Err(IntegrationError::new("owned_hook_conflict"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    #[test]
    fn preserves_unrelated_empty_groups_and_rejects_edited_owned_handler() {
        let root = tempfile::tempdir().unwrap();
        let manager = IntegrationManager::new(root.path().join("data"));
        fs::create_dir_all(manager.state_dir()).unwrap();
        let id = Uuid::from_bytes([7; 16]);
        let command =
            "\"C:/fixture/launcher.exe\" --data-dir \"C:/fixture\" hook codex --instance-id "
                .to_owned()
                + &id.to_string();
        let config = root.path().join("hooks.json");
        let binding = Binding {
            instance_id: id,
            adapter_id: "codex".into(),
            product_version: "fixture".into(),
            config_path: config.clone(),
            client_executable: root.path().join("codex.exe"),
            data_dir: root.path().join("data"),
            codex_home: Some(root.path().into()),
            owned_command: command.clone(),
        };
        manager.save_bindings(&[binding.clone()]).unwrap();
        let owned =
            json!({"type":"command","command":command,"commandWindows":command,"timeout":2});
        let unrelated = json!({"matcher":"keep","hooks":[]});
        fs::write(&config,serde_json::to_vec(&json!({"hooks":{"Stop":[unrelated.clone(),{"hooks":[owned.clone()]}]},"other":"kept"})).unwrap()).unwrap();
        let plan = preview(&manager).unwrap();
        let cleaned: Value = serde_json::from_slice(&plan[0].after).unwrap();
        assert_eq!(cleaned["hooks"]["Stop"], json!([unrelated]));
        assert_eq!(cleaned["other"], "kept");
        let mut second = binding.clone();
        second.instance_id = Uuid::from_bytes([8; 16]);
        second.owned_command = command.replace(&id.to_string(), &second.instance_id.to_string());
        manager.save_bindings(&[binding, second.clone()]).unwrap();
        let owned2 = json!({"type":"command","command":second.owned_command,"commandWindows":second.owned_command,"timeout":2});
        let other = json!({"type":"command","command":"other-plugin","timeout":3});
        fs::write(
            &config,
            serde_json::to_vec(
                &json!({"hooks":{"Stop":[{"hooks":[owned.clone(),owned2,other.clone()]}]}}),
            )
            .unwrap(),
        )
        .unwrap();
        let two = preview(&manager).unwrap();
        let cleaned: Value = serde_json::from_slice(&two[0].after).unwrap();
        assert_eq!(cleaned["hooks"]["Stop"][0]["hooks"], json!([other]));
        let mut edited = owned;
        edited["command"] = json!(
            "\"C:/fixture/launcher.exe\" --data-dir \"C:/fixture\" hook codex --instance-id changed"
        );
        fs::write(
            &config,
            serde_json::to_vec(&json!({"hooks":{"Stop":[{"hooks":[edited]}]}})).unwrap(),
        )
        .unwrap();
        assert_eq!(preview(&manager).err().unwrap().code, "owned_hook_conflict");
        let foreign = root.path().join("other-install");
        fs::create_dir(&foreign).unwrap();
        let mut foreign_binding = second;
        foreign_binding.data_dir = foreign;
        manager.save_bindings(&[foreign_binding]).unwrap();
        assert_eq!(
            preview(&manager).err().unwrap().code,
            "binding_root_conflict"
        );
    }
}
