//! 独立后台：GUI不持有生命周期；短事务入队/调度与SMTP线程分离。
use agentnotify_core::{
    clock::Clock,
    contracts::*,
    delivery::{
        mail::MailConfig,
        smtp::{SmtpAuth, SmtpConfig, deliver_lease},
    },
};
use agentnotify_platform::{
    PlatformError, Result,
    credentials::NativeCredentialStore,
    ipc::{Request, Response, Server},
    paths::PlatformPaths,
    power::PowerWatcher,
    runtime::HostRuntime,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
/// 停止接收新发送后等待在途SMTP落盘；意外退出由下一次租约恢复保留uncertain。
pub fn run(paths: PlatformPaths) -> Result<()> {
    // 1. 固定安全目录/队列/Policy实例，随后独占发送器；不按tick重新打开Queue。
    let host = Arc::new(HostRuntime::open(paths, true)?);
    let _sender = host
        .queue
        .acquire_sender()
        .map_err(|_| PlatformError("daemon_already_running_or_lock_failed"))?;
    let mut server = Server::bind(&host.paths)?;
    let power = PowerWatcher::register();
    host.clock.set_power_available(power.available);
    host.policy.set_runtime_clock(host.clock.clone());
    let mut power_epoch = power.epoch();
    for instance in host
        .queue
        .instances()
        .map_err(|_| PlatformError("activity_read_failed"))?
    {
        host.policy
            .suspend_persisted(&host.queue, instance)
            .map_err(|_| PlatformError("activity_suspend_failed"))?;
    }
    host.queue
        .recover_leases(host.clock.now())
        .map_err(|_| PlatformError("lease_recovery_failed"))?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let credential = Arc::new(Mutex::new(if host.settings.delivery.is_some() {
        CredentialStatus::Unknown
    } else {
        CredentialStatus::Missing
    }));
    let issues = Arc::new(Mutex::new(Vec::<HealthIssue>::new()));
    if !power.available {
        issues.lock().unwrap().push(HealthIssue {
            code: "power_notifications_unavailable".into(),
            message: "无法确认电源连续性，看门狗保持挂起。".into(),
        });
    }
    // 2. SMTP永远在线程内、SQLite写事务外执行；停止时不强杀DATA阶段。
    let worker = {
        let host = host.clone();
        let shutdown = shutdown.clone();
        let paused = paused.clone();
        let credential = credential.clone();
        let issues = issues.clone();
        std::thread::spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                if !paused.load(Ordering::SeqCst) {
                    if let Some(d) = &host.settings.delivery {
                        let result = (|| {
                            if !agentnotify_core::delivery::mail::valid_address(&d.from)
                                || d.recipients.is_empty()
                                || d.recipients.len() > 20
                                || d.recipients
                                    .iter()
                                    .any(|a| !agentnotify_core::delivery::mail::valid_address(a))
                            {
                                return Err(PlatformError("delivery_config_invalid"));
                            }
                            host.queue
                                .recover_leases(host.clock.now())
                                .map_err(|error| {
                                    #[cfg(windows)]
                                    let detail = agentnotify_platform::windows::take_last_verify_error().unwrap_or("not_directory_verification");
                                    #[cfg(not(windows))]
                                    let detail = "native_pending";
                                    record_issue(&issues, "lease_recovery_failed", &format!("租约恢复失败：{:?}/{detail}", error.code));
                                    PlatformError("lease_recovery_failed")
                                })?;
                            // 仅本轮租约恢复实际成功才撤销其旧告警，不清除其他诊断。
                            issues.lock().unwrap().retain(|issue| issue.code != "lease_recovery_failed");
                            let mut smtp = SmtpConfig::new(&d.host, d.port, &d.username, &d.tls)
                                .map_err(|_| PlatformError("delivery_config_invalid"))?;
                            if d.oauth {
                                smtp.auth = SmtpAuth::OAuth2;
                            }
                            let mail = MailConfig {
                                from: d.from.clone(),
                                recipients: d.recipients.clone(),
                                privacy: host.config.clone(),
                            };
                            // 单封领取避免等待前一封网络时耗尽批量租约。
                            let leases = host
                                .queue
                                .lease_due(host.clock.now(), 1)
                                .map_err(|_| PlatformError("lease_failed"))?;
                            for lease in leases {
                                let outcome = deliver_lease(
                                    &host.queue,
                                    &host.policy,
                                    &lease,
                                    &mail,
                                    smtp.clone(),
                                    &NativeCredentialStore::default(),
                                    &d.credential,
                                    host.clock.as_ref(),
                                )
                                .map_err(|_| PlatformError("delivery_state_failed"))?;
                                *credential.lock().unwrap() =
                                    if matches!(outcome, DeliveryOutcome::AuthBlocked { .. }) {
                                        CredentialStatus::Unavailable
                                    } else {
                                        CredentialStatus::Ready
                                    };
                            }
                            Ok::<_, PlatformError>(())
                        })();
                        if let Err(e) = result {
                            record_issue(&issues, e.0, "投递未完成；已保留队列状态。");
                        }
                    }
                }
                for _ in 0..10 {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
    };
    // 观察源扫描独立于控制管道，避免客户端数据库读取拖过 IPC 的 700ms 期限。
    let observer={let host=host.clone();let shutdown=shutdown.clone();let issues=issues.clone();std::thread::spawn(move||{
        while !shutdown.load(Ordering::SeqCst){
            match poll_observers(&host,&shutdown){
                Ok(())=>issues.lock().unwrap().retain(|issue|issue.code!="observer_degraded"),
                Err(e)=>record_issue(&issues,"observer_degraded",e.0),
            }
            for _ in 0..20 {if shutdown.load(Ordering::SeqCst){break;}std::thread::sleep(Duration::from_millis(100));}
        }
    })};
    let mut last_tick = Instant::now() - Duration::from_secs(15);
    let mut last_maintenance = Instant::now() - Duration::from_secs(60);
    let mut last_sample = Instant::now() - Duration::from_secs(1);
    let mut last_import = Instant::now() - Duration::from_millis(200);
    let result = (|| {
        while !shutdown.load(Ordering::SeqCst) {
            // 3. 恢复/重启先使旧活动挂起，随后新实时Started/Progress才可在本runtime计时。
            if last_sample.elapsed() >= Duration::from_secs(1) {
                let (snapshot, broken) = host.clock.sample();
                let changed = power.epoch() != power_epoch;
                power_epoch = power.epoch();
                if broken || changed {
                    host.policy.set_runtime_snapshot(snapshot.clone(), false);
                    for id in host
                        .queue
                        .instances()
                        .map_err(|_| PlatformError("activity_read_failed"))?
                    {
                        host.policy
                            .suspend_persisted(&host.queue, id)
                            .map_err(|_| PlatformError("activity_suspend_failed"))?;
                    }
                }
                host.policy.set_runtime_snapshot(
                    snapshot,
                    power.available && host.clock.continuity_available(),
                );
                last_sample = Instant::now();
            }
            if last_import.elapsed() >= Duration::from_millis(200) {
                match host.queue.import_spool() {
                    Ok(report) => {
                        if report.quarantined > 0 {
                            record_issue(&issues, "spool_items_quarantined", "无法验证来源、已撤销或损坏的暂存记录已隔离，原文件保留。");
                        }
                        // 仅完整成功扫描才能撤销当前导入故障；限时未完成仍保留告警。
                        if report.remaining == 0 {
                            issues.lock().unwrap().retain(|issue| issue.code != "spool_import_pending");
                        }
                    }
                    Err(error) => record_issue(
                        &issues,
                        "spool_import_pending",
                        &format!("暂存导入尚未完成（{:?}），将继续周期扫描。", error.code),
                    ),
                }
                last_import = Instant::now(); // remaining=0只代表本轮快照，不能停止未来扫描。
            }
            if last_tick.elapsed() >= Duration::from_secs(15) {
                for id in host
                    .queue
                    .instances()
                    .map_err(|_| PlatformError("activity_read_failed"))?
                {
                    if host.policy.flush_digests(&host.queue, id).is_err() {
                        record_issue(
                            &issues,
                            "digest_pending",
                            "摘要到期调度暂不可用，将继续尝试。",
                        );
                    }
                    if host.policy.evaluate_due(&host.queue, id).is_err() {
                        record_issue(&issues, "watchdog_pending", "活动评估暂不可用。");
                    }
                }
                last_tick = Instant::now();
            }
            if last_maintenance.elapsed() >= Duration::from_secs(60) {
                let _ = host.queue.maintain_retention(host.clock.now());
                last_maintenance = Instant::now();
            }
            // 4. IPC和控制循环不依赖网络；输入来源不能自行提升适配器能力。
            let polled = server.poll(|request| match request {
                Request::Health => Response::Health {
                    report: crate::health::report(
                        &host,
                        *credential.lock().unwrap(),
                        issues.lock().unwrap().clone(),
                    ),
                },
                Request::Stop => {
                    shutdown.store(true, Ordering::SeqCst);
                    Response::Ok
                }
                Request::Pause { paused: value } => {
                    paused.store(value, Ordering::SeqCst);
                    let mut config = host.config.clone();
                    config.enabled = host.config.enabled && !value;
                    match host.policy.set_config(config) {
                        Ok(()) => Response::Ok,
                        Err(_) => Response::Error {
                            code: "policy_invalid".into(),
                        },
                    }
                }
                Request::Ingest { id, output, presence_suppressed, admission_tag } => {
                    // IPC 客户端只通过当前用户 ACL 不足以证明事件由 AgentNotify 准入签发。
                    if !admission_tag.is_some_and(|tag| host.admission.verify_request(id, &output, presence_suppressed, &tag).is_ok()) {
                        return Response::Error { code: "ingress_unauthorized".into() };
                    }
                    match host.queue.accept_identified_with_presence(output, id, false, presence_suppressed) {
                        Ok(outcome) => Response::Accepted { outcome },
                        Err(_) => Response::Error {
                            code: "ingress_failed".into(),
                        },
                    }
                }
            });
            if polled.is_err() {
                record_issue(
                    &issues,
                    "ipc_peer_failed",
                    "一条本机请求未完成，持久队列继续保留。",
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    })();
    shutdown.store(true, Ordering::SeqCst);
    server.finish_response();
    // 停止响应已送出。观察读取异常卡住时不无限 join，也不能因观察失败跳过发送线程收尾。
    let observer_deadline=Instant::now()+Duration::from_secs(10);
    while !observer.is_finished() && Instant::now()<observer_deadline {std::thread::sleep(Duration::from_millis(50));}
    let observer_result=if observer.is_finished(){observer.join().map_err(|_|PlatformError("observer_thread_failed"))}else{Err(PlatformError("observer_shutdown_pending"))};
    let sender_result=worker.join().map_err(|_|PlatformError("sender_thread_failed"));
    // 超时按错误退出整个daemon，SQLite原子事务/稳定ID允许重启恢复；绝不报告干净完成。
    observer_result?;
    sender_result?;
    result
}
fn record_issue(issues: &Mutex<Vec<HealthIssue>>, code: &str, message: &str) {
    let mut list = issues.lock().unwrap();
    if !list.iter().any(|i| i.code == code) && list.len() < 32 {
        list.push(HealthIssue {
            code: code.into(),
            message: message.into(),
        });
    }
}

/// 宿主只编排已登记适配器与中立入队；客户端表结构完全留在适配器内。
fn poll_observers(host: &HostRuntime, shutdown: &AtomicBool) -> Result<()> {
    use agentnotify_adapters::codex_observer::{self, ObserverContext};
    use agentnotify_integration::IntegrationManager;

    // 1. 安全数据目录和本地实例登记是所有观察通道的共同前置条件。
    let security = host.paths.secure(false)?;
    let bindings = IntegrationManager::new(host.paths.data.clone()).bindings()
        .map_err(|_| PlatformError("observer_binding_unavailable"))?;
    let mut first_error = None;
    let mut incomplete = false;
    let mut reply_contract_changed = false;

    // 2. 各实例分别检查契约、入队和确认游标，损坏的实例不阻断后续实例。
    for binding in bindings.into_iter().filter(|binding| binding.adapter_id == "codex") {
        if shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            // 只有持久启用的实例拥有观察通道，原生 Hook 配置和旧 EXE 不参与校验。
            if !codex_observer::owns(&host.paths.data, binding.instance_id, security.as_ref())
                .map_err(|error| PlatformError(error.code))?
            {
                return Ok(());
            }
            let home = binding.codex_home.as_deref()
                .ok_or(PlatformError("observer_binding_unavailable"))?;
            let presence = agentnotify_platform::presence::suppress_at_receive(&host.config);
            let context = ObserverContext {
                instance_id: binding.instance_id,
                home,
                content_key_available: !host.content_unavailable,
                include_question: host.config.include_question && !presence,
            };
            let batch = codex_observer::poll(
                &host.paths.data, &context, security.as_ref(), presence, host.config.include_answer,
            ).map_err(|error| PlatformError(error.code))?;
            for event in batch.events {
                if shutdown.load(Ordering::SeqCst) {
                    return Ok(());
                }
                // 仅在核心确认持久接收后推进当前事件的游标，失败保留稳定 ID 供重试。
                host.queue.accept_identified_with_presence(
                    event.output.clone(), event.id, false, event.presence_suppressed,
                ).map_err(|_| PlatformError("observer_ingress_pending"))?;
                codex_observer::acknowledge(
                    &host.paths.data, context.instance_id, security.as_ref(), &event,
                ).map_err(|error| PlatformError(error.code))?;
            }
            incomplete |= batch.incomplete;
            reply_contract_changed |= batch.reply_contract_changed;
            Ok(())
        })();
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }

    // 3. 退出优先结束轮询。正常运行时，保留首个硬错误供后台诊断。
    if shutdown.load(Ordering::SeqCst) {
        return Ok(());
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if reply_contract_changed {
        return Err(PlatformError("observer_reply_contract_changed"));
    }
    if incomplete {
        return Err(PlatformError("observer_final_reply_pending"));
    }
    Ok(())
}
