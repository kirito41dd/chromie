//! 容器部署的首次初始化与恢复边界；不读取主机开发配置。
use crate::{
    config::Config,
    entity::spaces,
    error::{Error, Result},
    secret_crypto::SecretCipher,
    server_key::ServerKey,
    space_key::SpaceKey,
    store::Store,
};
use p256::{SecretKey, elliptic_curve::Generate};
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u8,
    pub public_origin: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    mysql_password: String,
    mysql_root_password: String,
    qdrant_key: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    key_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Initialized {
    space_id: Uuid,
}

pub struct Deployment {
    config: PathBuf,
    credentials: Credentials,
    settings: Settings,
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::invalid("无法检查部署文件，请检查目录权限")),
    }
}

fn directory(path: &Path, shared: bool) -> Result<()> {
    if let Some(meta) = metadata(path)? {
        if !meta.is_dir() {
            return Err(Error::invalid("部署目录不能是符号链接或普通文件"));
        }
    } else {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(if shared { 0o755 } else { 0o700 });
        }
        builder
            .create(path)
            .map_err(|_| Error::invalid("无法创建部署目录"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if shared { 0o755 } else { 0o700 }),
        )
        .map_err(|_| Error::invalid("无法设置部署目录权限"))?;
    }
    Ok(())
}

fn read(path: &Path, private: bool) -> Result<Vec<u8>> {
    let meta = metadata(path)?.ok_or_else(|| {
        Error::invalid("部署配置缺失；已有实例请恢复原 config 目录，禁止重新生成私钥")
    })?;
    if !meta.is_file() || meta.len() > 16384 {
        return Err(Error::invalid("部署配置必须是普通文件且不超过 16 KiB"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private && meta.permissions().mode() & 0o077 != 0 {
            return Err(Error::invalid("部署私有配置权限须为 0600 或更严格"));
        }
    }
    fs::read(path).map_err(|_| Error::invalid("无法读取部署配置"))
}

fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&read(path, true)?)
        .map_err(|_| Error::invalid("部署 JSON 配置格式或字段无效，未输出配置内容"))
}

/// 同目录临时文件原子发布，拒绝覆盖；崩溃重试保留已落盘的原文件。
fn create(path: &Path, bytes: &[u8], shared: bool) -> Result<()> {
    if metadata(path)?.is_some() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::invalid("部署文件缺少父目录"))?;
    let staging = parent.join(format!(".bootstrap-{}.tmp", Uuid::now_v7()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // MySQL 以独立 UID 读取只读挂载的子目录；宿主上层 config 为 0700。
        options.mode(if shared { 0o444 } else { 0o600 });
    }
    let result = (|| {
        let mut file = options
            .open(&staging)
            .map_err(|_| Error::invalid("无法创建部署临时文件"))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::invalid("无法持久保存部署配置"))?;
        fs::hard_link(&staging, path)
            .map_err(|_| Error::invalid("部署配置已存在或无法原子发布，未覆盖"))?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::invalid("配置已保存但目录同步失败，请保留原文件并检查磁盘"))?;
        Ok(())
    })();
    let _ = fs::remove_file(staging);
    result
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|_| Error::unavailable())
}

fn random_password() -> Result<String> {
    let secret = SecretKey::try_generate().map_err(|_| Error::unavailable())?;
    Ok(secret
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn password_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn populated(path: &Path) -> Result<bool> {
    let Some(meta) = metadata(path)? else {
        return Ok(false);
    };
    if !meta.is_dir() {
        return Err(Error::invalid("数据路径不是普通目录"));
    }
    Ok(fs::read_dir(path)
        .map_err(|_| Error::invalid("无法检查数据目录"))?
        .next()
        .is_some())
}

pub fn lock(config: &Path) -> Result<fs::File> {
    let path = config.join(".bootstrap.lock");
    if let Some(meta) = metadata(&path)?
        && !meta.is_file()
    {
        return Err(Error::invalid("初始化锁必须为普通文件"));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| Error::invalid("无法打开初始化锁"))?;
    file.try_lock()
        .map_err(|_| Error::invalid("另一个进程正在初始化部署，请稍后重试"))?;
    Ok(file)
}

pub fn initialize(config: &Path, data: &Path) -> Result<bool> {
    directory(config, false)?;
    directory(data, false)?;
    let _lock = lock(config)?;
    let exists = metadata(&config.join("instance.json"))?.is_some();
    if !exists && metadata(&config.join("initialized.json"))?.is_some() {
        return Err(Error::invalid(
            "已完成初始化但实例清单缺失，请恢复原 config，未重新初始化",
        ));
    }
    if !exists && (populated(&data.join("mysql"))? || populated(&data.join("qdrant"))?) {
        return Err(Error::invalid(
            "已有持久数据但缺少完整实例配置，请恢复原 config；拒绝生成新私钥或凭据",
        ));
    }
    if exists {
        Deployment::load(config)?;
        if metadata(&config.join("initialized.json"))?.is_some() && !populated(&data.join("mysql"))?
        {
            return Err(Error::invalid(
                "已初始化实例缺少 MySQL 数据，请恢复原 data，未创建空库替代",
            ));
        }
        return Ok(false);
    }
    directory(&config.join("mysql"), true)?;
    directory(&config.join("qdrant"), false)?;
    if metadata(&config.join("instance-key.pem"))?.is_none() {
        if metadata(&config.join("credentials.json"))?.is_some()
            || metadata(&config.join("initial-space.txt"))?.is_some()
        {
            return Err(Error::invalid(
                "已有私有配置却缺少根私钥，请恢复原私钥，未重新生成",
            ));
        }
        // 根私钥先于其他私有材料落盘，确保不把丢钥的半成品误判为全新实例。
        ServerKey::initialize(&config.join("instance-key.pem"))?;
    }
    create(
        &config.join("chromie.json"),
        &encode(&Settings {
            version: 1,
            public_origin: None,
        })?,
        false,
    )?;
    if metadata(&config.join("credentials.json"))?.is_none() {
        create(
            &config.join("credentials.json"),
            &encode(&Credentials {
                mysql_password: random_password()?,
                mysql_root_password: random_password()?,
                qdrant_key: random_password()?,
            })?,
            false,
        )?;
    }
    let credentials: Credentials = json(&config.join("credentials.json"))?;
    for value in [
        &credentials.mysql_password,
        &credentials.mysql_root_password,
        &credentials.qdrant_key,
    ] {
        if !password_valid(value) {
            return Err(Error::invalid("自动生成的服务凭据格式无效，请恢复原文件"));
        }
    }
    create(
        &config.join("mysql/password"),
        credentials.mysql_password.as_bytes(),
        true,
    )?;
    create(
        &config.join("mysql/root-password"),
        credentials.mysql_root_password.as_bytes(),
        true,
    )?;
    create(
        &config.join("qdrant/config.yaml"),
        qdrant_config(&credentials.qdrant_key).as_bytes(),
        false,
    )?;
    let key = ServerKey::load(&config.join("instance-key.pem"))?;
    create(
        &config.join("initial-space.txt"),
        format!("空间：personal\n密码：{}\n", random_password()?).as_bytes(),
        false,
    )?;
    let manifest = Manifest {
        version: 1,
        key_id: SecretCipher::from_key(&key)?.key_id().to_owned(),
    };
    // 最后发布实例清单；之前失败的空实例可续做，已有文件始终不覆盖。
    create(&config.join("instance.json"), &encode(&manifest)?, false)?;
    Deployment::load(config)?;
    Ok(true)
}

fn qdrant_config(key: &str) -> String {
    format!("telemetry_disabled: true\nservice:\n  api_key: \"{key}\"\n")
}

impl Deployment {
    pub fn load(config: &Path) -> Result<Self> {
        let meta = metadata(config)?
            .ok_or_else(|| Error::invalid("部署配置目录不存在，请先执行容器初始化"))?;
        if !meta.is_dir() {
            return Err(Error::invalid("配置目录不能是符号链接或普通文件"));
        }
        let manifest: Manifest = json(&config.join("instance.json"))?;
        let settings: Settings = json(&config.join("chromie.json"))?;
        if manifest.version != 1 || settings.version != 1 {
            return Err(Error::invalid("不支持的部署配置版本"));
        }
        if let Some(origin) = &settings.public_origin {
            crate::web_auth::validate_public_origin(origin)?;
        }
        let credentials: Credentials = json(&config.join("credentials.json"))?;
        for value in [
            &credentials.mysql_password,
            &credentials.mysql_root_password,
            &credentials.qdrant_key,
        ] {
            if !password_valid(value) {
                return Err(Error::invalid(
                    "服务凭据格式无效，请恢复原凭据，不能直接更换数据库密码文件",
                ));
            }
        }
        for (name, value) in [
            ("mysql/password", credentials.mysql_password.as_bytes()),
            (
                "mysql/root-password",
                credentials.mysql_root_password.as_bytes(),
            ),
            (
                "qdrant/config.yaml",
                qdrant_config(&credentials.qdrant_key).as_bytes(),
            ),
        ] {
            if read(&config.join(name), name.starts_with("qdrant"))? != value {
                return Err(Error::invalid(
                    "生成配置与实例凭据不一致，请恢复原文件，未自动覆盖",
                ));
            }
        }
        let key = ServerKey::load(&config.join("instance-key.pem"))?;
        if SecretCipher::from_key(&key)?.key_id() != manifest.key_id {
            return Err(Error::invalid(
                "实例根私钥与原配置不一致，请恢复原私钥，未更换或迁移密文",
            ));
        }
        if metadata(&config.join("initialized.json"))?.is_none() {
            Self::initial_password(config)?;
        }
        Ok(Self {
            config: config.to_owned(),
            credentials,
            settings,
        })
    }

    fn initial_password(config: &Path) -> Result<String> {
        let text = String::from_utf8(read(&config.join("initial-space.txt"), true)?)
            .map_err(|_| Error::invalid("初始空间文件格式无效"))?;
        let password = text
            .lines()
            .find_map(|line| line.strip_prefix("密码："))
            .filter(|value| password_valid(value))
            .ok_or_else(|| Error::invalid("初始空间密码格式无效，未输出文件内容"))?;
        if text.lines().next() != Some("空间：personal") {
            return Err(Error::invalid("初始空间标识无效"));
        }
        Ok(password.to_owned())
    }

    pub fn runtime_config(&self) -> Result<Config> {
        Ok(Config {
            database_url: format!(
                "mysql://chromie:{}@mysql:3306/chromie",
                self.credentials.mysql_password
            ),
            qdrant_url: "http://qdrant:6334".into(),
            qdrant_key: Some(self.credentials.qdrant_key.clone()),
            listen: "0.0.0.0:8732".parse().map_err(|_| Error::unavailable())?,
            public_origin: self.settings.public_origin.clone(),
            key_file: self.config.join("instance-key.pem"),
        })
    }

    pub async fn ensure_initial_space(&self, store: &Store) -> Result<()> {
        let path = self.config.join("initialized.json");
        if metadata(&path)?.is_some() {
            let state: Initialized = json(&path)?;
            if spaces::Entity::find_by_id(state.space_id.as_bytes().to_vec())
                .one(&store.db)
                .await?
                .is_none()
            {
                return Err(Error::invalid(
                    "数据库缺少原初始空间，请恢复对应数据，未重建空间或密码",
                ));
            }
            store
                .initialize_model_admin(Some(crate::types::SpaceId(state.space_id)))
                .await?;
            return Ok(());
        }
        let slug = SpaceKey::parse("personal").ok_or_else(Error::unavailable)?;
        let id = match store.resolve_space(&slug).await {
            Ok(id) => id.as_uuid(),
            Err(error) if error.code == "not_found" => {
                store
                    .create_space(
                        slug,
                        "个人空间".into(),
                        Self::initial_password(&self.config)?,
                    )
                    .await?
            }
            Err(error) => return Err(error),
        };
        create(&path, &encode(&Initialized { space_id: id })?, false)?;
        store
            .initialize_model_admin(Some(crate::types::SpaceId(id)))
            .await
    }
}

pub fn initial_login(config: &Path) -> Result<String> {
    Deployment::load(config)?;
    String::from_utf8(read(&config.join("initial-space.txt"), true)?)
        .map_err(|_| Error::invalid("初始登录文件不是有效文本"))
}
