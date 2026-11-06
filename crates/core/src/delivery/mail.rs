//! 纯文本邮件使用真实队列聚合/摘要数据，不能把响应结束升级成业务成功。
use crate::{
    config::ConfigV1,
    contracts::*,
    event::*,
    policy::Policy,
    queue::{Lease, SqliteQueue},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use uuid::Uuid;
#[derive(Clone)]
/// 邮箱是非秘密用户数据；不得放进诊断或日志。privacy是当前发送时隐私选择。
pub struct MailConfig {
    pub from: String,
    pub recipients: Vec<String>,
    pub privacy: ConfigV1,
}
#[derive(Debug, PartialEq, Eq)]
/// 邮件准备失败保留来源，只有已确认的邮件输入错误允许永久终止并清正文。
pub enum MailError {
    InvalidMail(&'static str),
    Queue(QueueError),
    ProtectionUnavailable,
    ConfigurationUnavailable,
}
/// 为主题保留项目、会话和事件语义，避免头字段超出整体512字节预算。
fn subject_label(text:&str,budget:usize)->&str{let mut end=text.len().min(budget);while !text.is_char_boundary(end){end-=1;}&text[..end]}
/// 具名工具共享单轮详情排版。tool 用于主题及回到客户端的指引，正文沿用隐私与安全预算。
fn detail_layout(e:&NormalizedEventV1,tool:&str,include_question:bool,include_answer:bool)->(String,String){
    let project=e.project_label.as_deref().filter(|s|!s.trim().is_empty()).unwrap_or("未知项目");
    let title=e.session_label.as_deref().filter(|s|!s.trim().is_empty()).unwrap_or("未命名会话");
    let short_title=if title.chars().count()>80 {format!("{}...",title.chars().take(77).collect::<String>())}else{title.to_owned()};
    let label=match e.kind{EventKind::ResponseFinished=>"本轮处理结束",EventKind::Interrupted=>"本轮已中断",EventKind::Stalled=>"会话疑似无进展",_=>if tool=="Codex" {"Codex 状态变化"} else {e.display_kind()}};
    let subject_title = if tool == "WorkBuddy" { subject_label(&short_title, 300) } else { &short_title };
    let subject=format!("{tool} 通知：{}｜{}｜{label}",subject_label(project,150),subject_title);
    let time=e.source_time.unwrap_or(e.received_at).with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string();
    let mut body=format!("{label}\n项目：{project}\n会话：{title}\n时间：{time}\n");
    if include_question || include_answer {
        body.push('\n');
        if include_question {body.push_str(&format!("本轮提问：\n{}\n",e.question.as_deref().unwrap_or("未取得对应轮次提问")));}
        if include_answer {if include_question{body.push('\n');}body.push_str(&format!("回答：\n{}\n",e.answer.as_deref().unwrap_or("未取得对应轮次回答")));}
    }
    if e.kind==EventKind::Stalled {body.push_str("\n检测到该轮长时间没有新的转录活动，可能正在等待处理、工具阻塞或已经暂停。\n");}
    body.push_str(&format!("\n请回到 {tool} 查看结果或处理待办。\n"));
    (subject,body)
}
/// 仅把动态文本作为文字展示，不解释模型HTML/Markdown，也不生成外部资源标签。
pub(super) fn html_escape(value:&str)->String{
    value.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('\"',"&quot;").replace('\'',"&#39;")
}
fn detail_html(e:&NormalizedEventV1,tool:&str,plain:&str,include_question:bool,include_answer:bool,truncated:bool)->String{
    let header=plain.lines().take(4).collect::<Vec<_>>().join("\n");
    let mut html=format!("<!doctype html><html><body style=\"margin:0;padding:24px;background:#ffffff;font-family:Arial,'Microsoft YaHei',sans-serif;font-size:15px;line-height:1.7;color:#273142\"><div style=\"white-space:pre-wrap;overflow-wrap:anywhere\">{}</div>",html_escape(&header));
    for (title,value,enabled,color) in [("本轮提问",e.question.as_deref().unwrap_or("未取得对应轮次提问"),include_question,"#1d4ed8"),("回答",e.answer.as_deref().unwrap_or("未取得对应轮次回答"),include_answer,"#047857")] {
        if enabled {html.push_str(&format!("<h2 style=\"margin:24px 0 8px;font-size:17px;font-weight:700;color:{color}\">{title}：</h2><div style=\"font-size:15px;color:#273142;overflow-wrap:anywhere\">{}</div>",super::markdown::render(value)));}
    }
    if e.kind==EventKind::Stalled {html.push_str("<p>检测到该轮长时间没有新的转录活动，可能正在等待处理、工具阻塞或已经暂停。</p>");}
    if truncated {html.push_str("<p>正文已按字节预算截断。</p>");}
    html.push_str(&format!("<p style=\"margin-top:24px;font-size:13px;color:#667085\">请回到 {} 查看结果或处理待办。</p></body></html>",html_escape(tool)));
    html
}
/// 首版只支持ASCII裸邮箱，拒绝显示名、SMTP注入及未声明SMTPUTF8的地址。
pub fn valid_address(s: &str) -> bool {
    s.len() <= 254
        && s.is_ascii()
        && s.split('@').count() == 2
        && s.split('@').all(|v| !v.is_empty())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~@".contains(&b))
}
/// 单事件预览；队列发送必须调用prepare_queued_mail以获取稳定ID和真实计数。
pub fn prepare_mail(
    event: &NormalizedEventV1,
    config: &MailConfig,
) -> Result<PreparedMail, MailError> {
    build(event, config, Uuid::new_v4(), 1, None)
}
/// 租约读取和摘要查询均结束后才进行网络发送；DATA门槛仍由SendJournal重新校验。
pub fn prepare_queued_mail(
    queue: &SqliteQueue,
    policy: &Policy,
    lease: &Lease,
    config: &MailConfig,
) -> Result<PreparedMail, MailError> {
    let (event, count) = queue.leased_mail_event(lease).map_err(MailError::Queue)?;
    let summary = policy
        .delivery_summary(queue, lease.id)
        .map_err(MailError::Queue)?;
    build(&event, config, lease.id, count, summary)
}
fn build(
    event: &NormalizedEventV1,
    config: &MailConfig,
    id: Uuid,
    count: u64,
    summary: Option<crate::policy::DeliverySummary>,
) -> Result<PreparedMail, MailError> {
    // 1. 所有头及地址先验证，隐私关闭时也不能让无效事件进入邮件。
    if !valid_address(&config.from)
        || config.recipients.is_empty()
        || config.recipients.len() > 20
        || config.recipients.iter().any(|r| !valid_address(r))
    {
        return Err(MailError::InvalidMail("invalid_address"));
    }
    // 先区分配置故障，避免unsupported_schema等共用错误码被误当成事件损坏。
    config
        .privacy
        .validate()
        .map_err(|_| MailError::ConfigurationUnavailable)?;
    let clean = crate::privacy::sanitize(event.clone(), &config.privacy).map_err(|e| {
        match e.code.as_str() {
            "content_key_unavailable" => MailError::ProtectionUnavailable,
            _ => MailError::InvalidMail("invalid_event"),
        }
    })?;
    let e = clean.event;
    // 来源工具由已归一化的适配器标识决定。摘要与其他适配器沿用通用展示。
    let detail_tool = if summary.is_none() {
        match e.adapter_id.as_str() {
            "codex" => Some("Codex"),
            "workbuddy" => Some("WorkBuddy"),
            _ => None,
        }
    } else {
        None
    };
    // 2. 摘要使用真实计数/标签，绝不显示代表事件的“本轮处理结束”或暂存正文。
    let (subject, mut body) = if let Some(s) = summary {
        if s.labels.iter().any(|l| l.contains(['\r', '\n'])) {
            return Err(MailError::InvalidMail("invalid_label"));
        }
        (
            "AgentNotify 通知摘要".to_owned(),
            format!("通知摘要：{} 条\n标签：{}\n", s.count, s.labels.join(" / ")),
        )
    } else if let Some(tool) = detail_tool {
        detail_layout(&e,tool,config.privacy.include_question,config.privacy.include_answer)
    } else {
        let mut b = format!(
            "{}\n项目：{}\n会话：{}\n",
            e.display_kind(),
            e.project_title(),
            e.session_title()
        );
        let occurred=e.source_time.unwrap_or(e.received_at).with_timezone(&chrono::Local);
        b.push_str(&format!("发生时间：{}\n",occurred.format("%Y-%m-%d %H:%M:%S %:z")));
        if count>1 {b.push_str(&format!("聚合事件：{count} 条\n"));}
        if let Some(d) = &e.watchdog_detail {
            b.push_str(&format!(
                "观察时长：{} 秒；阈值：{} 秒；最后观察：{}\n",
                d.elapsed_seconds, d.threshold_seconds, d.last_observed_at
            ));
        }
        if e.kind != EventKind::Stalled {
            for (label, value, included) in [("本轮提问", &e.question, config.privacy.include_question), ("回答", &e.answer, config.privacy.include_answer)] {
                if included {b.push_str(&format!("\n{label}：\n{}\n",value.as_deref().unwrap_or("未取得对应轮次内容")));}
            }
        }
        (format!("AgentNotify：{} | {} | {}",subject_label(e.project_title(),120),subject_label(e.session_title(),240),e.display_kind()), b)
    };
    if clean.question_truncated || clean.answer_truncated {
        body.push_str("\n正文已按字节预算截断。\n");
    }
    let html_body=detail_tool.map(|tool| SecretInput::new(detail_html(&e,tool,&body,config.privacy.include_question,config.privacy.include_answer,clean.question_truncated || clean.answer_truncated).into_bytes()));
    Ok(PreparedMail {
        message_id: format!("<{id}@agentnotify.local>"),
        from: config.from.clone(),
        recipients: config.recipients.clone(),
        subject,
        body: SecretInput::new(body.into_bytes()),
        html_body,
    })
}
/// 线协议再次验证公开PreparedMail边界；MIME Base64保证Unicode和任意正文不会注入SMTP命令。
pub(crate) fn wire(mail: &PreparedMail) -> Result<zeroize::Zeroizing<Vec<u8>>, MailError> {
    if !valid_address(&mail.from)
        || mail.recipients.is_empty()
        || mail.recipients.iter().any(|r| !valid_address(r))
        || mail.subject.contains(['\r', '\n'])
        || mail.subject.len() > 512
        || !mail.message_id.starts_with('<')
        || !mail.message_id.ends_with('>')
        || mail.message_id.len() > 128
        || mail
            .message_id
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ')
    {
        return Err(MailError::InvalidMail("invalid_headers"));
    }
    let subject = STANDARD.encode(mail.subject.as_bytes());
    let mut out=zeroize::Zeroizing::new(format!("From: {}\r\nTo: {}\r\nMessage-ID: {}\r\nSubject: =?UTF-8?B?{}?=\r\nMIME-Version: 1.0\r\n",mail.from,mail.recipients.join(", "),mail.message_id,subject).into_bytes());
    let boundary=format!("agentnotify-{}",Uuid::new_v4().simple());
    if mail.html_body.is_some(){out.extend_from_slice(format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n").as_bytes());}
    append_part(&mut out,"text/plain",&mail.body);
    if let Some(html)=&mail.html_body {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        append_part(&mut out,"text/html",html);
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    }
    out.extend_from_slice(b".\r\n");
    Ok(out)
}
/// 两种正文都独立Base64编码折行，正文无法构造MIME边界或SMTP控制行。
fn append_part(out:&mut Vec<u8>,mime:&str,body:&SecretInput){
    out.extend_from_slice(format!("Content-Type: {mime}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n").as_bytes());
    body.with_secret(|bytes|{
        let encoded=zeroize::Zeroizing::new(STANDARD.encode(bytes));
        for chunk in encoded.as_bytes().chunks(76){out.extend_from_slice(chunk);out.extend_from_slice(b"\r\n");}
    });
}

#[cfg(test)]
mod html_checks{
    use super::*;
    #[test]
    fn escaped_html_and_multipart_plain_fallback(){
        let event:NormalizedEventV1=serde_json::from_value(serde_json::json!({"schema_version":1,"adapter_id":"codex","instance_id":Uuid::nil(),"kind":"response_finished","completion_basis":"response_stop","received_at":"2026-11-03T00:00:00Z","project_label":"<project>&","session_label":"session","question":"<img src=\"https://invalid/remote\">&'question","answer":"<script>answer</script>"})).unwrap();
        let (_,plain)=codex_layout(&event,true,true);assert!(!plain.contains("最新内容："));
        let html=codex_html(&event,&plain,true,true,false);
        assert!(!html.contains("<img") && !html.contains("<script>"));
        assert!(html.contains("&lt;img src=&quot;") && html.contains("&amp;&#39;question"));
        assert!(html.contains("font-size:17px")&&html.contains("#1d4ed8")&&html.contains("#047857"));
        let hidden=codex_html(&event,&codex_layout(&event,false,false).1,false,false,false);
        assert!(!hidden.contains("question")&&!hidden.contains("answer"));
        let mail=PreparedMail{message_id:"<fixture@agentnotify.local>".into(),from:"sender@example.invalid".into(),recipients:vec!["recipient@example.invalid".into()],subject:"fixture".into(),body:SecretInput::new(plain.clone().into_bytes()),html_body:Some(SecretInput::new(html.clone().into_bytes()))};
        let wire=wire(&mail).unwrap();let text=std::str::from_utf8(&wire).unwrap();
        assert!(text.contains("Content-Type: multipart/alternative;")&&text.contains("Content-Type: text/plain;")&&text.contains("Content-Type: text/html;"));
        let compact=text.replace("\r\n","");assert!(compact.contains(&STANDARD.encode(plain)));assert!(compact.contains(&STANDARD.encode(html)));
        assert!(text.ends_with("--\r\n.\r\n"));
    }
}
