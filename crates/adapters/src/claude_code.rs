//! Claude Code正式Hook字段；StopFailure与Stop语义分离。
use crate::{Adapter, AdapterContext, AdapterError, field, parse_hook, required_id};
use agentnotify_core::event::*;
pub struct ClaudeCode;
impl Adapter for ClaudeCode {
    fn normalize(&self, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError> {
        context.require_binding("claude_code")?;
        let value=parse_hook(raw)?;
        let name=field(&value,"hook_event_name").ok_or(AdapterError::new("unsupported_event"))?;
        let session=required_id(&value,"session_id")?;
        let subagent=matches!(name,"SubagentStart"|"SubagentStop");
        let actor_id=if subagent { field(&value,"agent_id").map(str::to_owned) } else { None };
        let actor_scope=if subagent {
            if actor_id.is_some() { ActorScope::Identified } else { ActorScope::Unknown }
        } else { ActorScope::Main };
        let turn=field(&value,"prompt_id").map(str::to_owned);
        let lifecycle_kind=match name {
            "SubagentStart"=>Some(LifecycleKind::Started),
            "SessionEnd"=>Some(LifecycleKind::SessionClosed),
            _=>None,
        };
        let notification_kind=match name {
            "Stop"|"SubagentStop"=>Some(EventKind::ResponseFinished),
            "StopFailure"=>Some(EventKind::ExecutionFailed),
            "SubagentStart"|"SessionEnd"=>None,
            _=>return Err(AdapterError::new("unsupported_event")),
        };
        let lifecycle=lifecycle_kind.map(|kind| LifecycleSignal {
            instance_id: context.instance_id,
            session_id: session.clone(),
            actor_id: if kind==LifecycleKind::SessionClosed { None } else { actor_id.clone() },
            actor_scope: if kind==LifecycleKind::SessionClosed { ActorScope::Main } else { actor_scope },
            turn_id: if kind==LifecycleKind::SessionClosed { None } else { turn.clone() },
            upstream_id: None, kind, received_at: context.received_at,
        });
        let background_state=if name=="Stop" {
            match (value.get("background_tasks").and_then(serde_json::Value::as_array),
                   value.get("session_crons").and_then(serde_json::Value::as_array)) {
                (Some(a),Some(b)) if a.is_empty() && b.is_empty()=>BackgroundState::None,
                (Some(a),_) if !a.is_empty()=>BackgroundState::Pending,
                (_,Some(b)) if !b.is_empty()=>BackgroundState::Pending,
                _=>BackgroundState::Unknown,
            }
        } else { BackgroundState::Unknown };
        let notification=notification_kind.map(|kind| NormalizedEventV1 {
            schema_version: 1,
            adapter_id: "claude_code".into(),
            instance_id: context.instance_id,
            event_id: None,
            session_id: Some(session),
            turn_id: turn,
            parent_session_id: None,
            agent_kind: if subagent { AgentKind::Subagent } else { AgentKind::Main },
            actor_id,
            actor_scope,
            kind,
            completion_basis: if kind==EventKind::ResponseFinished { CompletionBasis::ResponseStop } else { CompletionBasis::None },
            source_time: None,
            received_at: context.received_at,
            project_label: field(&value,"cwd").and_then(|s| std::path::Path::new(s).file_name()).and_then(|s| s.to_str()).map(str::to_owned),
            session_label: None,
            question: None,
            answer: if kind==EventKind::ResponseFinished { field(&value,"last_assistant_message").map(str::to_owned) } else { None },
            background_state,
            resume_url: None,
            watchdog_detail: None,
            content_key_available: context.content_key_available,
        });
        let output=AdapterOutput {
            notification, lifecycle,
            source_evidence: SourceEvidence {
                source_class: if subagent { SourceClass::Subagent } else { SourceClass::Unknown },
                classification_reason: if subagent { "claude_subagent_event" } else { "source_unknown" }.into(),
                origin_evidence: OriginEvidence::VerifiedAdapter,
                background_result: BackgroundResult::Unknown,
            },
        };
        output.validate()?;
        Ok(output)
    }
}
