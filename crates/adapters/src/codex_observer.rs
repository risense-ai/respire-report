//! 只读 Codex 完成事实适配器。游标与来源仲裁属于适配边界，核心仅接收中立事件。
use crate::{AdapterContext, AdapterError};
use agentnotify_core::{event::*, queue::{DirectorySecurity, reject_links}};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, time::Duration};
use uuid::Uuid;
type Result<T> = std::result::Result<T, AdapterError>;
fn err() -> AdapterError { AdapterError::new("observer_degraded") }
fn sql<T>(v:rusqlite::Result<T>)->Result<T>{v.map_err(|_|err())}
/// 展示标签按中立契约单行化并限制 UTF-8 字节，正文保持原格式。
fn label(text:&str)->String {
    let clean=text.replace(['\r','\n']," ");let mut end=clean.len().min(512);
    while !clean.is_char_boundary(end){end-=1;} clean[..end].to_owned()
}
/// 展示路径键统一Windows扩展前缀、分隔符和大小写；UNC仍保留服务器/共享根。
fn project_path_key(value:&str)->String{
    let v=value.replace('\\',"/");
    let v=if v.get(..8).is_some_and(|p|p.eq_ignore_ascii_case("//?/unc/")){format!("//{}",&v[8..])}else{v.strip_prefix("//?/").unwrap_or(&v).to_owned()};
    let v=v.trim_end_matches('/');if cfg!(windows){v.to_lowercase()}else{v.to_owned()}
}
/// 旧通知器同样按项目根最长前缀补全显示名；仅路径组件边界匹配，不访问其他目录。
fn project_by_root(db:&Connection,cwd:&str)->Option<String>{
    let current=project_path_key(cwd);
    let mut stmt=db.prepare("SELECT p.name,r.path FROM projects p JOIN project_roots r ON r.project_id=p.id LIMIT 10000").ok()?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).ok()?;
    rows.filter_map(|r|r.ok()).filter_map(|(name,path)|{let root=project_path_key(&path);if !root.is_empty()&&(current==root||current.starts_with(&(root.clone()+"/"))){Some((root.len(),name))}else{None}}).max_by_key(|(len,_)|*len).map(|(_,name)|name)
}
/// 仅接受本地项目的精确ID关联；失效归属返回None，由调用方继续原路径回退。
fn assigned_project(value:&serde_json::Value,session:&str)->Option<String>{
    let assignment=value.get("thread-project-assignments")?.get(session)?;
    if assignment.get("projectKind")?.as_str()? != "local"{return None;}
    let id=assignment.get("projectId")?.as_str()?;
    let project=value.get("local-projects")?.get(id)?;
    if project.get("id")?.as_str()? != id{return None;}
    project.get("name")?.as_str().filter(|s|!s.trim().is_empty()).map(str::to_owned)
}
/// 显式会话项目归属优先于路径标签；只读取这三项映射，不将其它全局状态输出。
fn desktop_project(home:&Path,cwd:&str,session:&str)->Option<String>{
    let path=home.join(".codex-global-state.json");if !crate::codex_metadata::local_regular(&path){return None;}
    let file=std::fs::File::open(path).ok()?;if file.metadata().ok()?.len()>8*1024*1024{return None;}
    let value:serde_json::Value=serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    if let Some(name)=assigned_project(&value,session){return Some(name);}
    let labels=value.get("electron-workspace-root-labels")?.as_object()?;
    let current=project_path_key(cwd);
    labels.iter().filter_map(|(path,name)|{let root=project_path_key(path);let name=name.as_str()?;if !root.is_empty()&&!name.trim().is_empty()&&(current==root||current.starts_with(&(root.clone()+"/"))){Some((root.len(),name))}else{None}}).max_by_key(|(len,_)|*len).map(|(_,name)|name.to_owned())
}
const FINAL_REPLY_SQL: &str = "SELECT CAST(i.item_type AS TEXT),CASE WHEN json_valid(i.item_json) THEN CAST(json_extract(i.item_json,'$.type') AS TEXT) END,CASE WHEN json_valid(i.item_json) THEN CAST(json_extract(i.item_json,'$.phase') AS TEXT) END,CASE WHEN json_valid(i.item_json) THEN json_extract(i.item_json,'$.id')=i.item_id END FROM thread_turns t JOIN thread_items i ON i.thread_id=t.thread_id AND i.turn_id=t.turn_id AND i.item_id=t.final_agent_item_id WHERE t.thread_id=?1 AND t.turn_id=?2";
const THREAD_METADATA_SQL: &str = "SELECT t.source,t.thread_source,t.cwd,substr(COALESCE(NULLIF(t.name,''),t.title),1,512),p.name,t.originator FROM threads t LEFT JOIN projects p ON p.id=t.project_id WHERE t.id=?1";
const TURNS_SQL: &str = "SELECT thread_id,turn_id,status,completed_at FROM thread_turns ORDER BY thread_id,turn_id LIMIT 100001";

/// 真实完成必须对应本轮最终回答项；不以普通Stop、问号或工具/阶段回报推定最终回复。
fn final_reply(history:&Connection,session:&str,turn:&str)->Result<bool>{
    let row:Option<(Option<String>,Option<String>,Option<String>,Option<bool>)>=sql(history.query_row(FINAL_REPLY_SQL,params![session,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional())?;
    let Some((kind,inner,phase,matched))=row else{return Err(AdapterError::new("observer_final_reply_pending"));};
    // 已知的阶段回报继续跳过。未知结构保留待处理轮次，避免未来版本静默丢失通知。
    if kind.as_deref() == Some("agentMessage") && inner.as_deref() == Some("agentMessage") && matched == Some(true) {
        match phase.as_deref() {
            Some("final_answer") => return Ok(true),
            Some("commentary") => return Ok(false),
            _ => {}
        }
    }
    Err(AdapterError::new("observer_reply_contract_changed"))
}
fn state_path(root:&Path,id:Uuid)->PathBuf { root.join(format!("codex-observer-{id}.sqlite")) }
fn ledger(root:&Path,id:Uuid,security:&dyn DirectorySecurity,flags:OpenFlags)->Result<Connection>{
    security.verify(root).map_err(|_|err())?;
    let path=state_path(root,id);
    for suffix in ["", "-journal", "-wal", "-shm"] { reject_links(&PathBuf::from(format!("{}{suffix}",path.display()))).map_err(|_|err())?; }
    let db=sql(Connection::open_with_flags(path,flags))?;
    sql(db.busy_timeout(Duration::from_millis(50)))?;
    Ok(db)
}
/// 初始化与完成 Hook 的共同互斥；调用者必须持有至持久化 ACK，竞争明确报可重试。
pub fn completion_guard(root:&Path,id:Uuid,security:&dyn DirectorySecurity)->Result<std::fs::File>{
    security.verify(root).map_err(|_|err())?;
    let path=root.join(format!("codex-completion-{id}.lock"));reject_links(&path).map_err(|_|err())?;
    let file=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|_|err())?;
    file.try_lock().map_err(|_|AdapterError::new("observer_switch_busy_retry"))?;
    Ok(file)
}
/// 已提交 enabled 是唯一完成来源所有权；损坏不能静默退回 Hook。
pub fn owns(root:&Path,id:Uuid,security:&dyn DirectorySecurity)->Result<bool>{
    if !state_path(root,id).exists(){return Ok(false);}
    let db=ledger(root,id,security,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists:bool=sql(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",[],|r|r.get(0)))?;
    if !exists{return Ok(false);}
    Ok(sql(db.query_row("SELECT value FROM meta WHERE key='enabled'",[],|r|r.get::<_,String>(0)).optional())?==Some("1".into()))
}
/// 原生 Hook 完成仅由本适配边界辨识；生命周期事件不在仲裁范围。
pub fn completion_hook(raw:&[u8])->bool{
    serde_json::from_slice::<serde_json::Value>(raw).ok().and_then(|v|v.get("hook_event_name").and_then(|v|v.as_str()).map(|s|matches!(s,"Stop"|"SubagentStop"|"Interrupt"))).unwrap_or(false)
}
fn source(home:&Path,name:&str)->Result<Connection>{
    let path=home.join(name);
    if !crate::codex_metadata::local_regular(&path){return Err(err());}
    let db=sql(Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY|OpenFlags::SQLITE_OPEN_NO_MUTEX))?;
    sql(db.busy_timeout(Duration::from_millis(50)))?;
    Ok(db)
}
fn verified(context:&AdapterContext)->Result<&Path>{
    context.require_binding("codex")?;
    context.codex_home.as_deref().ok_or_else(err)
}
#[derive(Clone)]
struct Turn { session:String, turn:String, status:String, completed:Option<i64> }
/// 明确列及状态构成兼容契约；数量上限失败可见，不截断后推进水位。
fn turns(db:&Connection)->Result<Vec<Turn>>{
    let mut stmt=sql(db.prepare(TURNS_SQL))?;
    let rows=sql(stmt.query_map([],|r|Ok(Turn{session:r.get(0)?,turn:r.get(1)?,status:r.get(2)?,completed:r.get(3)?})))?;
    let rows=sql(rows.collect::<rusqlite::Result<Vec<_>>>())?;
    if rows.len()>100000 || rows.iter().any(|r|!matches!(r.status.as_str(),"completed"|"failed"|"interrupted"|"inProgress")){return Err(err());}
    Ok(rows)
}
fn fingerprint(context:&AdapterContext)->Result<String>{
    let home=verified(context)?.canonicalize().map_err(|_|err())?;
    Ok(format!("{}|{}|{}",context.instance_id,context.version,home.display()).to_lowercase())
}
/// 版本是既有 Hook 登记信息。观察游标只绑定实例与规范化数据目录。
fn check_identity(db: &Connection, id: Uuid, home: &Path) -> Result<()> {
    let stored: String = sql(db.query_row("SELECT value FROM meta WHERE key='binding'", [], |row| row.get(0)))?;
    let canonical_home = home.canonicalize().map_err(|_| err())?.display().to_string().to_lowercase();
    let mut fields = stored.splitn(3, '|');
    let stored_id = fields.next().and_then(|value| Uuid::parse_str(value).ok());
    let registered_version = fields.next();
    if stored_id != Some(id) || registered_version.is_none_or(str::is_empty)
        || fields.next() != Some(canonical_home.as_str())
    {
        return Err(AdapterError::new("observer_binding_changed"));
    }
    Ok(())
}

/// 校验监听实际使用的查询。只准备语句，探测不读取问答正文，也不推进游标。
fn observer_sources(home: &Path) -> Result<(Connection, Connection)> {
    let history = source(home, "thread_history_1.sqlite")?;
    let state = source(home, "state_5.sqlite")?;
    for (db, query) in [(&history, TURNS_SQL), (&history, FINAL_REPLY_SQL), (&state, THREAD_METADATA_SQL)] {
        db.prepare(query).map_err(|error| match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => err(),
            _ => AdapterError::new("observer_schema_unsupported"),
        })?;
    }
    Ok((history, state))
}

/// 设置页与后台复用实例和数据契约检查。检查既有绑定时不重建游标。
pub fn validate_binding(root: &Path, id: Uuid, home: &Path, security: &dyn DirectorySecurity) -> Result<()> {
    let db = ledger(root, id, security, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check_identity(&db, id, home)?;
    observer_sources(home)?;
    Ok(())
}

/// 源事务快照内建立历史 terminal 基线和活动列表，最后一次提交同时开启仲裁。
pub fn enable(root:&Path,context:&AdapterContext,security:&dyn DirectorySecurity)->Result<()> {
    let home=verified(context)?;
    // 同一工具根内，一个 CODEX_HOME 只能有一个完成源；全局启用锁防并发双登记。
    let lock_path=root.join("codex-observer-enable.lock");
    security.verify(root).map_err(|_|err())?;reject_links(&lock_path).map_err(|_|err())?;
    let enable_lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_|err())?;
    enable_lock.try_lock().map_err(|_|AdapterError::new("observer_switch_busy_retry"))?;
    let canonical_home=home.canonicalize().map_err(|_|err())?.display().to_string().to_lowercase();
    for entry in std::fs::read_dir(root).map_err(|_|err())? {
        let entry=entry.map_err(|_|err())?;let name=entry.file_name();let Some(name)=name.to_str()else{continue;};
        let Some(id)=name.strip_prefix("codex-observer-").and_then(|n|n.strip_suffix(".sqlite")).and_then(|n|Uuid::parse_str(n).ok())else{continue;};
        if owns(root,id,security)? {
            let other=ledger(root,id,security,OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let binding:String=sql(other.query_row("SELECT value FROM meta WHERE key='binding'",[],|r|r.get(0)))?;
            if binding.splitn(3,'|').nth(2)==Some(canonical_home.as_str()){return Err(AdapterError::new("observer_home_already_owned"));}
        }
    }
    let (mut history, _) = observer_sources(home)?;
    let baseline_start=Utc::now().timestamp();
    let snapshot=sql(history.transaction())?;
    let rows=turns(&snapshot)?;
    let mut db=ledger(root,context.instance_id,security,OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)?;
    let tx=sql(db.transaction())?;
    sql(tx.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS turns(session TEXT NOT NULL,turn TEXT NOT NULL,state TEXT NOT NULL,observed INTEGER,presence INTEGER,PRIMARY KEY(session,turn));"))?;
    let initialized:Option<String>=sql(tx.query_row("SELECT value FROM meta WHERE key='enabled'",[],|r|r.get(0)).optional())?;
    if initialized.is_some(){return Err(AdapterError::new("observer_already_enabled"));}
    for r in rows {sql(tx.execute("INSERT INTO turns(session,turn,state) VALUES(?1,?2,?3)",params![r.session,r.turn,if r.status=="inProgress"{"active"}else{"seen"}]))?;}
    sql(tx.execute("INSERT INTO meta VALUES('binding',?1)",[fingerprint(context)?]))?;
    sql(tx.execute("INSERT INTO meta VALUES('enabled_at',?1)",[baseline_start.to_string()]))?;
    sql(tx.execute("INSERT INTO meta VALUES('enabled','1')",[]))?;
    sql(tx.commit())?;
    Ok(())
}
/// 仅提供中立通知和稳定接收标识；ACK 后由调用者确认，不把正文写入游标库。
pub struct ObservedBatch {pub events:Vec<Observed>,pub incomplete:bool,pub reply_contract_changed:bool}
pub struct Observed { pub id:Uuid,pub output:AdapterOutput,pub presence_suppressed:bool, session:String,turn:String }
/// 已启用观察实例的运行参数。来源身份由持久游标绑定和只读查询契约验证。
pub struct ObserverContext<'a> {
    /// 本地登记实例 ID，与游标记录一致才继续。
    pub instance_id: Uuid,
    /// 登记的 Codex 数据目录，必须与游标绑定的规范路径一致。
    pub home: &'a Path,
    /// 本机是否可以安全保护待发正文。
    pub content_key_available: bool,
    /// 用户是否允许采集本轮提问。
    pub include_question: bool,
}

/// 按所有已见主键而非秒级时间排序扫描，迟到同秒/先启动后结束均不会遗漏。
pub fn poll(root:&Path,context:&ObserverContext<'_>,security:&dyn DirectorySecurity,presence:bool,include_answer:bool)->Result<ObservedBatch>{
    let home=context.home;
    let db = ledger(root, context.instance_id, security, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    check_identity(&db, context.instance_id, home)?;
    let enabled_at:String=sql(db.query_row("SELECT value FROM meta WHERE key='enabled_at'",[],|r|r.get(0)))?;
    let enabled_at:i64=enabled_at.parse().map_err(|_|err())?;
    let (history, state) = observer_sources(home)?;
    // 一次读取游标索引，避免每两秒对所有历史行反复 prepare/query（控制线程之外亦需有界）。
    let mut seen_stmt=sql(db.prepare("SELECT session,turn,state,observed,presence FROM turns"))?;
    let seen_rows=sql(seen_stmt.query_map([],|r|Ok(((r.get::<_,String>(0)?,r.get::<_,String>(1)?),(r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,Option<bool>>(4)?)))))?;
    let prior_rows:std::collections::HashMap<_,_>=sql(seen_rows.collect::<rusqlite::Result<_>>())?;
    let mut result=Vec::new();
    let mut incomplete=false;
    let mut reply_contract_changed=false;
    let deadline=std::time::Instant::now()+Duration::from_secs(3);
    for turn in turns(&history)? {
        if std::time::Instant::now()>=deadline {return if result.is_empty(){Err(AdapterError::new("observer_scan_pending"))}else{Ok(ObservedBatch{events:result,incomplete:true,reply_contract_changed})}; }
        if turn.status=="inProgress"{continue;}
        let prior=prior_rows.get(&(turn.session.clone(),turn.turn.clone())).cloned();
        if prior.as_ref().is_some_and(|p|p.0=="seen"){continue;}
        // 启用后导入的旧 terminal 不补发；初始化时进行中的键不受此限制。同秒仍按键处理。
        let completed=turn.completed.ok_or_else(err)?;
        if prior.is_none() && completed<enabled_at {
            sql(db.execute("INSERT INTO turns(session,turn,state) VALUES(?1,?2,'seen')",params![turn.session,turn.turn]))?;continue;
        }
        // 主表只读固定元数据列；创建线程时的 cli_version 不用于筛选续轮。
        let meta:Option<(String,Option<String>,String,String,Option<String>,Option<String>)>=sql(state.query_row(THREAD_METADATA_SQL,[&turn.session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional())?;
        let Some((origin,thread_source,cwd,title,project,originator))=meta else{return Err(err());};
        let origin_json:serde_json::Value=serde_json::from_str(&origin).unwrap_or_default();
        let guardian=thread_source.as_deref()==Some("guardian_review") || origin_json.pointer("/subagent/other").and_then(|v|v.as_str())==Some("guardian");
        // 官方 Respire archive.js 的 initialize.clientInfo.name；精确来源证明，不检查正文/角色名中的词。
        let respire_worker=originator.as_deref()==Some("respire-memory-worker");
        if guardian || respire_worker { sql(db.execute("INSERT INTO turns(session,turn,state) VALUES(?1,?2,'seen') ON CONFLICT(session,turn) DO UPDATE SET state='seen'",params![turn.session,turn.turn]))?; continue; }
        let observed=prior.as_ref().and_then(|p|p.1).unwrap_or_else(||Utc::now().timestamp_millis());
        let suppressed=prior.as_ref().and_then(|p|p.2).unwrap_or(presence);
        sql(db.execute("INSERT INTO turns(session,turn,state,observed,presence) VALUES(?1,?2,'pending',?3,?4) ON CONFLICT(session,turn) DO UPDATE SET state='pending',observed=excluded.observed,presence=excluded.presence",params![turn.session,turn.turn,observed,suppressed]))?;
        let subagent=origin_json.get("subagent").is_some() || thread_source.as_deref()==Some("subagent");
        // 子代理由统一策略notify_subagents门控；主会话仅最终回答结束进入正常通知。
        if !subagent {
            let final_answer=if turn.status!="completed"{false}else{match final_reply(&history,&turn.session,&turn.turn){
                Ok(value)=>value,
                Err(e) if e.code=="observer_final_reply_pending"=>{incomplete=true;continue;},
                Err(e) if e.code=="observer_reply_contract_changed"=>{reply_contract_changed=true;continue;},
                Err(e)=>return Err(e),
            }};
            if !final_answer {sql(db.execute("UPDATE turns SET state='seen' WHERE session=?1 AND turn=?2",params![turn.session,turn.turn]))?;continue;}
        }
        let kind=match turn.status.as_str(){"failed"=>EventKind::ExecutionFailed,"interrupted"=>EventKind::Interrupted,_=>EventKind::ResponseFinished};
        let key=format!("codex-observer-v1|{}|{}|{}",context.instance_id,turn.session,turn.turn);
        let digest=Sha256::digest(key.as_bytes()); let mut bytes=[0u8;16];bytes.copy_from_slice(&digest[..16]);bytes[6]=(bytes[6]&15)|128;bytes[8]=(bytes[8]&63)|128;let id=Uuid::from_bytes(bytes);
        let content=crate::codex_metadata::turn_content(home,&turn.session,&turn.turn,context.include_question && context.content_key_available && !suppressed,include_answer && context.content_key_available && !suppressed);
        // 附件/客户端封套不能成为会话标题；没有真实标题时明确缺失。
        let title=if title.trim().is_empty() || title.contains("# Files mentioned by the user:") || title.contains("<environment_context>"){None}else{Some(label(&title))};
        let output=AdapterOutput {lifecycle:None,source_evidence:SourceEvidence{source_class:if subagent{SourceClass::Subagent}else{SourceClass::Unknown},classification_reason:"codex_local_completion".into(),origin_evidence:OriginEvidence::VerifiedAdapter,background_result:BackgroundResult::Unknown},notification:Some(NormalizedEventV1{
            schema_version:1,adapter_id:"codex".into(),instance_id:context.instance_id,event_id:Some(id.to_string()),session_id:Some(turn.session.clone()),turn_id:Some(turn.turn.clone()),parent_session_id:None,
            agent_kind:if subagent{AgentKind::Subagent}else{AgentKind::Main},actor_id:if subagent{Some(turn.session.clone())}else{None},actor_scope:if subagent{ActorScope::Identified}else{ActorScope::Main},kind,completion_basis:if kind==EventKind::ResponseFinished{CompletionBasis::ResponseStop}else{CompletionBasis::None},
            source_time:turn.completed.and_then(|t|DateTime::from_timestamp(t,0)),received_at:DateTime::from_timestamp_millis(observed).ok_or_else(err)?,project_label:desktop_project(home,&cwd,&turn.session).or(project).filter(|p|!p.trim().is_empty()).or_else(||project_by_root(&state,&cwd)).map(|p|label(&p)).or_else(||Path::new(&cwd).file_name().and_then(|s|s.to_str()).map(label)),session_label:title,question:content.question,answer:content.answer,background_state:BackgroundState::Unknown,resume_url:None,watchdog_detail:None,content_key_available:context.content_key_available,
        })};
        output.validate()?;
        result.push(Observed{id,output,presence_suppressed:suppressed,session:turn.session,turn:turn.turn});
        if result.len()==32{break;}
    }
    Ok(ObservedBatch{events:result,incomplete,reply_contract_changed})
}
/// 只在 core 返回持久化 ACK（包括同 ID 重放 ACK）后调用。
pub fn acknowledge(root:&Path,id:Uuid,security:&dyn DirectorySecurity,item:&Observed)->Result<()> {
    let db=ledger(root,id,security,OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    sql(db.execute("UPDATE turns SET state='seen' WHERE session=?1 AND turn=?2 AND state='pending'",params![item.session,item.turn]))?;
    Ok(())
}

#[cfg(test)]
mod checks {
    use super::*;
    struct Isolated;
    impl DirectorySecurity for Isolated {fn verify(&self,_:&Path)->std::result::Result<(),agentnotify_core::contracts::QueueError>{Ok(())}}
    #[test]
    fn explicit_project_assignment_and_path_fallback(){
        let temp=tempfile::tempdir().unwrap();
        let value=serde_json::json!({"thread-project-assignments":{"chosen":{"projectKind":"local","projectId":"p1"},"stale":{"projectKind":"local","projectId":"missing"}},"local-projects":{"p1":{"id":"p1","name":"Explicit project"}},"electron-workspace-root-labels":{"D:/work/root":"Path alias"}});
        std::fs::write(temp.path().join(".codex-global-state.json"),serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(desktop_project(temp.path(),r"\\?\D:\work\root","chosen").as_deref(),Some("Explicit project"));
        assert_eq!(desktop_project(temp.path(),r"\\?\D:\work\root","stale").as_deref(),Some("Path alias"));
        assert_eq!(desktop_project(temp.path(),"D:/work/root-other","stale"),None);
        assert_eq!(project_path_key(r"\\?\UNC\server\share"),project_path_key(r"\\server\share"));
        assert_ne!(project_path_key(r"\\server\share"),project_path_key("server/share"));
    }
    #[test]
    fn baseline_same_second_ack_and_restart(){
        // 只用隔离临时库验证观察边界；不启动 daemon/SMTP，不读真实 Codex 数据。
        assert_eq!(label("a\r\nb"),"a  b");assert!(label(&"长".repeat(512)).len()<=512);
        let temp=tempfile::tempdir().unwrap();let root=temp.path();
        let history=Connection::open(root.join("thread_history_1.sqlite")).unwrap();
        history.execute_batch("CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,status TEXT,completed_at INTEGER); CREATE TABLE thread_items(thread_id TEXT,turn_id TEXT,item_id TEXT,item_type TEXT,item_json TEXT);").unwrap();
        let state=Connection::open(root.join("state_5.sqlite")).unwrap();
        state.execute_batch("CREATE TABLE threads(id TEXT,source TEXT,thread_source TEXT,cwd TEXT,title TEXT,rollout_path TEXT,name TEXT,project_id TEXT,originator TEXT,agent_nickname TEXT); CREATE TABLE projects(id TEXT,name TEXT); CREATE TABLE project_roots(project_id TEXT,path TEXT); INSERT INTO threads VALUES('session','vscode','user','/project','title','/absent',NULL,NULL,NULL,NULL);").unwrap();
        let now=Utc::now().timestamp();
        history.execute("INSERT INTO thread_turns VALUES('session','old','completed',?1)",[now]).unwrap();
        history.execute("INSERT INTO thread_turns VALUES('session','active','inProgress',NULL)",[]).unwrap();
        history.execute("ALTER TABLE thread_turns ADD COLUMN final_agent_item_id TEXT", []).unwrap();
        let context=AdapterContext{instance_id:Uuid::from_bytes([5;16]),adapter_id:"codex".into(),version:"0.166.0-alpha.5.1".into(),config_path:root.join("hooks.json"),executable_path:root.join("codex.exe"),registered_command:"hook".into(),codex_home:Some(root.into()),received_at:Utc::now(),content_key_available:false,include_question:false,supported_version:true};
        enable(root,&context,&Isolated).unwrap();assert!(owns(root,context.instance_id,&Isolated).unwrap());
        let mut second=context.clone();second.instance_id=Uuid::from_bytes([6;16]);assert_eq!(enable(root,&second,&Isolated).unwrap_err().code,"observer_home_already_owned");
        let observing = ObserverContext {
            instance_id: context.instance_id,
            home: root,
            content_key_available: false,
            include_question: false,
        };
        assert!(poll(root,&observing,&Isolated,false,false).unwrap().events.is_empty());
        // Use the recorded activation second so crossing a wall-clock second cannot make fixture turns historical.
        let cursor = ledger(root, context.instance_id, &Isolated, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let activated: String = cursor.query_row("SELECT value FROM meta WHERE key='enabled_at'", [], |row| row.get(0)).unwrap();
        let now: i64 = activated.parse().unwrap();
        history.execute("UPDATE thread_turns SET status='completed',completed_at=?1 WHERE turn_id='active'",[now]).unwrap();
        history.execute("INSERT INTO thread_turns VALUES('session','same-second','completed',?1,NULL)",[now]).unwrap();
        state.execute("INSERT INTO threads VALUES('worker','exec',NULL,'/project','worker','/absent',NULL,NULL,'respire-memory-worker',NULL)",[]).unwrap();
        state.execute("INSERT INTO threads VALUES('normal','{\"subagent\":{\"thread_spawn\":{\"agent_role\":\"respire_archive_review\"}}}','subagent','/project','normal','/absent',NULL,NULL,'codex_work_desktop',NULL)",[]).unwrap();
        for session in ["worker","normal"] {history.execute("INSERT INTO thread_turns VALUES(?1,'done','completed',?2,NULL)",rusqlite::params![session,now]).unwrap();}
        for turn in ["active","same-second"] {
            history.execute("UPDATE thread_turns SET final_agent_item_id=?1 WHERE thread_id='session' AND turn_id=?1",[turn]).unwrap();
            let raw=serde_json::json!({"id":turn,"type":"agentMessage","phase":"final_answer","text":"fixture"}).to_string();
            history.execute("INSERT INTO thread_items VALUES('session',?1,?1,'agentMessage',?2)",params![turn,raw]).unwrap();
        }
        history.execute("INSERT INTO thread_turns VALUES('session','unready','completed',?1,NULL)",[now]).unwrap();
        // 已启用观察通道不依赖旧的 Hook 文件或客户端程序路径。
        assert!(!context.config_path.exists() && !context.executable_path.exists());
        validate_binding(root, observing.instance_id, root, &Isolated).unwrap();
        let first_batch=poll(root,&observing,&Isolated,true,false).unwrap();assert!(first_batch.incomplete);
        let first=first_batch.events;assert_eq!(first.len(),3);
        assert!(first.iter().any(|e|e.output.notification.as_ref().unwrap().agent_kind==AgentKind::Subagent));
        assert!(first.iter().all(|e|e.session!="worker"));
        // 未 ACK 即重启/重读保留同 ID 与首次 presence 决定，不因之后活跃状态变化改写。
        let replay=poll(root,&observing,&Isolated,false,false).unwrap().events;assert_eq!(first[0].id,replay[0].id);assert!(replay.iter().all(|e|e.presence_suppressed));
        for event in &replay {acknowledge(root,context.instance_id,&Isolated,event).unwrap();}
        let waiting=poll(root,&observing,&Isolated,false,false).unwrap();assert!(waiting.events.is_empty() && waiting.incomplete);
        history.execute("UPDATE thread_turns SET final_agent_item_id='later' WHERE turn_id='unready'",[]).unwrap();
        history.execute("INSERT INTO thread_items VALUES('session','unready','later','agentMessage','{\"id\":\"later\",\"type\":\"agentMessage\",\"phase\":\"final_answer\"}')",[]).unwrap();
        let ready=poll(root,&observing,&Isolated,false,false).unwrap();assert!(!ready.incomplete);assert_eq!(ready.events.len(),1);
        acknowledge(root,context.instance_id,&Isolated,&ready.events[0]).unwrap();
        assert!(poll(root,&observing,&Isolated,false,false).unwrap().events.is_empty());
        std::fs::create_dir(root.join("new-home")).unwrap();
        assert_eq!(
            validate_binding(root, observing.instance_id, &root.join("new-home"), &Isolated).unwrap_err().code,
            "observer_binding_changed"
        );
        history.execute("INSERT INTO thread_turns VALUES('session','future','completed',?1,'future')", [now]).unwrap();
        history.execute("INSERT INTO thread_items VALUES('session','future','future','agentMessage','{\"type\":\"agentMessage\",\"phase\":\"future\"}')", []).unwrap();
        let changed = poll(root, &observing, &Isolated, false, false).unwrap();
        assert!(changed.events.is_empty() && changed.reply_contract_changed);
        history.execute("UPDATE thread_items SET item_json=json_set(item_json,'$.phase','final_answer','$.id','future') WHERE item_id='future'", []).unwrap();
        assert_eq!(poll(root, &observing, &Isolated, false, false).unwrap().events.len(), 1);
        history.execute("DROP TABLE thread_items", []).unwrap();
        assert_eq!(
            poll(root, &observing, &Isolated, false, false).err().unwrap().code,
            "observer_schema_unsupported"
        );
    }
}
