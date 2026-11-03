//! Codex 0.166/0.165的可选有界补全；失败保留通知，不读取其他会话或猜测轮次。
use crate::{AdapterContext,field};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::{fs::{self,File}, io::{BufRead,BufReader,Read,Seek,SeekFrom}, path::Path, time::{Duration,Instant}};
#[derive(Default)]
pub struct Metadata { pub question:Option<String>, pub title:Option<String> }
fn bounded(text:&str,max:usize)->String { let mut end=text.len().min(max); while !text.is_char_boundary(end){end-=1;} text[..end].to_owned() }
pub(crate) fn local_regular(path:&Path)->bool {
    if !path.is_absolute() || path.components().any(|p|matches!(p,std::path::Component::ParentDir)) {return false;}
    for p in path.ancestors() {
        let Ok(m)=fs::symlink_metadata(p) else {return false;};
        if m.file_type().is_symlink(){return false;}
        #[cfg(windows)] { use std::os::windows::fs::MetadataExt; if m.file_attributes()&0x400!=0{return false;} }
    }
    fs::metadata(path).is_ok_and(|m|m.is_file())
}
/// 固定文件名和精确主键查询；数据库仅在已知版本/当前注册CODEX_HOME下打开只读。
fn title(home:&Path,session:&str)->Option<String> {
    let path=home.join("state_5.sqlite");
    if !local_regular(&path){return None;}
    let db=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    db.busy_timeout(Duration::from_millis(15)).ok()?;
    let text:Option<String>=db.query_row("SELECT substr(title,1,512) FROM threads WHERE id=?1",[session],|r|r.get(0)).optional().ok().flatten();
    text.filter(|s|!s.trim().is_empty()).map(|s|bounded(&s,512))
}
fn turn_text(path:&Path,home:&Path,session:&str,target:&str,deadline:Instant,message_type:&str)->Option<String> {
    if !local_regular(path){return None;}
    let canonical=path.canonicalize().ok()?;
    let sessions=home.join("sessions").canonicalize().ok()?;
    if !canonical.starts_with(sessions){return None;}
    let mut file=File::open(canonical).ok()?;
    // 第一条session_meta必须绑定当前session；不信任文件名或payload路径本身。
    let mut head=Vec::new();
    BufReader::new((&mut file).take(65536)).read_until(b'\n',&mut head).ok()?;
    let first:Value=serde_json::from_slice(&head).ok()?;
    if first.get("type").and_then(Value::as_str)!=Some("session_meta") || first.pointer("/payload/id").and_then(Value::as_str)!=Some(session){return None;}
    let length=file.metadata().ok()?.len();
    let start=length.saturating_sub(2*1024*1024);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes=Vec::new(); (&mut file).take(2*1024*1024+1).read_to_end(&mut bytes).ok()?;
    if bytes.len()>2*1024*1024{return None;}
    let mut current:Option<String>=None; let mut found=None;
    for (index,line) in bytes.split(|b|*b==b'\n').enumerate() {
        if Instant::now()>=deadline{return None;}
        if (start>0&&index==0)||line.len()>1024*1024{continue;}
        let Ok(v)=serde_json::from_slice::<Value>(line) else {continue;};
        let kind=v.get("type").and_then(Value::as_str).unwrap_or("");
        let p=&v["payload"]; let event=field(p,"type").unwrap_or("");
        if kind=="turn_context" || (kind=="event_msg"&&event=="task_started") {
            current=field(p,"turn_id").map(str::to_owned);
        }
        if kind=="event_msg" && event==message_type {
            let explicit=field(p,"turn_id");
            let belongs=explicit.or(current.as_deref())==Some(target);
            if belongs { found=field(p,"message").map(|s| {
                // 用户消息先整理客户端附件封套，再应用同一正文预算；回答不经过封套处理。
                let display=if message_type=="user_message" {super::codex_question::display_question(s)}else{std::borrow::Cow::Borrowed(s)};
                bounded(&display,16*1024)
            }); }
        }
    }
    found
}
pub fn lookup(payload:&Value,context:&AdapterContext,session:&str,turn:Option<&str>)->Metadata {
    if !matches!(context.version.as_str(),"0.166.0-alpha.5.1"|"0.165.0-alpha.1.2"){return Metadata::default();}
    let Some(home)=context.codex_home.as_deref() else {return Metadata::default();};
    let deadline=Instant::now()+Duration::from_millis(75);
    let title=title(home,session);
    // 未开启正文、无安全密钥或无明确turn时不读取transcript正文。
    let question=if context.include_question && context.content_key_available && Instant::now()<deadline {
        turn.and_then(|id|field(payload,"transcript_path").and_then(|p|turn_text(Path::new(p),home,session,id,deadline,"user_message")))
    } else {None};
    Metadata{question,title}
}

/// 当前投影主键绑定精确轮次首条用户消息与最终回答；关闭的正文不查询 item_json。
#[derive(Default)]
pub(crate) struct TurnContent {pub question:Option<String>,pub answer:Option<String>}
pub(crate) fn turn_content(home:&Path,session:&str,turn:&str,question:bool,answer:bool)->TurnContent{
    if !question && !answer{return TurnContent::default();}
    let path=home.join("thread_history_1.sqlite");
    if !local_regular(&path){return TurnContent::default();}
    let read=||->Option<TurnContent>{
        let db=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
        db.busy_timeout(Duration::from_millis(25)).ok()?;
        let ids:(Option<String>,Option<String>)=db.query_row("SELECT first_user_item_id,final_agent_item_id FROM thread_turns WHERE thread_id=?1 AND turn_id=?2",rusqlite::params![session,turn],|r|Ok((r.get(0)?,r.get(1)?))).optional().ok()??;
        let item=|id:Option<String>,kind:&str|->Option<String>{
            let id=id?;
            let raw:String=db.query_row("SELECT item_json FROM thread_items WHERE thread_id=?1 AND turn_id=?2 AND item_id=?3 AND item_type=?4 AND length(CAST(item_json AS BLOB))<=1048576",rusqlite::params![session,turn,id,kind],|r|r.get(0)).optional().ok()??;
            projected_text(&raw,&id,kind)
        };
        Some(TurnContent{question:if question{item(ids.0,"userMessage")}else{None},answer:if answer{item(ids.1,"agentMessage")}else{None}})
    };
    read().unwrap_or_default()
}
/// JSON类型、item id 与投影键再核对；最终回答不接受 reasoning/commentary 或邻轮消息。
fn projected_text(raw:&str,id:&str,kind:&str)->Option<String>{
    let value:Value=serde_json::from_str(raw).ok()?;
    if field(&value,"type")!=Some(kind)||field(&value,"id")!=Some(id){return None;}
    let text=if kind=="agentMessage" {
        if field(&value,"phase").is_some_and(|p|p!="final_answer"){return None;}
        field(&value,"text")?.to_owned()
    }else{
        value.get("content")?.as_array()?.iter().filter(|v|field(v,"type")==Some("text")).filter_map(|v|field(v,"text")).collect::<Vec<_>>().join("\n")
    };
    let text=if kind=="userMessage" {super::codex_question::display_question(&text).into_owned()} else {text};
    if text.trim().is_empty(){return None;}
    let suffix="\n[正文已按适配器字节预算截断]";
    Some(if text.len()>16*1024 {format!("{}{}",bounded(&text,16*1024-suffix.len()),suffix)}else{text})
}
#[cfg(test)]
mod content_checks{
    use super::*;
    #[test]
    fn exact_projected_item_and_final_phase(){
        assert_eq!(projected_text(r#"{"type":"userMessage","id":"u","content":[{"type":"text","text":"question"}]}"#,"u","userMessage").as_deref(),Some("question"));
        assert_eq!(projected_text(r#"{"type":"agentMessage","id":"a","phase":"final_answer","text":"answer"}"#,"a","agentMessage").as_deref(),Some("answer"));
        assert!(projected_text(r#"{"type":"agentMessage","id":"a","phase":"commentary","text":"progress"}"#,"a","agentMessage").is_none());
        assert!(projected_text(r#"{"type":"agentMessage","id":"other","text":"wrong"}"#,"a","agentMessage").is_none());
        let ordinary=serde_json::json!({"type":"agentMessage","id":"a","phase":"final_answer","text":"{\"facts\":[]}"}).to_string();
        assert_eq!(projected_text(&ordinary,"a","agentMessage").as_deref(),Some("{\"facts\":[]}"));
        let long=serde_json::json!({"type":"agentMessage","id":"a","text":"长".repeat(20000)}).to_string();
        let cut=projected_text(&long,"a","agentMessage").unwrap();assert!(cut.len()<=16384 && cut.ends_with("[正文已按适配器字节预算截断]"));
    }
}
