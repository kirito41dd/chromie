//! “灵魂”快照只包含当前正文与 Secret；age 封装独立于实例根私钥。
//! 文件先完整鉴别与校验，再交给空间内的追加事务；公开预览永远不持有原文。
use crate::{
    error::{Error, Result},
    secret_contract::SecretContract,
    space_key::SpaceKey,
    types::bounded,
};
use age::secrecy::SecretString;
use p256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Write},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

pub const MAX_PLAINTEXT: usize = 64 * 1024 * 1024;
pub const MAX_PACKAGE: usize = MAX_PLAINTEXT + 1024 * 1024;
pub const MAX_RECORDS: usize = 10000;
pub(crate) const PREVIEW_PAGE_SIZE: usize = 5;

/// 页码只选择公开说明的切片；确认摘要始终绑定整个包和目标状态。
#[derive(Clone, Copy, Default)]
pub(crate) struct PreviewPage(usize);
impl PreviewPage {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        let index = value
            .parse::<usize>()
            .ok()
            .filter(|n| *n < MAX_RECORDS.div_ceil(PREVIEW_PAGE_SIZE))
            .ok_or_else(|| Error::invalid("预览页码无效"))?;
        Ok(Self(index))
    }
    pub(crate) fn index(self) -> usize {
        self.0
    }
    pub(crate) fn range(self, total: usize) -> Result<std::ops::Range<usize>> {
        let start = self.0 * PREVIEW_PAGE_SIZE;
        if self.0 != 0 && start >= total {
            return Err(Error::invalid("预览页码超出范围"));
        }
        Ok(start..start.saturating_add(PREVIEW_PAGE_SIZE).min(total))
    }
}
const WORK_FACTOR: u8 = 18;
static SLOTS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
pub(crate) async fn acquire() -> Result<OwnedSemaphorePermit> {
    tokio::time::timeout(Duration::from_secs(15), SLOTS.clone().acquire_owned())
        .await
        .map_err(|_| Error {
            code: "transfer_busy",
            message: "另一项炼化或夺舍正在处理，请稍后重试",
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        })?
        .map_err(|_| Error::unavailable())
}

/// 私有载荷没有 Debug，离开包的生命周期时清除原文字节。
pub struct SecretBytes(pub Zeroizing<Vec<u8>>);
impl Serialize for SecretBytes {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.as_slice().serialize(s)
    }
}
impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Vec::<u8>::deserialize(d).map(|v| Self(Zeroizing::new(v)))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    pub package_id: Uuid,
    pub source_space: String,
    pub source_name: String,
    pub exported_at: String,
    pub records: Vec<Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub memory_id: Uuid,
    pub source_revision: i32,
    pub content: String,
    pub project_key: Option<String>,
    pub secret: Option<Secret>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub contract: SecretContract,
    pub value: SecretBytes,
}
impl Bundle {
    pub fn validate(&self) -> Result<()> {
        if self.format != "chromie-soul"
            || self.version != 1
            || self.package_id.is_nil()
            || self.records.len() > MAX_RECORDS
            || SpaceKey::parse(&self.source_space).is_none()
        {
            return Err(invalid_package());
        }
        bounded(&self.source_name, 100)?;
        time::OffsetDateTime::parse(
            &self.exported_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| invalid_package())?;
        let mut ids = HashSet::new();
        for row in &self.records {
            if row.memory_id.is_nil() || row.source_revision < 1 || !ids.insert(row.memory_id) {
                return Err(invalid_package());
            }
            bounded(&row.content, if row.secret.is_some() { 256 } else { 16000 })?;
            if let Some(project) = &row.project_key {
                bounded(project, 128)?;
            }
            if let Some(secret) = &row.secret {
                secret.contract.validate_payload(&secret.value.0)?;
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<Vec<u8>> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, self).map_err(|_| invalid_package())?;
        Ok(writer.0.finalize().to_vec())
    }
}
pub(crate) fn passphrase(value: &str) -> Result<()> {
    if !(8..=1024).contains(&value.len()) {
        return Err(Error::invalid("迁移口令须为 8～1024 字节"));
    }
    Ok(())
}
fn invalid_package() -> Error {
    Error {
        code: "transfer_invalid_package",
        message: "迁移包无效、内容不完整或版本不支持",
        status: reqwest::StatusCode::BAD_REQUEST,
    }
}
fn decryption_error() -> Error {
    Error {
        code: "transfer_decryption_failed",
        message: "迁移口令不匹配，或文件已损坏／截断",
        status: reqwest::StatusCode::BAD_REQUEST,
    }
}

pub(crate) fn encrypt(bundle: Bundle, password: String) -> Result<Vec<u8>> {
    encrypt_with_cost(bundle, password, WORK_FACTOR)
}
fn encrypt_with_cost(bundle: Bundle, password: String, cost: u8) -> Result<Vec<u8>> {
    passphrase(&password)?;
    bundle.validate()?;
    struct Limited<'a>(&'a mut Vec<u8>);
    impl Write for Limited<'_> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(b.len()) > MAX_PLAINTEXT {
                return Err(std::io::Error::other("迁移内容超过上限"));
            }
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut plaintext = Zeroizing::new(Vec::new());
    serde_json::to_writer(Limited(&mut plaintext), &bundle)
        .map_err(|_| Error::invalid("炼化内容超过 64 MiB 或无法编码"))?;
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(password));
    recipient.set_work_factor(cost);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|_| Error::unavailable())?;
    let mut output = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut output)
        .map_err(|_| Error::unavailable())?;
    writer
        .write_all(&plaintext)
        .map_err(|_| Error::unavailable())?;
    writer.finish().map_err(|_| Error::unavailable())?;
    if output.len() > MAX_PACKAGE {
        return Err(Error::invalid("迁移包超过 65 MiB"));
    }
    Ok(output)
}
pub(crate) fn decrypt(bytes: Vec<u8>, password: String) -> Result<Bundle> {
    passphrase(&password)?;
    if bytes.is_empty() || bytes.len() > MAX_PACKAGE {
        return Err(invalid_package());
    }
    let decryptor = age::Decryptor::new(bytes.as_slice()).map_err(|_| decryption_error())?;
    if !decryptor.is_scrypt() {
        return Err(invalid_package());
    }
    let mut identity = age::scrypt::Identity::new(SecretString::from(password));
    identity.set_max_work_factor(WORK_FACTOR);
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| decryption_error())?
        .take((MAX_PLAINTEXT + 1) as u64);
    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .read_to_end(&mut plaintext)
        .map_err(|_| decryption_error())?;
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(invalid_package());
    }
    let bundle: Bundle = serde_json::from_slice(&plaintext).map_err(|_| invalid_package())?;
    bundle.validate()?;
    Ok(bundle)
}

/// 仅识别源空间的标准 Secret 引用；调用方验证依赖可用后才写入目标正文。
fn reference_tokens<'a>(text: &'a str, source: &str) -> Vec<(&'a str, Uuid)> {
    let prefix = format!("secret://{source}/");
    text.match_indices(&prefix)
        .filter_map(|(start, _)| {
            let rest = text.get(start + prefix.len()..)?;
            let candidate = rest.get(..36)?;
            if rest
                .get(36..)?
                .starts_with(|c: char| c.is_ascii_hexdigit() || c == '-')
            {
                return None;
            }
            let id = Uuid::parse_str(candidate).ok()?;
            (id.hyphenated().to_string() == candidate.to_ascii_lowercase())
                .then_some((text.get(start..start + prefix.len() + 36)?, id))
        })
        .collect()
}
pub(crate) fn references(text: &str, source: &str) -> Vec<Uuid> {
    reference_tokens(text, source)
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}
pub(crate) fn relocate(text: &str, source: &str, target: &str) -> String {
    let mut result = text.to_owned();
    for (token, id) in reference_tokens(text, source) {
        result = result.replace(token, &format!("secret://{target}/{id}"));
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bundle() -> Bundle {
        Bundle {
            format: "chromie-soul".into(),
            version: 1,
            package_id: Uuid::now_v7(),
            source_space: "test".into(),
            source_name: "虚构灵魂".into(),
            exported_at: "2026-11-04T00:00:00Z".into(),
            records: vec![Record {
                memory_id: Uuid::now_v7(),
                source_revision: 2,
                content: "虚构凭据".into(),
                project_key: None,
                secret: Some(Secret {
                    contract: SecretContract::Text {
                        version: 1,
                        kind: crate::secret_contract::TextKind::Password,
                    },
                    value: SecretBytes(Zeroizing::new(b"00123 fictional".to_vec())),
                }),
            }],
        }
    }
    #[test]
    fn age_package_preserves_bytes_and_rejects_password_tampering_and_truncation() {
        let original = bundle();
        let id = original.records[0].memory_id;
        let bytes = encrypt_with_cost(original, "fictional-password".into(), 2).unwrap();
        assert!(!bytes.windows(5).any(|v| v == b"00123"));
        let opened = decrypt(bytes.clone(), "fictional-password".into()).unwrap();
        assert_eq!(opened.records[0].memory_id, id);
        assert_eq!(
            opened.records[0]
                .secret
                .as_ref()
                .unwrap()
                .value
                .0
                .as_slice(),
            b"00123 fictional"
        );
        assert!(decrypt(bytes.clone(), "wrong-password".into()).is_err());
        let mut changed = bytes.clone();
        let n = changed.len() - 9;
        changed[n] ^= 1;
        assert!(decrypt(changed, "fictional-password".into()).is_err());
        assert!(
            decrypt(
                bytes[..bytes.len() - 8].to_vec(),
                "fictional-password".into()
            )
            .is_err()
        );
    }
    #[test]
    fn standard_references_are_relocated_without_changing_other_spaces() {
        let id = Uuid::now_v7();
        let text = format!("凭据 secret://work/{id}；外部 secret://other/{id}");
        assert_eq!(references(&text, "work"), vec![id]);
        assert_eq!(
            relocate(&text, "work", "home"),
            format!("凭据 secret://home/{id}；外部 secret://other/{id}")
        );
        let upper = format!("secret://work/{}", id.to_string().to_uppercase());
        assert_eq!(
            relocate(&upper, "work", "home"),
            format!("secret://home/{id}")
        );
        assert!(references(&format!("{upper}abc"), "work").is_empty());
    }
}
