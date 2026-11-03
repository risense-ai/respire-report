//! 生产AEAD仅接受安全存储取回的内容密钥；不包含默认/fixture密钥。
use super::*;
use crate::contracts::CredentialHandle;
use aes_gcm::{
    Aes256Gcm, KeyInit,
    aead::{Aead, Payload},
};
use ring::rand::{SecureRandom, SystemRandom};
pub struct AeadBodyProtector {
    key: CredentialHandle,
}
impl AeadBodyProtector {
    /// key必须来自宿主的系统安全存储；长度不符失败关闭，不生成替代密钥。
    pub fn from_key(key: CredentialHandle) -> Result<Self, QueueError> {
        if !key.with_secret(|b| b.len() == 32) {
            return Err(error(QueueErrorCode::BodyProtectionUnavailable));
        }
        Ok(Self { key })
    }
}
impl BodyProtector for AeadBodyProtector {
    fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<SealedBody, QueueError> {
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))?;
        let mut ciphertext = self
            .key
            .with_secret(|k| {
                Aes256Gcm::new_from_slice(k)
                    .unwrap()
                    .encrypt((&nonce).into(), Payload { msg: plain, aad })
            })
            .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))?;
        let tag = ciphertext
            .split_off(ciphertext.len() - 16)
            .try_into()
            .unwrap();
        Ok(SealedBody {
            nonce,
            tag,
            ciphertext,
        })
    }
    fn open(&self, sealed: &SealedBody, aad: &[u8]) -> Result<Vec<u8>, QueueError> {
        let mut bytes = sealed.ciphertext.clone();
        bytes.extend(sealed.tag);
        self.key
            .with_secret(|k| {
                Aes256Gcm::new_from_slice(k)
                    .unwrap()
                    .decrypt((&sealed.nonce).into(), Payload { msg: &bytes, aad })
            })
            .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))
    }
}
