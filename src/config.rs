use crate::error::{Error, Result};
use std::{env, net::SocketAddr, path::PathBuf};

pub struct Config {
    pub database_url: String,
    pub qdrant_url: String,
    pub qdrant_key: Option<String>,
    pub listen: SocketAddr,
    pub public_origin: Option<String>,
    pub key_file: PathBuf,
}

pub use crate::environment::required;

impl Config {
    pub fn load() -> Result<Self> {
        // 镜像中的管理命令复用持久配置；不把连接串和服务凭据写入容器环境。
        if let Some(directory) = env::var_os("CHROMIE_CONFIG_DIR") {
            return crate::deployment::Deployment::load(std::path::Path::new(&directory))?
                .runtime_config();
        }
        let listen = env::var("CHROMIE_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8732".into())
            .parse()
            .map_err(|_| Error::invalid("监听地址格式无效"))?;
        Ok(Self {
            database_url: required("DATABASE_URL")?,
            qdrant_url: required("CHROMIE_QDRANT_URL")?,
            qdrant_key: env::var("CHROMIE_QDRANT_API_KEY").ok(),
            listen,
            public_origin: env::var("CHROMIE_PUBLIC_ORIGIN").ok(),
            key_file: env::var_os("CHROMIE_KEY_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".local/chromie-key.pem")),
        })
    }
}
