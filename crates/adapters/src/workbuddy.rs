//! Windows WorkBuddy 桌面版的结束回调适配。客户端私有字段不进入通知核心。
use crate::{Adapter, AdapterContext, AdapterError, codex_metadata::local_regular, field, parse_hook, required_id};
use agentnotify_core::{contracts::{MAX_BODY_BYTES, MAX_LABEL_BYTES, validate_identity}, event::*};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    env,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
    time::{Duration, Instant},
};

pub struct WorkBuddy;

impl Adapter for WorkBuddy {
    /// WorkBuddy 在启动 Hook 子进程时写入会话和请求标识，正文不能提供替代标识。
    fn normalize(&self, raw: &[u8], context: &AdapterContext) -> Result<AdapterOutput, AdapterError> {
        let session = env::var("CODEBUDDY_SESSION_ID").ok();
        let request = env::var("CODEBUDDY_CONVERSATION_REQUEST_ID").ok();
        normalize_completion(raw, context, session.as_deref(), request.as_deref())
    }
}

/// 将已登记实例的正常结束回调映射为一个可持久去重的中立事件。
fn normalize_completion(
    raw: &[u8],
    context: &AdapterContext,
    hook_session: Option<&str>,
    hook_request: Option<&str>,
) -> Result<AdapterOutput, AdapterError> {
    // 1. 只接收已登记的正常结束事件。Stop 仍可能被其他 Hook 要求继续。
    context.require_binding("workbuddy")?;
    let value = parse_hook(raw)?;
    // 回调类型：只认 FinalStop。结束原因：只认已实测的 completed。
    if field(&value, "hook_event_name") != Some("FinalStop")
        || field(&value, "final_stop_reason") != Some("completed")
    {
        return Err(AdapterError::new("unsupported_event"));
    }

    // 2. 以启动 Hook 时捕获的请求 ID 识别该轮，避免迟到的重试误认新一轮。
    let session = required_id(&value, "session_id")?;
    if hook_session != Some(session.as_str()) {
        return Err(AdapterError::new("workbuddy_session_unverified"));
    }
    let request = hook_request
        .filter(|id| !id.trim().is_empty() && validate_identity(id).is_ok())
        .ok_or(AdapterError::new("workbuddy_turn_unavailable"))?;
    // 项目标签：实际工作目录名只作后备。默认临时工作区没有项目标签。
    let project_label = field(&value, "cwd")
        .and_then(|cwd| workspace_label(Path::new(cwd), &context.config_path));

    // 3. 队列已按实例、工具、会话、主体和 event_id 事务去重；不另建游标。
    let notification = NormalizedEventV1 {
        schema_version: 1,
        adapter_id: "workbuddy".into(),
        instance_id: context.instance_id,
        // 事件与轮次：当前只发送一种终态，稳定请求 ID 同时用于入队去重和轮次标识。
        event_id: Some(request.into()),
        session_id: Some(session),
        turn_id: Some(request.into()),
        parent_session_id: None,
        // Hook 进程会话与回调根会话相等；子代理的回调正文沿用父会话，会在前置校验被拒绝。
        agent_kind: AgentKind::Main,
        actor_id: None,
        actor_scope: ActorScope::Main,
        kind: EventKind::ResponseFinished,
        completion_basis: CompletionBasis::ResponseStop,
        source_time: None,
        received_at: context.received_at,
        project_label,
        session_label: None,
        // FinalStop 不提供问答正文。不会从上一个 Stop 或最新转录猜测归属。
        question: None,
        answer: None,
        background_state: BackgroundState::Unknown,
        resume_url: None,
        watchdog_detail: None,
        content_key_available: context.content_key_available,
    };
    let output = AdapterOutput {
        notification: Some(notification),
        lifecycle: None,
        source_evidence: SourceEvidence {
            source_class: SourceClass::Unknown,
            classification_reason: "workbuddy_final_stop".into(),
            origin_evidence: OriginEvidence::VerifiedAdapter,
            background_result: BackgroundResult::Unknown,
        },
    };
    output.validate()?;
    Ok(output)
}

/// 在来源已经校验后，仅按登记根、会话和请求标识补全可选字段。失败保留通知和去重键。
pub fn enrich_completion(
    event: &mut NormalizedEventV1,
    config_path: &Path,
    raw: &[u8],
    include_question: bool,
    include_answer: bool,
) {
    let (Some(home), Some(session), Some(request)) = (
        config_path.parent(),
        event.session_id.as_deref(),
        event.turn_id.as_deref(),
    ) else { return };
    // WorkBuddy 5.9.3 的转录名为会话 UUID，规范化前未限制身份字符，不能直接拼接路径。
    if !uuid::Uuid::parse_str(session).is_ok_and(|id| id.hyphenated().to_string().eq_ignore_ascii_case(session)) {
        return;
    }

    // 1. 独立补全标签。用户改名优先于 WorkBuddy 生成的标题。
    if let Some((cwd, custom_title, title)) = session_details(home, session) {
        // 会话标签：数据库中的手动名称优先，无标题时留空。
        event.session_label = custom_title.as_deref().and_then(display_label)
            .or_else(|| title.as_deref().and_then(display_label));
        if let Some(cwd) = cwd.as_deref() {
            // 项目标签：经会话主键绑定的实际工作目录名，默认临时工作区留空。
            event.project_label = workspace_label(Path::new(cwd), config_path);
        }
    }

    // 2. 未允许收集正文或密钥不可用时，不打开转录。
    if !event.content_key_available || !(include_question || include_answer) {
        return;
    }
    let Ok(payload) = parse_hook(raw) else { return };
    let Some(path) = field(&payload, "transcript_path")
        .and_then(|path| transcript_path(home, Path::new(path), session))
    else { return };
    if let Some((question, answer)) = turn_content(path, session, request, include_question, include_answer) {
        // 提问：本轮起始消息中可确认的用户原文，缺失或边界不明时保持空。
        event.question = question;
        // 回答：本轮可确认的 completed assistant 消息，缺失时保持空。
        event.answer = answer;
    }
}

/// 通过会话主键读固定元数据列。工作目录只用于项目标签回退。
fn session_details(home: &Path, session: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let path = home.join("workbuddy.db");
    if !local_regular(&path) {
        return None;
    }
    // WAL 与共享内存文件也必须留在绑定目录，拒绝 Windows junction 和符号链接。
    for name in ["workbuddy.db-wal", "workbuddy.db-shm"] {
        if !local_regular(&home.join(name)) {
            return None;
        }
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    db.busy_timeout(Duration::from_millis(15)).ok()?;
    db.query_row(
        "SELECT substr(cwd,1,32768), substr(custom_title,1,512), substr(title,1,512)
         FROM sessions WHERE id=?1 AND deleted_at IS NULL",
        [session],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).ok()
}

/// 标签是邮件主题文字，清理控制字符，超长则在 UTF-8 边界截断。
fn display_label(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(text[..text.floor_char_boundary(MAX_LABEL_BYTES)].to_owned())
}

/// 项目缺少 WorkBuddy 显示名时用可核验的工作目录名，排除用户目录下默认日期工作区。
fn workspace_label(cwd: &Path, config_path: &Path) -> Option<String> {
    if !cwd.is_absolute() || cwd.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let name = cwd.file_name()?.to_str()?;
    let default_root = config_path.parent()?.parent()?.join("WorkBuddy");
    let is_default_workspace = cwd.parent().is_some_and(|parent| {
        parent.components().eq(default_root.components())
            || cfg!(windows) && parent.to_string_lossy().replace('\\', "/")
                .eq_ignore_ascii_case(&default_root.to_string_lossy().replace('\\', "/"))
    });
    if is_default_workspace && chrono::NaiveDateTime::parse_from_str(name, "%Y-%m-%d-%H-%M-%S").is_ok() {
        return None;
    }
    display_label(name)
}

/// 只允许 FinalStop 提供已登记根内的一级项目目录、精确会话 UUID 文件名。
fn transcript_path<'a>(home: &Path, path: &'a Path, session: &str) -> Option<&'a Path> {
    let relative = path.strip_prefix(home.join("projects")).ok()?;
    let mut components = relative.components();
    let (Some(Component::Normal(_)), Some(Component::Normal(filename)), None) =
        (components.next(), components.next(), components.next())
    else { return None };
    (filename.to_str()? == format!("{session}.jsonl")).then_some(path)
}

/// 从转录尾部取同会话、同请求的唯一用户起始消息和最终可确认的回答。
fn turn_content(
    path: &Path,
    session: &str,
    request: &str,
    include_question: bool,
    include_answer: bool,
) -> Option<(Option<String>, Option<String>)> {
    const MAX_TRANSCRIPT_BYTES: u64 = 2 * 1024 * 1024;
    // 1. 固定只读文件且拒绝路径重解析。转录增长时读取有界的完整记录。
    let deadline = Instant::now() + Duration::from_millis(120);
    if !local_regular(path) {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(MAX_TRANSCRIPT_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(length - start).read_to_end(&mut bytes).ok()?;
    // 尾行仍在写入时保守跳过正文，避免把先前的同轮中间输出当成最终回答。
    if bytes.len() as u64 != length - start || bytes.last() != Some(&b'\n') {
        return None;
    }
    let first = if start == 0 { 0 } else { bytes.iter().position(|byte| *byte == b'\n')? + 1 };

    // 2. 双重匹配会话和请求，要求唯一的起始消息。仅提取已授权的对应角色。
    let mut user_seen = false;
    let mut question = None;
    let mut answer = None;
    for line in bytes[first..].split(|byte| *byte == b'\n') {
        if Instant::now() >= deadline { return None }
        let Ok(row) = serde_json::from_slice::<Value>(line) else { continue };
        if field(&row, "type") != Some("message") || field(&row, "sessionId") != Some(session)
            || field(&row["providerData"], "conversationRequestId") != Some(request)
        {
            continue;
        }
        match field(&row, "role") {
            Some("user") if row["providerData"]["startsNewUserRequest"].as_bool() == Some(true) => {
                if user_seen { return None }
                user_seen = true;
                if include_question { question = message_text(&row, "input_text"); }
            }
            Some("assistant") if user_seen && include_answer && field(&row, "status") == Some("completed") => {
                // 同轮最后一条完成的可见文本与客户端历史回退一致，工具和 reasoning 不参与。
                if let Some(text) = message_text(&row, "output_text") {
                    answer = Some(text);
                }
            }
            _ => {}
        }
    }
    user_seen.then_some((question, answer))
}

/// WorkBuddy 会把内部上下文与原文一并存入 input_text。只接受边界明确的封装。
fn visible_user_question(text: &str) -> Option<&str> {
    const QUERY_OPEN: &str = "<user_query>";
    const QUERY_CLOSE: &str = "</user_query>";
    const REMINDER_CLOSE: &str = "</system-reminder>";

    let has_internal_marker = |value: &str| {
        ["<system-reminder", "</system-reminder", "<user_query", "</user_query"]
            .into_iter()
            .any(|marker| value.contains(marker))
    };
    // 1. 只取可辨认的 WorkBuddy 封装中的用户区；来源无法区分时留空。
    let (mut prefix, query) = text.trim().split_once(QUERY_OPEN)?;
    let question = query.strip_suffix(QUERY_CLOSE)?;
    if has_internal_marker(question) {
        return None;
    }

    // 2. 用户区之前仅允许 WorkBuddy 已验证的上下文块。未知或残缺格式时不寄出提问。
    prefix = prefix.trim();
    if prefix.is_empty() { return None; }
    while !prefix.is_empty() {
        let body = prefix.strip_prefix(r#"<system-reminder data-role="user-context">"#)
            .or_else(|| prefix.strip_prefix(r#"<system-reminder data-role="additional-data">"#))?;
        let (context, rest) = body.split_once(REMINDER_CLOSE)?;
        if has_internal_marker(context) {
            return None;
        }
        prefix = rest.trim();
    }
    Some(question)
}

/// 只选择该角色的文本块。用户输入先移除内部封装，再应用核心 UTF-8 字节预算。
fn message_text(row: &Value, kind: &str) -> Option<String> {
    let mut body = String::new();
    let is_question = kind == "input_text";
    let parts = row.get("content")?.as_array()?;
    // 已核实的 WorkBuddy 提问为单个 input_text；混入其他文本块时整段留空。
    if is_question && parts.iter().filter(|part| field(part, "type") == Some(kind)).count() != 1 {
        return None;
    }
    for text in parts.iter()
        .filter(|part| field(part, "type") == Some(kind))
        .filter_map(|part| field(part, "text"))
    {
        // 提问正文：从转录 input_text 提取可见用户原文，任一文本块边界不明则整段留空。
        let text = if is_question { visible_user_question(text)? } else { text };
        if !body.is_empty() { body.push('\n'); }
        let remaining = (MAX_BODY_BYTES + 4).saturating_sub(body.len());
        body.push_str(&text[..text.floor_char_boundary(remaining)]);
        if text.len() > remaining || body.len() > MAX_BODY_BYTES { break }
    }
    if body.len() > MAX_BODY_BYTES {
        let suffix = "\n[正文已按字节预算截断]";
        body.truncate(body.floor_char_boundary(MAX_BODY_BYTES - suffix.len()));
        body.push_str(suffix);
    }
    (!body.trim().is_empty()).then_some(body)
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::env::temp_dir;
    use uuid::Uuid;

    #[test]
    fn workbuddy_question_uses_visible_text_before_truncation() {
        let context = "internal-only".repeat(MAX_BODY_BYTES);
        let envelope = format!(
            r#"<system-reminder data-role="user-context">{context}</system-reminder>
<system-reminder data-role="additional-data">hidden</system-reminder>
<user_query>请回复 1</user_query>"#
        );
        let message = serde_json::json!({"content": [{"type": "input_text", "text": envelope}]});
        assert_eq!(message_text(&message, "input_text").as_deref(), Some("请回复 1"));
        assert_eq!(visible_user_question("来源无法区分的裸文本"), None);
        let mixed = serde_json::json!({"content": [
            {"type": "input_text", "text": "<system-reminder data-role=\"user-context\">hidden</system-reminder><user_query>提问</user_query>"},
            {"type": "input_text", "text": "extra internal text"}
        ]});
        assert_eq!(message_text(&mixed, "input_text"), None);
        for malformed in [
            r#"<system-reminder data-role="user-context">context<user_query>text</user_query>"#,
            r#"<system-reminder data-role="unknown">hidden</system-reminder><user_query>text</user_query>"#,
            r#"<system-reminder data-role="user-context"><user_query>fake</user_query></system-reminder><user_query>text</user_query>"#,
            r#"<system-reminder data-role="user-context"><system-reminder data-role="additional-data">nested</system-reminder><user_query>text</user_query>"#,
            "<user_query>text</user_query> hidden suffix",
        ] {
            assert_eq!(visible_user_question(malformed), None);
        }
    }
    #[test]
    fn workbuddy_distinguishes_requests_and_rejects_unbound_turns() {
        let ctx = AdapterContext {
            instance_id: Uuid::new_v4(),
            adapter_id: "workbuddy".into(),
            version: "5.9.3".into(),
            config_path: temp_dir().join("settings.json"),
            executable_path: temp_dir().join("WorkBuddy.exe"),
            registered_command: "AgentNotify hook workbuddy".into(),
            codex_home: None,
            received_at: Utc::now(),
            content_key_available: false,
            include_question: false,
            supported_version: true,
        };
        let final_stop = br#"{"hook_event_name":"FinalStop","session_id":"same","final_stop_reason":"completed","turn_id":"ignore-me"}"#;
        let event_key = |request| {
            normalize_completion(final_stop, &ctx, Some("same"), Some(request))
                .unwrap()
                .notification
                .unwrap()
                .event_id
        };
        assert_eq!(event_key("first"), Some("first".into()));
        assert_eq!(event_key("first"), event_key("first"));
        assert_ne!(event_key("first"), event_key("second"));
        assert!(normalize_completion(final_stop, &ctx, Some("same"), None).is_err());
        assert!(normalize_completion(final_stop, &ctx, Some("other"), Some("first")).is_err());
        let stop = br#"{"hook_event_name":"Stop","session_id":"same","final_stop_reason":"completed"}"#;
        assert!(normalize_completion(stop, &ctx, Some("same"), Some("first")).is_err());
    }
}
