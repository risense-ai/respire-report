//! 固定44字节头 + <=8KiB受控元数据JSON + 原始AEAD密文；不做base64。
use super::*;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::ReadDir,
    io::Read,
    time::Instant,
};
pub const MAX_ITEM_BYTES: usize = 64 * 1024;
pub const MAX_SPOOL_BYTES: usize = 16 * 1024 * 1024;
const HEADER: usize = 44;
#[derive(Clone)]
/// nonce/tag固定长度，BodyProtector必须为每次seal生成唯一nonce并认证metadata。
pub struct SealedBody {
    pub nonce: [u8; 12],
    pub tag: [u8; 16],
    pub ciphertext: Vec<u8>,
}
/// 扫描游标跨调用推进；完整扫描前不应用候选，避免未扫描的更早序号被越过。
/// BTreeMap在每个扫描步骤增量排序，预算结束时无需额外执行全量sort。
#[derive(Default)]
pub(crate) struct ImportScan {
    initialized: bool,
    complete: bool,
    cursor: Option<ReadDir>,
    retry_path: Option<PathBuf>,
    pending: BTreeMap<(uuid::Uuid, bool, u64, uuid::Uuid), PathBuf>,
    malformed: VecDeque<PathBuf>,
}
impl ImportScan {
    fn remaining(&self) -> usize {
        self.pending.len()
            + self.malformed.len()
            + usize::from(!self.complete || self.retry_path.is_some())
    }
    fn insert(&mut self, path: PathBuf, envelope: &IngressEnvelopeV1) {
        // Unknown排在同实例已知项前，仅用于先传播降级；这不表示推断了真实发生顺序。
        self.pending.insert(
            (
                instance(envelope),
                envelope.per_instance_sequence.is_some(),
                envelope.per_instance_sequence.unwrap_or(0),
                envelope.ingress_id,
            ),
            path,
        );
    }
}
impl SqliteQueue {
    pub(crate) fn encode(&self, e: &IngressEnvelopeV1) -> Result<Vec<u8>, QueueError> {
        // 1. 从metadata移除两个正文，二进制明文使用长度前缀避免JSON转义膨胀。
        let mut clean = e.clone();
        let mut body = zeroize::Zeroizing::new(Vec::new());
        let mut present = false;
        if let Some(n) = &mut clean.adapter_output.notification {
            for field in [&mut n.question, &mut n.answer] {
                match field.take() {
                    Some(s) => {
                        present = true;
                        body.extend_from_slice(&(s.len() as u32).to_le_bytes());
                        body.extend_from_slice(s.as_bytes());
                    }
                    None => body.extend_from_slice(&u32::MAX.to_le_bytes()),
                }
            }
        }
        let metadata = storage(serde_json::to_vec(&clean))?;
        if metadata.len() > 8192 {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let len = if present { body.len() } else { 0 };
        let mut header = vec![0; HEADER];
        header[..4].copy_from_slice(b"ANQ1");
        header[4..6].copy_from_slice(&1u16.to_le_bytes());
        header[6..8].copy_from_slice(&(present as u16).to_le_bytes());
        header[8..12].copy_from_slice(&(metadata.len() as u32).to_le_bytes());
        header[12..16].copy_from_slice(&(len as u32).to_le_bytes());
        let mut aad = header[..16].to_vec();
        aad.extend_from_slice(&metadata);
        // 2. 缺保护器或不满足固定AEAD封装长度时直接拒绝，绝不保存body明文。
        let ciphertext = if present {
            let sealed = self
                .deps
                .protector
                .as_ref()
                .ok_or_else(|| error(QueueErrorCode::BodyProtectionUnavailable))?
                .seal(&body, &aad)?;
            if sealed.ciphertext.len() != len {
                return Err(error(QueueErrorCode::StorageFailed));
            }
            header[16..28].copy_from_slice(&sealed.nonce);
            header[28..44].copy_from_slice(&sealed.tag);
            sealed.ciphertext
        } else {
            Vec::new()
        };
        header.extend_from_slice(&metadata);
        header.extend_from_slice(&ciphertext);
        if header.len() > MAX_ITEM_BYTES {
            return Err(error(QueueErrorCode::SpoolFull));
        }
        Ok(header)
    }
    pub(crate) fn decode(&self, bytes: &[u8]) -> Result<IngressEnvelopeV1, QueueError> {
        // 1. 验证头、版本、标志及所有长度后才解析JSON/AEAD。
        if bytes.len() < HEADER || bytes.len() > MAX_ITEM_BYTES || &bytes[..4] != b"ANQ1" {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        if u16::from_le_bytes(bytes[4..6].try_into().unwrap()) != 1 {
            return Err(error(QueueErrorCode::UnsupportedSchema));
        }
        let flags = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        let ml = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let bl = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        if flags > 1
            || ml > 8192
            || HEADER + ml + bl != bytes.len()
            || (flags == 0 && bl != 0)
            || (flags == 1 && bl < 8)
        {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        let metadata = &bytes[HEADER..HEADER + ml];
        let mut e: IngressEnvelopeV1 = storage(serde_json::from_slice(metadata))?;
        // metadata不能夹带明文，损坏文件不会绕过privacy路径导入。
        if e.adapter_output
            .notification
            .as_ref()
            .is_some_and(|n| n.question.is_some() || n.answer.is_some())
        {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        e.validate()
            .map_err(|_| error(QueueErrorCode::InvalidInput))?;
        // 2. 正文仅经AEAD验证后恢复为瞬态输入；开关关闭后导入也不能重新保留旧正文。
        if flags == 1 {
            let mut aad = bytes[..16].to_vec();
            aad.extend_from_slice(metadata);
            let sealed = SealedBody {
                nonce: bytes[16..28].try_into().unwrap(),
                tag: bytes[28..44].try_into().unwrap(),
                ciphertext: bytes[HEADER + ml..].to_vec(),
            };
            let plain = zeroize::Zeroizing::new(
                self.deps
                    .protector
                    .as_ref()
                    .ok_or_else(|| error(QueueErrorCode::BodyProtectionUnavailable))?
                    .open(&sealed, &aad)?,
            );
            let mut at = 0;
            let q = read_field(&plain, &mut at)?;
            let a = read_field(&plain, &mut at)?;
            if at != plain.len() {
                return Err(error(QueueErrorCode::InvalidInput));
            }
            let n = e
                .adapter_output
                .notification
                .as_mut()
                .ok_or_else(|| error(QueueErrorCode::InvalidInput))?;
            n.question = if self.config.include_question {
                q
            } else {
                None
            };
            n.answer = if self.config.include_answer { a } else { None };
            n.content_key_available = true;
        }
        Ok(e)
    }
    /// 仅显式受控借用者读取正文；读取前检查路径、大小及AEAD，不输出到诊断。
    pub fn inspect_spool(&self, path: &Path) -> Result<IngressEnvelopeV1, QueueError> {
        self.deps.security.verify(&self.root)?;
        if path.parent() != Some(self.spool_path().as_path()) {
            return Err(error(QueueErrorCode::InvalidInput));
        }
        reject_links(path)?;
        let file = storage(std::fs::File::open(path))?;
        if storage(file.metadata())?.len() > MAX_ITEM_BYTES as u64 {
            return Err(error(QueueErrorCode::SpoolFull));
        }
        let mut bytes = Vec::new();
        storage(
            file.take((MAX_ITEM_BYTES + 1) as u64)
                .read_to_end(&mut bytes),
        )?;
        self.decode(&bytes)
    }
    fn entries(&self) -> Result<Vec<PathBuf>, QueueError> {
        let dir = self.spool_path();
        reject_links(&dir)?;
        let mut entries = Vec::new();
        for entry in storage(std::fs::read_dir(dir))? {
            let path = storage(entry)?.path();
            reject_links(&path)?;
            if !storage(std::fs::symlink_metadata(&path))?.is_file() {
                return Err(error(QueueErrorCode::StorageUnavailable));
            }
            entries.push(path);
        }
        Ok(entries)
    }
    pub fn pending_spool(&self) -> Result<usize, QueueError> {
        Ok(self
            .entries()?
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "spool"))
            .count())
    }
    pub(crate) fn has_backlog(&self, instance: uuid::Uuid) -> Result<bool, QueueError> {
        for p in self.entries()? {
            if p.extension().is_some_and(|e| e == "spool") {
                match self.inspect_spool(&p) {
                    Ok(e) if super::instance(&e) != instance => {}
                    _ => return Ok(true),
                }
            }
        }
        Ok(false)
    }
    pub(crate) fn write_spool(&self, e: &IngressEnvelopeV1) -> Result<EnqueueOutcome, QueueError> {
        let bytes = self.encode(e)?;
        let mut total = 0u64;
        for p in self.entries()? {
            total = total
                .checked_add(storage(std::fs::metadata(p))?.len())
                .ok_or_else(|| error(QueueErrorCode::SpoolFull))?;
        }
        // 隔离文件与崩溃临时文件也占配额，不能靠坏文件绕过总量上限。
        if total + bytes.len() as u64 > MAX_SPOOL_BYTES as u64 {
            return Err(error(QueueErrorCode::SpoolFull));
        }
        let path = self.spool_path().join(format!("{}.spool", e.ingress_id));
        super::ingress::atomic_write(&path, &bytes, |p| self.deps.faults.check(p), Some(false))?;
        Ok(EnqueueOutcome::Spooled { id: e.ingress_id })
    }
    pub fn import_spool(&self) -> Result<ImportReport, QueueError> {
        // 1. 整轮只有一个deadline；互斥竞争、目录扫描、AEAD和SQLite都消费同一预算。
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut report = ImportReport::default();
        let mut scan = match self.import_scan.try_lock() {
            Ok(scan) => scan,
            Err(std::sync::TryLockError::WouldBlock) => {
                report.remaining = 1;
                return Ok(report);
            }
            Err(_) => return Err(error(QueueErrorCode::StorageFailed)),
        };
        if !scan.initialized {
            self.deps.security.verify(&self.root)?;
            reject_links(&self.spool_path())?;
            if check_deadline(Some(deadline)).is_err() {
                report.remaining = 1;
                return Ok(report);
            }
            scan.cursor = Some(storage(std::fs::read_dir(self.spool_path()))?);
            scan.initialized = true;
        }
        // 2. 游标跨轮保存，不从头重扫。未完成快照之前不执行任何生命周期参与者。
        while !scan.complete {
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            let path = if let Some(path) = scan.retry_path.take() {
                path
            } else {
                match scan
                    .cursor
                    .as_mut()
                    .expect("initialized scan cursor")
                    .next()
                {
                    Some(entry) => storage(entry)?.path(),
                    None => {
                        scan.cursor = None;
                        scan.complete = true;
                        break;
                    }
                }
            };
            if !path.extension().is_some_and(|e| e == "spool") {
                continue;
            }
            match self.inspect_spool(&path) {
                Ok(e) => scan.insert(path, &e),
                Err(e) if e.code == QueueErrorCode::BodyProtectionUnavailable => {
                    scan.retry_path = Some(path);
                    return Err(e);
                }
                Err(_) => scan.malformed.push_back(path),
            }
        }
        // 3. 完整快照按实例/保守顺序处理，所有锁等待只使用整轮剩余时间。
        while scan.complete && check_deadline(Some(deadline)).is_ok() {
            let malformed = scan.malformed.front().cloned();
            let candidate = scan
                .pending
                .first_key_value()
                .map(|(key, path)| (*key, path.clone()));
            let path = match (&malformed, &candidate) {
                (Some(path), _) => path.clone(),
                (_, Some((_, path))) => path.clone(),
                _ => break,
            };
            let _guard = match self.gate.enter_until(deadline) {
                Ok(guard) => guard,
                Err(e) if e.code == QueueErrorCode::AdmissionTimeout => break,
                Err(e) => return Err(e),
            };
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            if self.maintenance()? {
                break;
            }
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            if !storage(path.try_exists())? {
                if malformed.is_some() {
                    scan.malformed.pop_front();
                } else {
                    scan.pending.remove(&candidate.unwrap().0);
                }
                continue;
            }
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            let envelope = self.inspect_spool(&path);
            // 单次不可抢占IO返回后必须再检查，不能把已经耗尽的扫描/解密预算重新计时。
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            let envelope = match envelope {
                Ok(envelope) => envelope,
                Err(e) if e.code == QueueErrorCode::BodyProtectionUnavailable => return Err(e),
                Err(_) => {
                    super::ingress::replace_durable(&path, &path.with_extension("quarantine"))?;
                    report.quarantined += 1;
                    if malformed.is_some() {
                        scan.malformed.pop_front();
                    } else {
                        scan.pending.remove(&candidate.unwrap().0);
                    }
                    continue;
                }
            };
            if malformed.is_some() {
                // 已被合法替换的隔离候选重新进入排序，不能跳过更早候选直接执行。
                scan.malformed.pop_front();
                scan.insert(path, &envelope);
                continue;
            }
            match self.ingest_transaction(&envelope, Duration::from_millis(5), true, Some(deadline))
            {
                Ok(EnqueueOutcome::Duplicate { .. }) => report.duplicates += 1,
                Ok(_) => report.imported += 1,
                Err(e) if e.code == QueueErrorCode::AdmissionTimeout => break,
                Err(e) => return Err(e),
            }
            self.deps.faults.check(FaultPoint::AfterImportCommit)?;
            if check_deadline(Some(deadline)).is_err() {
                break;
            }
            storage(std::fs::remove_file(path))?;
            scan.pending.remove(&candidate.unwrap().0);
        }
        report.remaining = scan.remaining();
        if report.remaining == 0 {
            *scan = ImportScan::default();
        }
        Ok(report)
    }
}
fn read_field(bytes: &[u8], at: &mut usize) -> Result<Option<String>, QueueError> {
    if bytes.len().saturating_sub(*at) < 4 {
        return Err(error(QueueErrorCode::InvalidInput));
    }
    let len = u32::from_le_bytes(bytes[*at..*at + 4].try_into().unwrap());
    *at += 4;
    if len == u32::MAX {
        return Ok(None);
    }
    let len = len as usize;
    if len > 16384 || bytes.len().saturating_sub(*at) < len {
        return Err(error(QueueErrorCode::InvalidInput));
    }
    let text = storage(std::str::from_utf8(&bytes[*at..*at + len]))?.to_owned();
    *at += len;
    Ok(Some(text))
}
