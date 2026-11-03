use agentnotify_core::{config::*, contracts::*, event::*, privacy::*};
use serde_json::{Value, json};
fn context() -> IngressContext {
    IngressContext {
        instance_id: uuid::Uuid::nil(),
        received_at: "2026-10-30T00:00:00Z".parse().unwrap(),
        content_key_available: false,
    }
}
fn event() -> Value {
    json!({"schema_version":1,"adapter_id":"generic","instance_id":uuid::Uuid::nil(),"kind":"response_finished","completion_basis":"response_stop"})
}
fn parse(v: Value) -> Result<AdapterOutput, InputError> {
    parse_generic(&serde_json::to_vec(&v).unwrap(), &context())
}
#[test]
fn schema_rejects_unknown_version() {
    let mut v = event();
    v["schema_version"] = json!(2);
    assert_eq!(parse(v).unwrap_err().code, "unsupported_schema");
}
#[test]
fn external_cannot_claim_trusted_source() {
    for key in [
        "source_evidence",
        "source_class",
        "origin_evidence",
        "classification_reason",
        "background_result",
        "received_at",
        "watchdog_detail",
    ] {
        let mut v = event();
        v[key] = json!("verified_adapter");
        assert_eq!(parse(v).unwrap_err().code, "reserved_field");
    }
    let out = parse(event()).unwrap();
    assert_eq!(
        out.source_evidence.origin_evidence,
        OriginEvidence::CallerReported
    );
    assert_eq!(out.source_evidence.source_class, SourceClass::Unknown);
}
#[test]
fn task_completed_requires_explicit_basis() {
    let mut v = event();
    v["kind"] = json!("task_completed");
    assert_eq!(parse(v.clone()).unwrap_err().code, "invalid_completion");
    v["completion_basis"] = json!("explicit_task_event");
    assert_eq!(parse(v.clone()).unwrap_err().code, "invalid_completion");
    v["completion_basis"] = json!("explicit_caller_report");
    assert_eq!(
        parse(v).unwrap().notification.unwrap().display_kind(),
        "调用方报告完成"
    );
    assert_eq!(
        parse(event()).unwrap().notification.unwrap().display_kind(),
        "本轮处理结束"
    );
}
#[test]
fn missing_titles_are_explicit() {
    let e = parse(event()).unwrap().notification.unwrap();
    assert_eq!(e.project_title(), "项目未提供");
    assert_eq!(e.session_title(), "会话未提供");
    assert!(e.session_id.is_none() && e.turn_id.is_none());
}
#[test]
fn utf8_limits_and_no_body_when_disabled() {
    let mut v = event();
    v["question"] = json!("密".repeat(6000));
    v["answer"] = json!("SECRET");
    let e = parse(v).unwrap().notification.unwrap();
    let clean = sanitize(e.clone(), &ConfigV1::default()).unwrap();
    assert!(clean.event.question.is_none() && clean.event.answer.is_none());
    let mut cfg = ConfigV1::default();
    cfg.include_question = true;
    cfg.include_answer = true;
    assert_eq!(
        sanitize(e.clone(), &cfg).unwrap_err().code,
        "content_key_unavailable"
    );
    let mut e = e;
    e.content_key_available = true;
    let clean = sanitize(e, &cfg).unwrap();
    assert_eq!(clean.event.question.as_ref().unwrap().len(), 16383);
    assert!(clean.question_truncated);
    assert_eq!(clean.event.answer.as_deref(), Some("SECRET"));
}
#[test]
fn identity_metadata_and_raw_budgets() {
    let mut v = event();
    v["event_id"] = json!("x".repeat(256));
    assert!(parse(v.clone()).is_ok());
    v["event_id"] = json!("x".repeat(257));
    assert_eq!(parse(v).unwrap_err().code, "invalid_identity");
    let mut v = event();
    v["project_label"] = json!("界".repeat(171));
    assert_eq!(parse(v).unwrap_err().code, "invalid_label");
    let mut raw = serde_json::to_vec(&event()).unwrap();
    raw.resize(1024 * 1024, b' ');
    assert!(parse_generic(&raw, &context()).is_ok());
    raw.push(b' ');
    assert_eq!(
        parse_generic(&raw, &context()).unwrap_err().code,
        "input_too_large"
    );
    let mut out = parse(event()).unwrap();
    out.source_evidence.classification_reason = "x".repeat(8192);
    assert_eq!(out.validate().unwrap_err().code, "metadata_too_large");
}
#[test]
fn schema_enum_and_order_are_strict() {
    assert!(serde_json::from_str::<QueueStatus>("\"alien\"").is_err());
    assert!(serde_json::from_str::<IntegrationStatus>("\"alien\"").is_err());
    let env = IngressEnvelopeV1 {
        presence_suppressed: false,
        schema_version: 1,
        ingress_id: uuid::Uuid::nil(),
        received_at: context().received_at,
        per_instance_sequence: None,
        adapter_output: parse(event()).unwrap(),
    };
    assert_eq!(env.diagnostics(), vec!["order_unknown"]);
}
#[test]
fn config_defaults_and_atomic_version_checks() {
    let cfg = ConfigV1::default();
    assert!(cfg.enabled && cfg.quiet.enabled);
    assert!(!cfg.include_question && !cfg.include_answer && !cfg.watchdog.enabled);
    assert_eq!(cfg.watchdog.threshold_seconds, 1800);
    assert_eq!(cfg.quiet.start, "23:00");
    assert_eq!(cfg.quiet.end, "07:30");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    cfg.save_atomic(&path).unwrap();
    assert_eq!(ConfigV1::load(&path).unwrap(), cfg);
    let mut bad = cfg.clone();
    bad.schema_version = 2;
    assert!(bad.save_atomic(&path).is_err());
    assert_eq!(ConfigV1::load(&path).unwrap(), cfg);
    assert!(
        cfg.watchdog
            .validate_capabilities(&AdapterCapabilities::default())
            .is_ok()
    );
    let mut w = cfg.watchdog;
    w.enabled = true;
    assert!(
        w.validate_capabilities(&AdapterCapabilities::default())
            .is_err()
    );
}
#[test]
fn generated_artifacts_do_not_drift() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, generated) in generated_artifacts() {
        assert_eq!(
            std::fs::read_to_string(root.join(path)).unwrap(),
            generated,
            "regenerate {path}"
        );
    }
}
#[test]
fn envelope_metadata_and_reception_time_are_checked() {
    let mut env = IngressEnvelopeV1 {
        presence_suppressed: false,
        schema_version: 1,
        ingress_id: uuid::Uuid::nil(),
        received_at: context().received_at,
        per_instance_sequence: Some(1),
        adapter_output: parse(event()).unwrap(),
    };
    env.adapter_output
        .notification
        .as_mut()
        .unwrap()
        .received_at = "2026-10-29T00:00:00Z".parse().unwrap();
    assert_eq!(env.validate().unwrap_err().code, "context_mismatch");
}
#[test]
fn stalled_semantics_and_capabilities_are_not_business_failure() {
    let mut e = parse(event()).unwrap().notification.unwrap();
    e.kind = EventKind::Stalled;
    e.completion_basis = CompletionBasis::None;
    e.watchdog_detail = Some(WatchdogDetail {
        mode: WatchdogMode::ElapsedWithoutFinish,
        threshold_seconds: 1800,
        elapsed_seconds: 1801,
        last_observed_at: context().received_at,
    });
    e.validate().unwrap();
    assert_eq!(e.display_kind(), "本轮运行时间较长，尚未收到结束事件");
    e.watchdog_detail.as_mut().unwrap().mode = WatchdogMode::Inactivity;
    assert_eq!(e.display_kind(), "疑似长时间无进展");
    let w = WatchdogConfig {
        enabled: true,
        mode: WatchdogMode::Inactivity,
        threshold_seconds: 1800,
    };
    let mut c = AdapterCapabilities {
        reliable_started: true,
        reliable_actor: true,
        reliable_turn_or_generation: true,
        reliable_progress: false,
    };
    assert_eq!(
        w.validate_capabilities(&c).unwrap_err().code,
        "progress_capability_required"
    );
    c.reliable_progress = true;
    w.validate_capabilities(&c).unwrap();
}
#[test]
fn generic_cannot_bypass_body_key_guard() {
    let mut v = event();
    v["content_key_available"] = json!(true);
    assert_eq!(parse(v).unwrap_err().code, "reserved_field");
}
#[test]
fn fixed_enum_values_roundtrip_without_unknown_success() {
    for value in [
        "response_finished",
        "attention_required",
        "execution_failed",
        "interrupted",
        "task_completed",
        "stalled",
    ] {
        let kind: EventKind = serde_json::from_value(json!(value)).unwrap();
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(value));
    }
    for value in [
        "response_stop",
        "explicit_task_event",
        "explicit_caller_report",
        "none",
    ] {
        assert!(serde_json::from_value::<CompletionBasis>(json!(value)).is_ok());
    }
    for value in [
        "not_detected",
        "detected",
        "unsupported_version",
        "research_pending",
        "configured",
        "trust_pending",
        "reload_pending",
        "probe_passed",
        "live_verified",
        "degraded",
    ] {
        assert!(serde_json::from_value::<IntegrationStatus>(json!(value)).is_ok());
    }
    for value in [
        "queued",
        "leased",
        "sent",
        "suppressed",
        "digest_pending",
        "retry_wait",
        "auth_blocked",
        "delivery_unknown",
        "expired",
        "permanent_failed",
    ] {
        assert!(serde_json::from_value::<QueueStatus>(json!(value)).is_ok());
    }
    let mut e = parse(event()).unwrap().notification.unwrap();
    e.kind = EventKind::TaskCompleted;
    e.completion_basis = CompletionBasis::ExplicitTaskEvent;
    e.validate().unwrap();
    assert_eq!(e.display_kind(), "收到明确任务完成事件");
}
#[test]
fn lifecycle_evidence_and_actor_keys_are_shared_without_guessing() {
    let signal = LifecycleSignal {
        instance_id: context().instance_id,
        session_id: "s".into(),
        actor_id: Some("child-a".into()),
        actor_scope: ActorScope::Identified,
        turn_id: None,
        upstream_id: None,
        kind: LifecycleKind::Started,
        received_at: context().received_at,
    };
    let out = AdapterOutput {
        notification: None,
        lifecycle: Some(signal),
        source_evidence: SourceEvidence {
            source_class: SourceClass::InternalBackground,
            classification_reason: "controlled_fixture".into(),
            origin_evidence: OriginEvidence::VerifiedAdapter,
            background_result: BackgroundResult::Unknown,
        },
    };
    out.validate().unwrap();
    let bytes = serde_json::to_vec(&out).unwrap();
    let roundtrip: AdapterOutput = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(roundtrip, out);
    let mut e = parse(event()).unwrap().notification.unwrap();
    e.session_id = Some("s".into());
    e.actor_scope = ActorScope::Unknown;
    let first = ActorKey::from_event(&e, uuid::Uuid::from_u128(1));
    let second = ActorKey::from_event(&e, uuid::Uuid::from_u128(2));
    assert_ne!(first, second);
    e.actor_scope = ActorScope::Identified;
    e.actor_id = Some("child-a".into());
    let a = ActorKey::from_event(&e, uuid::Uuid::nil());
    e.actor_id = Some("child-b".into());
    assert_ne!(a, ActorKey::from_event(&e, uuid::Uuid::nil()));
}
#[test]
fn metadata_exact_boundary_and_utf8_identity_are_enforced() {
    let mut out = parse(event()).unwrap();
    let base = serde_json::to_vec(&out).unwrap().len();
    out.source_evidence
        .classification_reason
        .push_str(&"x".repeat(8192 - base));
    assert_eq!(serde_json::to_vec(&out).unwrap().len(), 8192);
    out.validate().unwrap();
    out.source_evidence.classification_reason.push('x');
    assert_eq!(out.validate().unwrap_err().code, "metadata_too_large");
    let mut v = event();
    v["event_id"] = json!("界".repeat(85));
    assert!(parse(v.clone()).is_ok());
    v["event_id"] = json!("界".repeat(86));
    assert_eq!(parse(v).unwrap_err().code, "invalid_identity");
}

#[test]
fn json_integer_precision_boundary_is_enforced_on_input_and_output() {
    let safe = 9_007_199_254_740_991_u64;
    let mut envelope = IngressEnvelopeV1 {
        presence_suppressed: false,
        schema_version: 1,
        ingress_id: uuid::Uuid::nil(),
        received_at: context().received_at,
        per_instance_sequence: Some(safe),
        adapter_output: parse(event()).unwrap(),
    };
    let raw = serde_json::to_vec(&envelope).unwrap();
    assert_eq!(
        serde_json::from_slice::<IngressEnvelopeV1>(&raw)
            .unwrap()
            .per_instance_sequence,
        Some(safe)
    );
    envelope.per_instance_sequence = Some(safe + 1);
    assert!(envelope.validate().is_err());
    assert!(serde_json::to_vec(&envelope).is_err());
    let mut value: Value = serde_json::from_slice(&raw).unwrap();
    value["per_instance_sequence"] = json!(safe + 1);
    assert!(serde_json::from_value::<IngressEnvelopeV1>(value).is_err());
    assert_eq!(
        serde_json::to_value(TurnKey::Generation(safe)).unwrap()["value"],
        json!(safe)
    );
    assert!(serde_json::to_value(TurnKey::Generation(safe + 1)).is_err());
    assert!(
        serde_json::from_value::<TurnKey>(json!({"kind":"generation","value":safe+1})).is_err()
    );
}

#[test]
fn shared_policy_delivery_and_ingress_contracts_are_importable_and_typed() {
    struct OfflineIngress;
    impl Ingress for OfflineIngress {
        fn accept(&self, _: AdapterOutput) -> Result<EnqueueOutcome, QueueError> {
            Err(QueueError {
                code: QueueErrorCode::StorageUnavailable,
            })
        }
    }
    struct OfflineQueue;
    impl Queue for OfflineQueue {
        fn ingest(&self, _: IngressEnvelopeV1) -> Result<EnqueueOutcome, QueueError> {
            Err(QueueError {
                code: QueueErrorCode::StorageUnavailable,
            })
        }
    }
    assert_eq!(
        OfflineIngress
            .accept(parse(event()).unwrap())
            .unwrap_err()
            .code,
        QueueErrorCode::StorageUnavailable
    );
    let decision = PolicyDecision::Notify {
        not_before: context().received_at,
    };
    assert_eq!(
        serde_json::to_value(decision).unwrap(),
        json!({"decision":"notify","not_before":"2026-10-30T00:00:00Z"})
    );
    for stage in [
        SendStage::Prepared,
        SendStage::Connecting,
        SendStage::Envelope,
        SendStage::DataStarted,
        SendStage::Accepted,
    ] {
        let value = serde_json::to_value(stage).unwrap();
        assert_eq!(serde_json::from_value::<SendStage>(value).unwrap(), stage);
    }
    assert_eq!(
        serde_json::to_value(DeliveryOutcome::Uncertain {
            stage: SendStage::DataStarted
        })
        .unwrap(),
        json!({"outcome":"uncertain","stage":"data_started"})
    );
    let _queue: &dyn Queue = &OfflineQueue;
}

#[test]
fn integration_transitions_require_configuration_and_evidence_stages() {
    use IntegrationStatus::*;
    let mut state = NotDetected;
    for next in [
        Detected,
        Configured,
        TrustPending,
        ReloadPending,
        Configured,
        ProbePassed,
        LiveVerified,
        Degraded,
        Configured,
        ProbePassed,
        LiveVerified,
    ] {
        state = state.transition_to(next).unwrap();
        assert_eq!(state, next);
    }
    for (from, to) in [
        (NotDetected, LiveVerified),
        (Detected, ProbePassed),
        (Configured, LiveVerified),
        (TrustPending, LiveVerified),
        (TrustPending, ProbePassed),
        (ReloadPending, ProbePassed),
        (UnsupportedVersion, Configured),
        (ResearchPending, LiveVerified),
        (Degraded, LiveVerified),
    ] {
        assert_eq!(
            from.transition_to(to).unwrap_err().code,
            "invalid_integration_transition"
        );
    }
    assert_eq!(
        LiveVerified.transition_to(NotDetected).unwrap(),
        NotDetected
    );
    assert_eq!(
        LiveVerified.transition_to(LiveVerified).unwrap(),
        LiveVerified
    );
}

#[test]
fn credential_transport_contracts_do_not_require_platform_dependencies() {
    struct NoCredentials;
    impl CredentialStore for NoCredentials {
        fn get(&self, _: &CredentialRef) -> Result<CredentialHandle, CredentialError> {
            Err(CredentialError {
                code: CredentialErrorCode::Unavailable,
            })
        }
        fn put(&self, _: &CredentialRef, _: SecretInput) -> Result<(), CredentialError> {
            Err(CredentialError {
                code: CredentialErrorCode::Unavailable,
            })
        }
        fn delete(&self, _: &CredentialRef) -> Result<(), CredentialError> {
            Err(CredentialError {
                code: CredentialErrorCode::Unavailable,
            })
        }
    }
    struct NoTransport;
    impl Transport for NoTransport {
        fn send(&self, _: &PreparedMail, _: &CredentialHandle) -> DeliveryOutcome {
            DeliveryOutcome::AuthBlocked {
                code: "credential_unavailable".into(),
            }
        }
    }
    let reference = CredentialRef {
        id: uuid::Uuid::nil(),
    };
    let handle = CredentialHandle::from_secret(SecretInput::new(b"ISOLATED_TEST_VALUE".to_vec()));
    assert_eq!(handle.with_secret(|secret| secret.len()), 19);
    let error = NoCredentials.get(&reference).err().unwrap();
    assert_eq!(error.code, CredentialErrorCode::Unavailable);
    let _transport: &dyn Transport = &NoTransport;
    let report = HealthReport {
        schema_version: 1,
        daemon: DaemonStatus::Unavailable,
        queue_counts: QueueCounts::default(),
        credential_status: CredentialStatus::Unavailable,
        integrations: vec![],
        issues: vec![],
    };
    assert_eq!(
        serde_json::to_value(report).unwrap()["credential_status"],
        "unavailable"
    );
}

#[test]
fn optional_sequence_missing_null_and_integer_bounds_match_schema() {
    let envelope = IngressEnvelopeV1 {
        presence_suppressed: false,
        schema_version: 1,
        ingress_id: uuid::Uuid::nil(),
        received_at: context().received_at,
        per_instance_sequence: Some(9_007_199_254_740_991),
        adapter_output: parse(event()).unwrap(),
    };
    let base = serde_json::to_value(&envelope).unwrap();
    let mut missing = base.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("per_instance_sequence");
    let parsed: IngressEnvelopeV1 = serde_json::from_value(missing).unwrap();
    assert_eq!(parsed.per_instance_sequence, None);
    assert_eq!(parsed.diagnostics(), vec!["order_unknown"]);
    parsed.validate().unwrap();
    let mut null = base.clone();
    null["per_instance_sequence"] = Value::Null;
    let parsed: IngressEnvelopeV1 = serde_json::from_value(null).unwrap();
    assert_eq!(parsed.per_instance_sequence, None);
    assert_eq!(parsed.diagnostics(), vec!["order_unknown"]);
    parsed.validate().unwrap();
    let parsed: IngressEnvelopeV1 = serde_json::from_value(base.clone()).unwrap();
    assert_eq!(parsed.per_instance_sequence, Some(9_007_199_254_740_991));
    assert!(parsed.diagnostics().is_empty());
    parsed.validate().unwrap();
    let mut overflow = base;
    overflow["per_instance_sequence"] = json!(9_007_199_254_740_992_u64);
    assert!(serde_json::from_value::<IngressEnvelopeV1>(overflow).is_err());
    let schema = serde_json::to_value(schemars::schema_for!(IngressEnvelopeV1)).unwrap();
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("per_instance_sequence"))
    );
    assert_eq!(
        schema["properties"]["per_instance_sequence"]["type"],
        json!(["integer", "null"])
    );
    assert_eq!(
        schema["properties"]["per_instance_sequence"]["maximum"],
        json!(9_007_199_254_740_991_u64)
    );
}
