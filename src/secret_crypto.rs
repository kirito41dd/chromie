use crate::{
    error::{Error, Result},
    server_key::ServerKey,
    types::SpaceId,
};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use hkdf::{
    Hkdf,
    hmac::{Hmac, Mac},
};
use p256::{elliptic_curve::zeroize::Zeroizing, pkcs8::EncodePublicKey};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const FORMAT_VERSION: i32 = 1;
pub use crate::secret_contract::MAX_SECRET_BYTES;

#[derive(Clone)]
pub struct SecretCipher {
    key: ServerKey,
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::{
        SecretKey,
        elliptic_curve::Generate,
        pkcs8::{EncodePrivateKey, LineEnding},
    };

    #[test]
    fn model_keys_are_bound_to_generation_connection_revision_and_separate_purpose() {
        let pem = SecretKey::try_generate()
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        let cipher = SecretCipher::from_key(&ServerKey::from_pem(&pem).unwrap()).unwrap();
        let id = Uuid::now_v7();
        let envelope = cipher.seal_model(id, 1, b"fictional-model-key").unwrap();
        let open = |id, revision| {
            cipher.open_model(
                id,
                EncryptedPayload {
                    revision,
                    format: 1,
                    key_id: cipher.key_id(),
                    nonce: &envelope.nonce,
                    ciphertext: &envelope.ciphertext,
                },
            )
        };
        assert_eq!(open(id, 1).unwrap().as_slice(), b"fictional-model-key");
        assert!(open(id, 2).is_err());
        assert!(open(Uuid::now_v7(), 1).is_err());
        assert!(
            cipher
                .open(
                    SpaceId(Uuid::nil()),
                    id,
                    EncryptedPayload {
                        revision: 1,
                        format: 1,
                        key_id: cipher.key_id(),
                        nonce: &envelope.nonce,
                        ciphertext: &envelope.ciphertext
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn agent_keys_bind_space_identity_purpose_revision_and_ciphertext() {
        let pem = SecretKey::try_generate()
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        let cipher = SecretCipher::from_key(&ServerKey::from_pem(&pem).unwrap()).unwrap();
        let space = SpaceId(Uuid::now_v7());
        let agent = Uuid::now_v7();
        let envelope = cipher
            .seal_agent(space, agent, "mcp", 1, b"fictional-agent-key")
            .unwrap();
        let open = |space, agent, kind, revision, ciphertext: &[u8]| {
            cipher.open_agent(
                space,
                agent,
                kind,
                EncryptedPayload {
                    revision,
                    format: FORMAT_VERSION,
                    key_id: cipher.key_id(),
                    nonce: &envelope.nonce,
                    ciphertext,
                },
            )
        };
        assert!(
            open(space, agent, "mcp", 1, &envelope.ciphertext)
                .unwrap()
                .as_slice()
                == b"fictional-agent-key"
        );
        assert!(
            open(
                SpaceId(Uuid::now_v7()),
                agent,
                "mcp",
                1,
                &envelope.ciphertext
            )
            .is_err()
        );
        assert!(open(space, Uuid::now_v7(), "mcp", 1, &envelope.ciphertext).is_err());
        assert!(open(space, agent, "hook", 1, &envelope.ciphertext).is_err());
        assert!(open(space, agent, "mcp", 2, &envelope.ciphertext).is_err());
        let mut changed = envelope.ciphertext.clone();
        changed[0] ^= 1;
        assert!(open(space, agent, "mcp", 1, &changed).is_err());
        assert!(
            cipher
                .open(
                    space,
                    agent,
                    EncryptedPayload {
                        revision: 1,
                        format: FORMAT_VERSION,
                        key_id: cipher.key_id(),
                        nonce: &envelope.nonce,
                        ciphertext: &envelope.ciphertext
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn reload_and_pem_format_preserve_keys_but_scope_object_revision_and_tampering_fail() {
        let pem = SecretKey::try_generate()
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        let key = ServerKey::from_pem(&pem).unwrap();
        let cipher = SecretCipher::from_key(&key).unwrap();
        let space = SpaceId(Uuid::now_v7());
        let id = Uuid::now_v7();
        let plaintext = b"fictional-credential-only\0\xff\n";
        let envelope = cipher.seal(space, id, 1, plaintext).unwrap();
        let reloaded =
            SecretCipher::from_key(&ServerKey::from_pem(&pem.replace('\n', "\r\n")).unwrap())
                .unwrap();
        assert_eq!(cipher.key_id(), reloaded.key_id());
        let open = |cipher: &SecretCipher, owner, target, revision, bytes: &[u8]| {
            cipher.open(
                owner,
                target,
                EncryptedPayload {
                    revision,
                    format: FORMAT_VERSION,
                    key_id: reloaded.key_id(),
                    nonce: &envelope.nonce,
                    ciphertext: bytes,
                },
            )
        };
        assert_eq!(
            open(&reloaded, space, id, 1, &envelope.ciphertext)
                .unwrap()
                .as_slice(),
            plaintext
        );
        assert!(
            open(
                &reloaded,
                SpaceId(Uuid::now_v7()),
                id,
                1,
                &envelope.ciphertext
            )
            .is_err()
        );
        assert!(open(&reloaded, space, Uuid::now_v7(), 1, &envelope.ciphertext).is_err());
        assert!(open(&reloaded, space, id, 2, &envelope.ciphertext).is_err());
        let mut changed = envelope.ciphertext.clone();
        changed[0] ^= 1;
        assert!(open(&reloaded, space, id, 1, &changed).is_err());
        let other_pem = SecretKey::try_generate()
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        let other = SecretCipher::from_key(&ServerKey::from_pem(&other_pem).unwrap()).unwrap();
        assert!(open(&other, space, id, 1, &envelope.ciphertext).is_err());
        assert_ne!(
            cipher.seal(space, id, 1, plaintext).unwrap().nonce,
            envelope.nonce
        );
        assert_ne!(
            cipher
                .fingerprint(space, b"metadata", b"weak-example")
                .unwrap(),
            Sha256::digest(b"weak-example").to_vec()
        );
        assert_ne!(
            cipher.fingerprint(space, b"metadata", plaintext).unwrap(),
            cipher
                .fingerprint(space, b"other-metadata", plaintext)
                .unwrap()
        );
    }
}

pub(crate) struct Envelope {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub(crate) struct EncryptedPayload<'a> {
    pub revision: i32,
    pub format: i32,
    pub key_id: &'a str,
    pub nonce: &'a [u8],
    pub ciphertext: &'a [u8],
}

impl SecretCipher {
    // 实例模型密钥具有独立的派生用途和 AAD，不能与 Secret／Agent 密钥互换。
    pub(crate) fn seal_model(
        &self,
        generation: Uuid,
        revision: i32,
        plaintext: &[u8],
    ) -> Result<Envelope> {
        if revision < 1 || plaintext.len() > 8192 {
            return Err(Error::invalid("模型凭据版本或大小无效"));
        }
        let space = SpaceId(Uuid::nil());
        let mut context = b"chromie/model-credential/aes-256-gcm/v1".to_vec();
        context.extend_from_slice(generation.as_bytes());
        context.extend_from_slice(&revision.to_be_bytes());
        let key = self.derive(b"model-credential-encryption-v1", space, &context)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_generate().map_err(|_| Error::unavailable())?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &context,
                },
            )
            .map_err(|_| Error::unavailable())?;
        Ok(Envelope {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub(crate) fn open_model(
        &self,
        generation: Uuid,
        payload: EncryptedPayload<'_>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        if payload.revision < 1
            || payload.key_id != self.id
            || payload.format != FORMAT_VERSION
            || payload.nonce.len() != 12
            || payload.ciphertext.len() > 8208
        {
            return Err(Error::unavailable());
        }
        let mut context = b"chromie/model-credential/aes-256-gcm/v1".to_vec();
        context.extend_from_slice(generation.as_bytes());
        context.extend_from_slice(&payload.revision.to_be_bytes());
        let key = self.derive(
            b"model-credential-encryption-v1",
            SpaceId(Uuid::nil()),
            &context,
        )?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_from(payload.nonce).map_err(|_| Error::unavailable())?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: payload.ciphertext,
                    aad: &context,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::unavailable())
    }

    pub fn from_key(key: &ServerKey) -> Result<Self> {
        let public = key
            .0
            .public_key()
            .to_public_key_der()
            .map_err(|_| Error::unavailable())?;
        let id = Sha256::digest(public.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            key: key.clone(),
            id,
        })
    }

    pub fn key_id(&self) -> &str {
        &self.id
    }

    fn derive(
        &self,
        purpose: &[u8],
        space: SpaceId,
        context: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>> {
        let raw = Zeroizing::new(self.key.0.to_bytes());
        let hkdf = Hkdf::<Sha256>::new(Some(b"chromie-secret-root-v1"), raw.as_slice());
        let mut info = purpose.to_vec();
        info.extend_from_slice(space.as_uuid().as_bytes());
        info.extend_from_slice(context);
        let mut key = Zeroizing::new([0u8; 32]);
        hkdf.expand(&info, key.as_mut())
            .map_err(|_| Error::unavailable())?;
        Ok(key)
    }

    fn context(space: SpaceId, id: Uuid, revision: i32) -> Result<Vec<u8>> {
        if revision < 1 {
            return Err(Error::unavailable());
        }
        let mut context = b"chromie/secret/aes-256-gcm/v1".to_vec();
        context.extend_from_slice(space.as_uuid().as_bytes());
        context.extend_from_slice(id.as_bytes());
        context.extend_from_slice(&revision.to_be_bytes());
        Ok(context)
    }

    pub(crate) fn seal(
        &self,
        space: SpaceId,
        id: Uuid,
        revision: i32,
        plaintext: &[u8],
    ) -> Result<Envelope> {
        if plaintext.is_empty() || plaintext.len() > MAX_SECRET_BYTES {
            return Err(Error::invalid("Secret 正文须为 1～60,000 字节"));
        }
        let context = Self::context(space, id, revision)?;
        // 每个对象载荷修订独立派生密钥，避免更新凭据与改说明之间的 nonce 生命周期混淆。
        let key = self.derive(b"encryption-v1", space, &context)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_generate().map_err(|_| Error::unavailable())?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &context,
                },
            )
            .map_err(|_| Error::unavailable())?;
        Ok(Envelope {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub(crate) fn open(
        &self,
        space: SpaceId,
        id: Uuid,
        payload: EncryptedPayload<'_>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        let EncryptedPayload {
            revision,
            format,
            key_id,
            nonce,
            ciphertext,
        } = payload;
        if format != FORMAT_VERSION
            || key_id != self.id
            || nonce.len() != 12
            || ciphertext.len() < 16
            || ciphertext.len() > MAX_SECRET_BYTES + 16
        {
            return Err(Error::unavailable());
        }
        let context = Self::context(space, id, revision)?;
        let key = self.derive(b"encryption-v1", space, &context)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_from(nonce).map_err(|_| Error::unavailable())?;
        let plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &context,
                },
            )
            .map_err(|_| Error::unavailable())?;
        Ok(Zeroizing::new(plaintext))
    }

    pub(crate) fn fingerprint(
        &self,
        space: SpaceId,
        metadata: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let key = self.derive(b"idempotency-hmac-v1", space, b"")?;
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_slice())
            .map_err(|_| Error::unavailable())?;
        mac.update(&(metadata.len() as u64).to_be_bytes());
        mac.update(metadata);
        mac.update(plaintext);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn agent_context(space: SpaceId, agent: Uuid, kind: &str, revision: i32) -> Result<Vec<u8>> {
        if !matches!(kind, "mcp" | "hook") || revision < 1 {
            return Err(Error::unavailable());
        }
        let mut context = b"chromie/agent-credential/aes-256-gcm/v1".to_vec();
        context.extend_from_slice(space.as_uuid().as_bytes());
        context.extend_from_slice(agent.as_bytes());
        context.extend_from_slice(&revision.to_be_bytes());
        context.extend_from_slice(kind.as_bytes());
        Ok(context)
    }

    pub(crate) fn seal_agent(
        &self,
        space: SpaceId,
        agent: Uuid,
        kind: &str,
        revision: i32,
        key_text: &[u8],
    ) -> Result<Envelope> {
        let context = Self::agent_context(space, agent, kind, revision)?;
        let key = self.derive(b"agent-credential-encryption-v1", space, &context)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_generate().map_err(|_| Error::unavailable())?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: key_text,
                    aad: &context,
                },
            )
            .map_err(|_| Error::unavailable())?;
        Ok(Envelope {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub(crate) fn open_agent(
        &self,
        space: SpaceId,
        agent: Uuid,
        kind: &str,
        payload: EncryptedPayload<'_>,
    ) -> Result<Zeroizing<Vec<u8>>> {
        if payload.format != FORMAT_VERSION
            || payload.key_id != self.id
            || payload.nonce.len() != 12
            || payload.ciphertext.len() > 256
        {
            return Err(Error::unavailable());
        }
        let context = Self::agent_context(space, agent, kind, payload.revision)?;
        let key = self.derive(b"agent-credential-encryption-v1", space, &context)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).map_err(|_| Error::unavailable())?;
        let nonce = Nonce::try_from(payload.nonce).map_err(|_| Error::unavailable())?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: payload.ciphertext,
                    aad: &context,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::unavailable())
    }
}
