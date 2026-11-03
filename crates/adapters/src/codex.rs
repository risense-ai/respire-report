//! Codex正式Hook字段仅在此适配器解释；Stop只表示响应结束。
use crate::{Adapter, AdapterContext, AdapterError, field, parse_hook, required_id, source_classifier::{self, OriginProof, RuleSet}};
use agentnotify_core::event::*;
use std::{env, path::PathBuf};
pub struct Codex;
impl Adapter for Codex {
    fn normalize(&self, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError> {
        context.require_binding("codex")?;
        let value=parse_hook(raw)?;
        let name=field(&value,"hook_event_name").ok_or(AdapterError::new("unsupported_event"))?;
        let session=required_id(&value,"session_id")?;
        let actor_id=field(&value,"agent_id").map(str::to_owned);
        let subagent=matches!(name,"SubagentStart"|"SubagentStop");
        let actor_scope=if subagent {
            if actor_id.is_some() { ActorScope::Identified } else { ActorScope::Unknown }
        } else { ActorScope::Main };
        let turn=field(&value,"turn_id").map(str::to_owned);
        let mut evidence=SourceEvidence {
            source_class: if subagent { SourceClass::Subagent } else { SourceClass::Unknown },
            classification_reason: if subagent { "codex_subagent_event" } else { "source_unknown" }.into(),
            origin_evidence: OriginEvidence::VerifiedAdapter,
            background_result: BackgroundResult::Unknown,
        };
        if name=="Stop" {
            let proof=OriginProof {
                codex_home: context.codex_home.clone().ok_or(AdapterError::new("unverified_binding"))?,
                plugin_root: env::var_os("PLUGIN_ROOT").map(PathBuf::from),
                plugin_data: env::var_os("PLUGIN_DATA").map(PathBuf::from),
                hook_event: name.into(),
                registered_adapter: true,
            };
            evidence=source_classifier::classify(field(&value,"last_assistant_message").unwrap_or("").as_bytes(), &proof, &RuleSet::default());
        }
        let lifecycle_kind=match name {
            "SubagentStart"=>Some(LifecycleKind::Started),
            "SessionEnd"=>Some(LifecycleKind::SessionClosed),
            _=>None,
        };
        let notification_kind=match name {
            "Stop"|"SubagentStop"=>Some(EventKind::ResponseFinished),
            "Interrupt"=>Some(EventKind::Interrupted),
            "SubagentStart"|"SessionEnd"=>None,
            _=>return Err(AdapterError::new("unsupported_event")),
        };
        let lifecycle=lifecycle_kind.map(|kind| LifecycleSignal {
            instance_id: context.instance_id,
            session_id: session.clone(),
            actor_id: if kind==LifecycleKind::SessionClosed { None } else { actor_id.clone() },
            actor_scope: if kind==LifecycleKind::SessionClosed { ActorScope::Main } else { actor_scope },
            turn_id: if kind==LifecycleKind::SessionClosed { None } else { turn.clone() },
            upstream_id: None,
            kind, received_at: context.received_at,
        });
        // 元数据仅在受控Codex适配器有界读取；提问必须与本Hook明确轮次匹配。
        let metadata=crate::codex_metadata::lookup(&value,context,&session,turn.as_deref());
        let notification=notification_kind.map(|kind| NormalizedEventV1 {
            schema_version: 1,
            adapter_id: "codex".into(),
            instance_id: context.instance_id,
            event_id: None,
            session_id: Some(session),
            turn_id: turn,
            parent_session_id: None,
            agent_kind: if subagent { AgentKind::Subagent } else { AgentKind::Main },
            actor_id: if subagent { actor_id } else { None },
            actor_scope,
            kind,
            completion_basis: if kind==EventKind::ResponseFinished { CompletionBasis::ResponseStop } else { CompletionBasis::None },
            source_time: None,
            received_at: context.received_at,
            project_label: field(&value,"cwd").and_then(|s| std::path::Path::new(s).file_name()).and_then(|s| s.to_str()).map(str::to_owned),
            session_label: metadata.title,
            question: metadata.question,
            answer: field(&value,"last_assistant_message").map(str::to_owned),
            background_state: BackgroundState::Unknown,
            resume_url: None,
            watchdog_detail: None,
            content_key_available: context.content_key_available,
        });
        let output=AdapterOutput { notification, lifecycle, source_evidence: evidence };
        output.validate()?;
        Ok(output)
    }
}
