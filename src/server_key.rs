use crate::error::{Error, Result};
use p256::{
    SecretKey,
    elliptic_curve::Generate,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding},
};
use std::{fs, io::Write, path::Path, sync::Arc};

/// 实例唯一根私钥；必须独立备份，缺失时不自动创建或替换。
#[derive(Clone)]
pub struct ServerKey(pub(crate) Arc<SecretKey>);

impl ServerKey {
    /// 本机显式初始化；原子发布新文件，绝不覆盖已有密钥。
    pub fn initialize(path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .map_err(|_| Error::invalid("无法创建密钥目录"))?;
        let secret = SecretKey::try_generate().map_err(|_| Error::unavailable())?;
        let pem = secret
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| Error::unavailable())?;
        let staging = parent.join(format!(".chromie-key-{}.tmp", uuid::Uuid::now_v7()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&staging)
            .map_err(|_| Error::invalid("无法创建私钥文件"))?;
        let result = (|| {
            file.write_all(pem.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| Error::invalid("无法保存私钥文件"))?;
            fs::hard_link(&staging, path)
                .map_err(|_| Error::invalid("密钥文件已存在或无法发布，未覆盖原文件"))?;
            #[cfg(unix)]
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| {
                    Error::invalid("密钥已创建，但无法同步目录，请保留原文件并检查磁盘")
                })?;
            Ok(())
        })();
        let _ = fs::remove_file(staging);
        result
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            Error::invalid("找不到实例根私钥；已有实例请恢复原私钥并核对 CHROMIE_KEY_FILE，禁止重新初始化；仅新实例使用 init-key")
        })?;
        if !metadata.is_file() || metadata.len() > 16384 {
            return Err(Error::invalid("实例根私钥必须是有效的普通文件"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::invalid(
                    "私钥文件不能对组或其他用户开放，请设置为 0600",
                ));
            }
        }
        let pem = p256::elliptic_curve::zeroize::Zeroizing::new(
            fs::read_to_string(path).map_err(|_| Error::invalid("无法读取实例根私钥"))?,
        );
        Self::from_pem(&pem)
    }

    pub fn from_pem(pem: &str) -> Result<Self> {
        let secret = SecretKey::from_pkcs8_pem(pem)
            .map_err(|_| Error::invalid("实例根私钥必须是 PKCS#8 格式的 P-256 私钥"))?;
        Ok(Self(Arc::new(secret)))
    }
}
