//! CLI只调用受限本机协议；Hook无stdout控制指令，持久化后才报告接收。
use crate::{
    commands::{Command, Invocation},
    output,
};
use agentnotify_core::contracts::*;
use agentnotify_adapters::{AdapterContext,normalize};
use agentnotify_integration::{IntegrationManager,compatibility,binding_active};
use agentnotify_platform::{
    PlatformError, Result,
    ipc::{self, Request, Response},
    paths::PlatformPaths,
    runtime::HostRuntime,
    service::{self, ServiceManager},
};
use uuid::Uuid;
fn success(value: impl serde::Serialize) -> i32 {
    match serde_json::to_string(&CommandReport::success(value)) {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(_) => 4,
    }
}
pub fn execute(i: Invocation) -> i32 {
    let hook = matches!(i.command, Command::Hook { .. });
    let paths = match i.data_dir {
        Some(p) => PlatformPaths::override_data(&p),
        None => PlatformPaths::discover(),
    };
    let result = paths.and_then(|paths| match i.command {
        Command::Desktop => { service::open_desktop(&paths)?; Ok(0) },
        Command::LaunchDaemon => service::launch(paths),
        Command::Service(action) => {
            let manager = ServiceManager::new(paths);
            let report = match action.as_str() {
                "install" => manager.install(),
                "start" => manager.start(),
                "stop" => manager.stop(),
                "status" => manager.status(),
                "uninstall" => manager.uninstall(),
                _ => Err(PlatformError("invalid_arguments")),
            }?;
            let code = if report.failed {
                4
            } else if report.unsupported {
                3
            } else {
                0
            };
            if code == 0 {
                Ok(success(report))
            } else {
                Ok(output::report(
                    code,
                    &report.code,
                    "用户后台操作未完成。",
                    serde_json::to_value(&report).ok(),
                ))
            }
        }
        Command::ObserverEnable(instance) => {
            let host=HostRuntime::open(paths.clone(),false)?;
            let security=paths.secure(false)?;
            let _ownership=agentnotify_adapters::codex_observer::completion_guard(&paths.data,instance,security.as_ref()).map_err(|e|PlatformError(e.code))?;
            // 旧入站没有可靠跨来源 child 键；本版仅允许未接收过完成事件的实例首次切源。
            if host.queue.instances().map_err(|_|PlatformError("observer_queue_unavailable"))?.contains(&instance) {return Err(PlatformError("observer_existing_ingress_migration_unsupported"));}
            // 离线暂存未导入时不能证明实例归属，保守要求整个暂存区为空。
            if std::fs::read_dir(host.queue.spool_path()).map_err(|_|PlatformError("observer_queue_unavailable"))?.next().is_some(){return Err(PlatformError("observer_spool_pending_retry"));}
            let binding=IntegrationManager::new(paths.data.clone()).lookup(instance).map_err(|_|PlatformError("binding_unavailable"))?.ok_or(PlatformError("binding_missing"))?;
            if !binding_active(&binding).map_err(|_|PlatformError("binding_unavailable"))? {return Err(PlatformError("binding_changed"));}
            let context=AdapterContext{instance_id:instance,adapter_id:binding.adapter_id,version:binding.product_version.clone(),config_path:binding.config_path,executable_path:binding.client_executable,registered_command:binding.owned_command,codex_home:binding.codex_home,received_at:chrono::Utc::now(),content_key_available:false,include_question:false,supported_version:compatibility::find("codex",&binding.product_version).is_ok()};
            agentnotify_adapters::codex_observer::enable(&paths.data,&context,security.as_ref()).map_err(|e|PlatformError(e.code))?;
            Ok(success(serde_json::json!({"enabled":true,"completion_source":"local_observer"})))
        }
        Command::Doctor => {
            if paths.data.exists() {
                let _guard = paths.secure(false)?;
                match ipc::call(&paths, &Request::Health) {
                    Ok(Response::Health { report }) => Ok(success(report)),
                    _ => Ok(success(HealthReport {
                        schema_version: 1,
                        daemon: DaemonStatus::Unknown,
                        queue_counts: QueueCounts::default(),
                        credential_status: CredentialStatus::Unknown,
                        integrations: Vec::new(),
                        issues: vec![HealthIssue {
                            code: "daemon_unreachable".into(),
                            message: "后台不可达，队列数量与凭据状态尚未读取。".into(),
                        }],
                    })),
                }
            } else {
                Ok(success(HealthReport {
                    schema_version: 1,
                    daemon: DaemonStatus::Stopped,
                    queue_counts: QueueCounts::default(),
                    credential_status: CredentialStatus::Unknown,
                    integrations: Vec::new(),
                    issues: vec![HealthIssue {
                        code: "not_initialized".into(),
                        message: "尚未初始化本工具目录。".into(),
                    }],
                }))
            }
        }
        Command::Pause(paused) => {
            let _guard = paths.secure(false)?;
            match ipc::call(&paths, &Request::Pause { paused })? {
                Response::Ok => Ok(success(serde_json::json!({"paused":paused}))),
                _ => Err(PlatformError("pause_failed")),
            }
        }
        Command::Emit => ingest(paths, None, false, "generic"),
        Command::Hook { adapter, instance } => ingest(paths, Some(instance), true, &adapter),
        _ => Err(PlatformError("invalid_arguments")),
    });
    match result {
        Ok(code) => code,
        Err(e) if hook => {
            eprintln!("{{\"ok\":false,\"error\":\"{}\"}}", e.0);
            0
        }
        Err(e) => output::report(4, e.0, "操作未完成；未声称事件已接收或邮件已发送。", None),
    }
}
/// 单条有界诊断仅含时间、阶段和静态错误码；锁竞争/写入失败不改变Hook语义。
pub(crate) fn hook_diagnostic(paths: &PlatformPaths, stage: &'static str, code: &'static str) {
    use std::io::Write;
    let _ = (|| -> Result<()> {
        let security = paths.secure(false)?;
        let target = paths.data.join("hook-diagnostic.json");
        let lock_path = paths.data.join("hook-diagnostic.lock");
        for p in [&target, &lock_path] { agentnotify_core::queue::reject_links(p).map_err(|_| PlatformError("unsafe_diagnostic"))?; }
        let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_| PlatformError("diagnostic_unavailable"))?;
        lock.try_lock().map_err(|_| PlatformError("diagnostic_busy"))?;
        let value = serde_json::json!({"at":chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),"stage":stage,"code":code});
        let raw = serde_json::to_vec(&value).map_err(|_| PlatformError("diagnostic_unavailable"))?;
        if raw.len() > 1024 { return Err(PlatformError("diagnostic_too_large")); }
        let mut pending = tempfile::NamedTempFile::new_in(&paths.data).map_err(|_| PlatformError("diagnostic_unavailable"))?;
        pending.write_all(&raw).map_err(|_| PlatformError("diagnostic_unavailable"))?;
        security.verify(&paths.data).map_err(|_| PlatformError("unsafe_diagnostic"))?;
        pending.persist(&target).map_err(|_| PlatformError("diagnostic_unavailable"))?;
        Ok(())
    })();
}
fn ingest(paths: PlatformPaths, instance: Option<Uuid>, hook: bool, adapter: &str) -> Result<i32> {
    let result = ingest_inner(paths.clone(), instance, hook, adapter);
    if let Err(e) = &result { if hook { hook_diagnostic(&paths, "rejected", e.0); } }
    result
}
fn ingest_inner(paths: PlatformPaths, instance: Option<Uuid>, hook: bool, adapter: &str) -> Result<i32> {
    // 1. 平台先验证安全目录与内容key；原始stdin只进入中立解析器。
    let host = HostRuntime::open(paths, true)?;
    let presence_suppressed = agentnotify_platform::presence::suppress_at_receive(&host.config);
    let raw = read_bounded(std::io::stdin().lock()).map_err(|_| PlatformError("invalid_input"))?;
    if hook { hook_diagnostic(&host.paths, "stdin_read", "ok"); }
    let instance = if let Some(id) = instance {
        id
    } else {
        let v: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| PlatformError("invalid_input"))?;
        Uuid::parse_str(
            v.get("instance_id")
                .and_then(|v| v.as_str())
                .ok_or(PlatformError("invalid_identity"))?,
        )
        .map_err(|_| PlatformError("invalid_identity"))?
    };
    let context = IngressContext {
        instance_id: instance,
        received_at: std::time::SystemTime::now().into(),
        content_key_available: !host.content_unavailable,
    };
    // 仲裁锁覆盖 ownership 检查到实际 ACK；初始化不能越过正在接收的完成 Hook。
    let _completion_guard=if hook && adapter=="codex" && agentnotify_adapters::codex_observer::completion_hook(&raw){
        let security=host.paths.secure(false)?;
        Some(agentnotify_adapters::codex_observer::completion_guard(&host.paths.data,instance,security.as_ref()).map_err(|e|PlatformError(e.code))?)
    }else{None};
    let mut output = if adapter == "generic" {
        parse_generic(&raw, &context).map_err(|_| PlatformError("invalid_input"))?
    } else {
        let adapter = if adapter == "claude-code" { "claude_code" } else { adapter };
        let binding = IntegrationManager::new(host.paths.data.clone()).lookup(instance)
            .map_err(|_| PlatformError("binding_unavailable"))?
            .ok_or(PlatformError("binding_missing"))?;
        if binding.adapter_id != adapter || !binding.client_executable.is_file() {
            return Err(PlatformError("unverified_binding"));
        }
        if !binding_active(&binding).map_err(|_| PlatformError("config_unavailable"))? {
            return Err(PlatformError("binding_changed"));
        }
        // 完成来源切换仅在适配边界；SubagentStart/SessionEnd 生命周期仍走既有链路。
        if adapter=="codex" && agentnotify_adapters::codex_observer::completion_hook(&raw) {
            let security=host.paths.secure(false)?;
            if agentnotify_adapters::codex_observer::owns(&host.paths.data,instance,security.as_ref()).map_err(|e|PlatformError(e.code))? {
                hook_diagnostic(&host.paths,"normalized","observer_completion_owned");
                return Ok(0);
            }
        }
        let adapter_context = AdapterContext {
            instance_id: instance,
            adapter_id: binding.adapter_id.clone(),
            version: binding.product_version.clone(),
            config_path: binding.config_path,
            executable_path: binding.client_executable,
            registered_command: binding.owned_command,
            codex_home: binding.codex_home,
            received_at: context.received_at,
            content_key_available: context.content_key_available,
            include_question: host.config.include_question && !presence_suppressed,
            supported_version: compatibility::find(adapter, &binding.product_version).is_ok(),
        };
        normalize(adapter, &raw, &adapter_context).map_err(|e| PlatformError(e.code))?
    };
    if hook { hook_diagnostic(&host.paths, "normalized", "ok"); }
    // 已在本机接收时抑制的通知只保留状态元数据，不传输或落盘正文。
    if presence_suppressed {
        if let Some(event) = &mut output.notification {
            event.question = None;
            event.answer = None;
        }
    }
    let id = Uuid::new_v4();
    // 2. 同一ID覆盖ACK超时后fallback；离线仅spool，禁止短命Hook创建活动runtime。
    let outcome = match ipc::call(
        &host.paths,
        &Request::Ingest {
            id,
            output: output.clone(),
            presence_suppressed,
        },
    ) {
        Ok(Response::Accepted { outcome }) => outcome,
        _ => host
            .queue
            .accept_identified_with_presence(output, id, true, presence_suppressed)
            .map_err(|_| PlatformError("ingress_not_persisted"))?,
    };
    if hook { hook_diagnostic(&host.paths, "persisted", "ok"); }
    // wake失败不撤销已持久事件；周期daemon扫描不依赖本次唤醒成功。
    if matches!(outcome, EnqueueOutcome::Spooled { .. }) {
        let _ = service::wake_detached(&host.paths);
    }
    if hook { Ok(0) } else { Ok(success(outcome)) }
}
