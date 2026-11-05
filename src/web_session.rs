use crate::{
    auth::VerifiedPassword,
    error::{Error, Result},
    types::SpaceId,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

pub const SESSION_SECONDS: u64 = 30 * 24 * 60 * 60;
const ISSUER: &str = "chromie";
const AUDIENCE: &str = "chromie-dashboard";

#[derive(Clone)]
pub struct WebSessions(Arc<Keys>);

struct Keys {
    signing: EncodingKey,
    verifying: DecodingKey,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    credential: String,
    // 旧登录态继续有效；新登录总有独立标识，不能因同秒签发共享迁移授权。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jti: Option<uuid::Uuid>,
}

impl WebSessions {
    pub fn initialize(path: &Path) -> Result<()> {
        crate::server_key::ServerKey::initialize(path)
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::from_key(&crate::server_key::ServerKey::load(path)?)
    }

    pub fn from_pem(pem: &str) -> Result<Self> {
        Self::from_key(&crate::server_key::ServerKey::from_pem(pem)?)
    }

    pub fn from_key(key: &crate::server_key::ServerKey) -> Result<Self> {
        let secret = key.0.as_ref();
        let pem = secret
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| Error::unavailable())?;
        let public = secret
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| Error::unavailable())?;
        Ok(Self(Arc::new(Keys {
            signing: EncodingKey::from_ec_pem(pem.as_bytes())
                .map_err(|_| Error::invalid("无法载入 ES256 签名密钥"))?,
            verifying: DecodingKey::from_ec_pem(public.as_bytes())
                .map_err(|_| Error::unavailable())?,
        })))
    }

    pub(crate) fn issue(&self, verified: &VerifiedPassword) -> Result<String> {
        let now = jsonwebtoken::get_current_timestamp();
        let claims = Claims {
            iss: ISSUER.into(),
            aud: AUDIENCE.into(),
            sub: verified.space.as_uuid().to_string(),
            iat: now,
            nbf: now,
            exp: now + SESSION_SECONDS,
            credential: verified.version.clone(),
            jti: Some(uuid::Uuid::now_v7()),
        };
        encode(&Header::new(Algorithm::ES256), &claims, &self.0.signing)
            .map_err(|_| Error::unavailable())
    }

    pub(crate) fn verify(&self, token: &str, space: SpaceId) -> Result<String> {
        if token.len() > 4096 {
            return Err(Error::unauthorized());
        }
        let mut validation = Validation::new(Algorithm::ES256);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.sub = Some(space.as_uuid().to_string());
        validation.set_issuer(&[ISSUER]);
        validation.set_audience(&[AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "nbf", "iss", "aud", "sub"]);
        let claims = decode::<Claims>(token, &self.0.verifying, &validation)
            .map_err(|_| Error::unauthorized())?
            .claims;
        let now = jsonwebtoken::get_current_timestamp();
        if claims.iat > now
            || now >= claims.exp
            || claims.nbf != claims.iat
            || claims.exp.checked_sub(claims.iat) != Some(SESSION_SECONDS)
        {
            return Err(Error::unauthorized());
        }
        Ok(claims.credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::{SecretKey, elliptic_curve::Generate};
    use std::fs;
    use uuid::Uuid;

    fn keys() -> WebSessions {
        let pem = SecretKey::try_generate()
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap();
        WebSessions::from_pem(&pem).unwrap()
    }

    #[test]
    fn jwt_rejects_forgery_wrong_algorithm_space_and_claims() {
        let keys = keys();
        let space = SpaceId(Uuid::now_v7());
        let verified = VerifiedPassword {
            space,
            version: "测试凭据版本".into(),
        };
        let token = keys.issue(&verified).unwrap();
        assert_ne!(
            token,
            keys.issue(&verified).unwrap(),
            "每次登录必须形成独立会话"
        );
        assert_eq!(
            jsonwebtoken::decode_header(&token).unwrap().alg,
            Algorithm::ES256
        );
        assert_eq!(keys.verify(&token, space).unwrap(), verified.version);
        assert!(keys.verify(&token, SpaceId(Uuid::now_v7())).is_err());
        let mut forged = token.as_bytes().to_vec();
        let signature = token.rfind('.').unwrap() + 1;
        forged[signature] = if forged[signature] == b'A' {
            b'B'
        } else {
            b'A'
        };
        assert!(
            keys.verify(std::str::from_utf8(&forged).unwrap(), space)
                .is_err()
        );
        let now = jsonwebtoken::get_current_timestamp();
        let legacy = Claims {
            iss: ISSUER.into(),
            aud: AUDIENCE.into(),
            sub: space.as_uuid().to_string(),
            iat: now,
            nbf: now,
            exp: now + SESSION_SECONDS,
            credential: verified.version.clone(),
            jti: None,
        };
        let legacy = encode(&Header::new(Algorithm::ES256), &legacy, &keys.0.signing).unwrap();
        assert_eq!(
            keys.verify(&legacy, space).unwrap(),
            verified.version,
            "升级不能使已有登录态失效"
        );
        for (issuer, audience, iat, nbf, exp) in [
            ("other", AUDIENCE, now, now, now + SESSION_SECONDS),
            (ISSUER, "other", now, now, now + SESSION_SECONDS),
            (
                ISSUER,
                AUDIENCE,
                now - SESSION_SECONDS - 1,
                now - SESSION_SECONDS - 1,
                now - 1,
            ),
            (
                ISSUER,
                AUDIENCE,
                now + 60,
                now + 60,
                now + 60 + SESSION_SECONDS,
            ),
            (ISSUER, AUDIENCE, now, now, now + SESSION_SECONDS + 1),
        ] {
            let claims = Claims {
                iss: issuer.into(),
                aud: audience.into(),
                sub: space.as_uuid().to_string(),
                iat,
                nbf,
                exp,
                credential: verified.version.clone(),
                jti: None,
            };
            let invalid = encode(&Header::new(Algorithm::ES256), &claims, &keys.0.signing).unwrap();
            assert!(keys.verify(&invalid, space).is_err());
            let wrong_algorithm = encode(
                &Header::new(Algorithm::HS256),
                &claims,
                &EncodingKey::from_secret(b"fictional-test-only"),
            )
            .unwrap();
            assert!(keys.verify(&wrong_algorithm, space).is_err());
        }
    }

    #[test]
    fn persisted_key_survives_reload_and_is_not_overwritten() {
        let directory = std::env::temp_dir().join(format!("chromie-key-test-{}", Uuid::now_v7()));
        let path = directory.join("key.pem");
        WebSessions::initialize(&path).unwrap();
        let keys = WebSessions::load(&path).unwrap();
        let space = SpaceId(Uuid::now_v7());
        let token = keys
            .issue(&VerifiedPassword {
                space,
                version: "测试版本".into(),
            })
            .unwrap();
        assert!(WebSessions::initialize(&path).is_err());
        assert_eq!(
            WebSessions::load(&path)
                .unwrap()
                .verify(&token, space)
                .unwrap(),
            "测试版本"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(WebSessions::load(&path).is_err());
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
