//! 首次初始化单独保存 RustFS 凭据，升级旧实例复用原根私钥和数据库。
use super::*;
use serde_json::json;

pub(super) fn prepare(config: &Path, data: &Path) -> Result<()> {
    let dir = config.join("rustfs");
    let storage = data.join("rustfs");
    let provisioned = config.join("rustfs-configured");
    let initialized = metadata(&provisioned)?.is_some() || populated(&storage)?;
    if initialized
        && (metadata(&dir.join("root-secret"))?.is_none()
            || metadata(&dir.join("app-secret"))?.is_none())
    {
        return Err(Error::invalid(
            "RustFS 凭据缺失，请恢复对应配置和对象存储备份",
        ));
    }
    if metadata(&dir.join("ready"))?.is_some() && !populated(&storage)? {
        return Err(Error::invalid("已初始化的 RustFS 卷缺少数据，请恢复备份"));
    }
    directory(&dir, true)?;
    for name in ["root-secret", "app-secret"] {
        let path = dir.join(name);
        if metadata(&path)?.is_none() {
            create(&path, random_password()?.as_bytes(), true)?;
        }
        let value = read(&path, false)?;
        if !std::str::from_utf8(&value).is_ok_and(password_valid) {
            return Err(Error::invalid("RustFS 凭据格式无效"));
        }
    }
    let policy = json!({"Version":"2012-10-17","Statement":[
        {"Effect":"Allow","Action":["s3:ListBucket","s3:GetBucketLocation"],"Resource":["arn:aws:s3:::chromie-skills"]},
        {"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:DeleteObject"],"Resource":["arn:aws:s3:::chromie-skills/*"]}
    ]});
    create(&dir.join("policy.json"), &encode(&policy)?, true)?;
    create(&provisioned, b"1", false)?;
    Ok(())
}
