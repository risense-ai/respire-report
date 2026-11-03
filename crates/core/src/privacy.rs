//! 正文隐私边界：默认清除，缺安全密钥不返回可落盘正文，不降级明文。
use crate::{config::ConfigV1, event::NormalizedEventV1};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyError {
    pub code: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
/// 仅瞬态DTO，不实现Serialize；存储层必须将保留正文单独AEAD封装。
pub struct SanitizedEvent {
    pub event: NormalizedEventV1,
    pub question_truncated: bool,
    pub answer_truncated: bool,
}
/// 按UTF-8字节截断；关闭字段在任何存储操作之前直接清除。
pub fn sanitize(
    mut event: NormalizedEventV1,
    config: &ConfigV1,
) -> Result<SanitizedEvent, PrivacyError> {
    config.validate().map_err(|_| PrivacyError {
        code: "invalid_config".into(),
    })?;
    // 1. 先清除用户未选择的正文，错误也不保留正文副本。
    if !config.include_question {
        event.question = None;
    }
    if !config.include_answer {
        event.answer = None;
    }
    // 2. 只在适配器接收上下文确认安全密钥可用后保留正文；JSON不能设置此标志。
    if (event.question.is_some() || event.answer.is_some()) && !event.content_key_available {
        return Err(PrivacyError {
            code: "content_key_unavailable".into(),
        });
    }
    fn truncate(value: &mut Option<String>) -> bool {
        if let Some(s) = value {
            if s.len() > 16384 {
                let mut end = 16384;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                s.truncate(end);
                return true;
            }
        }
        false
    }
    let question_truncated = truncate(&mut event.question);
    let answer_truncated = truncate(&mut event.answer);
    // 3. 语义错误同样失败关闭，返回码不包含正文。
    event
        .validate()
        .map_err(|e| PrivacyError { code: e.code })?;
    Ok(SanitizedEvent {
        event,
        question_truncated,
        answer_truncated,
    })
}
