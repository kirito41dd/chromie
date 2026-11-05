use crate::{
    agent_key::KeyKind,
    environment::required,
    error::{Error, Result},
    space_key::SpaceKey,
};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde::Deserialize;
use std::{env, fs, path::Path};
use uuid::Uuid;

#[cfg(all(test, unix))]
#[path = "client_config_tests.rs"]
mod tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    url: String,
    space: String,
    agent_id: Uuid,
    #[serde(default)]
    hook_key: String,
    #[serde(default)]
    mcp_key: String,
}

pub(crate) struct ClientConnection {
    pub base_url: String,
    pub space_id: String,
    pub agent_id: Uuid,
    pub key: Zeroizing<String>,
}

pub(crate) fn load(kind: KeyKind, path: Option<&Path>) -> Result<ClientConnection> {
    let environment_path = env::var_os("CHROMIE_CLIENT_CONFIG");
    let file = path.or_else(|| environment_path.as_deref().map(Path::new));
    let (address, space, agent_id, key) = if let Some(path) = file {
        let meta =
            fs::symlink_metadata(path).map_err(|_| Error::invalid("无法读取本机接入配置"))?;
        if !meta.is_file() || meta.len() > 8192 {
            return Err(Error::invalid("本机接入配置必须为普通私有文件，最大 8 KiB"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(Error::invalid("本机接入配置权限必须为 0600 或更严格"));
            }
        }
        let bytes =
            Zeroizing::new(fs::read(path).map_err(|_| Error::invalid("无法读取本机接入配置"))?);
        let config: FileConfig = serde_json::from_slice(bytes.as_slice())
            .map_err(|_| Error::invalid("本机接入配置 JSON 格式无效"))?;
        let key = match kind {
            KeyKind::Mcp => config.mcp_key,
            KeyKind::Hook => config.hook_key,
        };
        (config.url, config.space, config.agent_id, key)
    } else {
        let agent_id = Uuid::parse_str(&required("CHROMIE_AGENT_ID")?)
            .map_err(|_| Error::invalid("CHROMIE_AGENT_ID 无效"))?;
        (
            required("CHROMIE_URL")?,
            required("CHROMIE_SPACE")?,
            agent_id,
            required(match kind {
                KeyKind::Mcp => "CHROMIE_MCP_KEY",
                KeyKind::Hook => "CHROMIE_HOOK_KEY",
            })?,
        )
    };
    let url = reqwest::Url::parse(&address).map_err(|_| Error::invalid("接入 URL 无效"))?;
    if !crate::transport::allows(&url) {
        return Err(Error::invalid("接入地址必须使用 HTTP 或 HTTPS"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid("接入 URL 不能携带凭据、查询或片段"));
    }
    let space = SpaceKey::parse(&space)
        .ok_or_else(|| Error::invalid("空间须为小写英文与连字符组成的短标识"))?;
    if !key.starts_with(kind.prefix())
        || key.len() != kind.prefix().len() + 64
        || !key
            .bytes()
            .skip(kind.prefix().len())
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::invalid(
            "接入密钥格式或用途无效，请从 Web 接入页重新配置",
        ));
    }
    Ok(ClientConnection {
        base_url: address.trim_end_matches('/').into(),
        space_id: space.as_str().into(),
        agent_id,
        key: Zeroizing::new(key),
    })
}
