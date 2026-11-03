//! 仅来源路径证据与精确Respire输出协议同时成立时标记内部成功。
use agentnotify_core::event::{BackgroundResult, OriginEvidence, SourceClass, SourceEvidence};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};
use std::{fmt, fs, path::PathBuf};

pub struct OriginProof {
    pub codex_home: PathBuf,
    pub plugin_root: Option<PathBuf>,
    pub plugin_data: Option<PathBuf>,
    pub hook_event: String,
    pub registered_adapter: bool,
}
pub struct RuleSet { pub respire_enabled: bool }
impl Default for RuleSet {
    fn default() -> Self { Self { respire_enabled: true } }
}
/// JSON解码时拒绝任意层重复键，防止失败字段被后值覆盖。
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("unique JSON value") }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique,E> { Ok(Unique(Value::Bool(v))) }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique,E> { Ok(Unique(Value::Number(v.into()))) }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique,E> { Ok(Unique(Value::Number(v.into()))) }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique,E> { Number::from_f64(v).map(|n| Unique(Value::Number(n))).ok_or_else(|| E::custom("number")) }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique,E> { Ok(Unique(Value::String(v.to_owned()))) }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique,E> { Ok(Unique(Value::String(v))) }
            fn visit_none<E: de::Error>(self) -> Result<Unique,E> { Ok(Unique(Value::Null)) }
            fn visit_unit<E: de::Error>(self) -> Result<Unique,E> { Ok(Unique(Value::Null)) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique,A::Error> {
                let mut result=Vec::new();
                while let Some(v)=seq.next_element::<Unique>()? { result.push(v.0); }
                Ok(Unique(Value::Array(result)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique,A::Error> {
                let mut result=Map::new();
                while let Some((k,v))=map.next_entry::<String,Unique>()? {
                    if result.contains_key(&k) { return Err(de::Error::custom("duplicate key")); }
                    result.insert(k,v.0);
                }
                Ok(Unique(Value::Object(result)))
            }
        }
        d.deserialize_any(V)
    }
}
fn exact_keys(v: &Value, keys: &[&str]) -> bool {
    v.as_object().is_some_and(|m| m.len()==keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}
fn ecma_trimmed(s: &str) -> bool {
    const SPACE: &[char] = &['\t','\n','\u{000b}','\u{000c}','\r',' ','\u{00a0}','\u{1680}','\u{2000}','\u{2001}','\u{2002}','\u{2003}','\u{2004}','\u{2005}','\u{2006}','\u{2007}','\u{2008}','\u{2009}','\u{200a}','\u{2028}','\u{2029}','\u{202f}','\u{205f}','\u{3000}','\u{feff}'];
    !s.trim_matches(SPACE).is_empty()
}
fn extraction_entry(v: &Value, fact: bool) -> bool {
    if !exact_keys(v, if fact { &["text","sources","status"] } else { &["text","sources"] }) { return false; }
    let Some(text)=v.get("text").and_then(Value::as_str) else { return false };
    if !ecma_trimmed(text) || text.encode_utf16().count()>1500 { return false; }
    let Some(sources)=v.get("sources").and_then(Value::as_array) else { return false };
    if sources.is_empty() || sources.iter().any(|s| !s.as_str().is_some_and(valid_source)) { return false; }
    !fact || v.get("status").and_then(Value::as_str).is_some_and(|s| matches!(s,"confirmed"|"proposed"|"uncertain"))
}
fn valid_source(s: &str) -> bool {
    let Some(rest)=s.strip_prefix("item-") else { return false };
    let Some((item,part))=rest.split_once("-part-") else { return false };
    !item.is_empty() && !part.is_empty() && item.bytes().all(|b| b.is_ascii_digit()) && part.bytes().all(|b| b.is_ascii_digit())
}
fn protocol(raw: &[u8]) -> Option<&'static str> {
    let text=std::str::from_utf8(raw).ok()?;
    let value=serde_json::from_str::<Unique>(text).ok()?.0;
    if exact_keys(&value, &["result","ids"]) {
        let result=value.get("result")?.as_str()?;
        if !matches!(result,"stored"|"already-stored"|"nothing-to-store"|"read-only") { return None; }
        let ids=value.get("ids")?.as_array()?;
        if result=="already-stored" && ids.is_empty() { return None; }
        if ids.iter().all(|v| v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok() && s.len()==36)) {
            return Some("respire_receipt");
        }
    }
    if exact_keys(&value, &["facts","constraints"]) && text.encode_utf16().count()<=6000 {
        let facts=value.get("facts")?.as_array()?;
        let constraints=value.get("constraints")?.as_array()?;
        if facts.len()<=24 && constraints.len()<=24
            && facts.iter().all(|v| extraction_entry(v,true))
            && constraints.iter().all(|v| extraction_entry(v,false)) {
            return Some("respire_extraction");
        }
    }
    None
}
fn verified_respire(p: &OriginProof) -> bool {
    if !p.registered_adapter || p.hook_event!="Stop" { return false; }
    let (Some(root),Some(data))=(&p.plugin_root,&p.plugin_data) else { return false };
    let Ok(home)=fs::canonicalize(&p.codex_home) else { return false };
    let Ok(root)=fs::canonicalize(root) else { return false };
    let Ok(data)=fs::canonicalize(data) else { return false };
    let Ok(expected_parent)=fs::canonicalize(home.join("plugins/cache/rsrsai/respire")) else { return false };
    let Ok(expected_data)=fs::canonicalize(home.join("plugins/data/respire-rsrsai")) else { return false };
    if root.parent()!=Some(expected_parent.as_path()) || data!=expected_data { return false; }
    let Some(version)=root.file_name().and_then(|v| v.to_str()) else { return false };
    let manifest=root.join(".codex-plugin/plugin.json");
    let Ok(bytes)=fs::read(manifest) else { return false };
    if bytes.len()>8192 { return false; }
    let Ok(value)=serde_json::from_slice::<Value>(&bytes) else { return false };
    value.get("name").and_then(Value::as_str)==Some("respire")
        && value.get("version").and_then(Value::as_str)==Some(version)
}
/// 路径证据仅降低误分类；并非防同用户恶意进程的认证机制。
pub fn classify(raw: &[u8], proof: &OriginProof, rules: &RuleSet) -> SourceEvidence {
    let matched=if rules.respire_enabled && verified_respire(proof) { protocol(raw) } else { None };
    SourceEvidence {
        source_class: if matched.is_some() { SourceClass::InternalBackground } else { SourceClass::Unknown },
        classification_reason: matched.unwrap_or("source_or_protocol_unknown").to_owned(),
        origin_evidence: if proof.registered_adapter { OriginEvidence::VerifiedAdapter } else { OriginEvidence::Unknown },
        background_result: if matched.is_some() { BackgroundResult::Success } else { BackgroundResult::Unknown },
    }
}
