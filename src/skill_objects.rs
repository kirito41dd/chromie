//! 不可变文件的 S3 访问边界。空间、修订和发布状态均由 MySQL 决定。
use crate::{
    error::{Error, Result},
    skill_package::MAX_BYTES,
};
use p256::elliptic_curve::zeroize::Zeroizing;
use s3::{Bucket, Region, creds::Credentials};
use std::{env, fs, time::Duration};
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset, format_description::well_known::Rfc3339};

pub(crate) struct BucketStore(Box<Bucket>);

fn unavailable<T>(_: T) -> Error {
    Error::unavailable()
}

fn success(status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(Error::unavailable())
    }
}

impl BucketStore {
    pub(crate) fn configured() -> bool {
        env::var_os("CHROMIE_S3_ENDPOINT").is_some()
    }

    pub(crate) fn from_env() -> Result<Self> {
        let endpoint = env::var("CHROMIE_S3_ENDPOINT").map_err(unavailable)?;
        let url = reqwest::Url::parse(&endpoint).map_err(unavailable)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::invalid("对象存储地址须为有效的 HTTP 或 HTTPS 地址"));
        }
        let name = env::var("CHROMIE_S3_BUCKET").map_err(unavailable)?;
        let access = env::var("CHROMIE_S3_ACCESS_KEY").map_err(unavailable)?;
        let secret = Zeroizing::new(match env::var_os("CHROMIE_S3_SECRET_KEY_FILE") {
            Some(path) => fs::read_to_string(path).map_err(unavailable)?,
            None => env::var("CHROMIE_S3_SECRET_KEY").map_err(unavailable)?,
        });
        if name.is_empty() || access.is_empty() || secret.trim_end().is_empty() {
            return Err(Error::unavailable());
        }
        let credentials =
            Credentials::new(Some(&access), Some(secret.trim_end()), None, None, None)
                .map_err(unavailable)?;
        let region = Region::Custom {
            region: "us-east-1".to_owned(),
            endpoint: endpoint.trim_end_matches('/').to_owned(),
        };
        let bucket = Bucket::new(&name, region, credentials)
            .map_err(unavailable)?
            .with_path_style()
            .with_request_timeout(Duration::from_secs(30))
            .map_err(unavailable)?;
        Ok(Self(bucket))
    }

    pub(crate) async fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let response = self.0.put_object(key, bytes).await.map_err(unavailable)?;
        success(response.status_code())
    }

    /// 直接按已登记的原始长度接收。短读和超量响应都不能作为已验证的内容返回。
    pub(crate) async fn get(&self, key: &str, size: usize) -> Result<Vec<u8>> {
        if size > MAX_BYTES {
            return Err(Error::unavailable());
        }
        let mut contents = vec![0; size];
        let mut writer = std::io::Cursor::new(contents.as_mut_slice());
        let status = self
            .0
            .get_object_to_writer(key, &mut writer)
            .await
            .map_err(unavailable)?;
        success(status)?;
        if writer.position() != size as u64 {
            return Err(Error::unavailable());
        }
        Ok(contents)
    }

    pub(crate) async fn delete(&self, key: &str) -> Result<()> {
        let response = self.0.delete_object(key).await.map_err(unavailable)?;
        // 删除后进程可能在更新数据库前停止。对象已经消失同样可以完成回收。
        if response.status_code() == 404 {
            return Ok(());
        }
        success(response.status_code())
    }

    pub(crate) async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
    ) -> Result<(Vec<(String, PrimitiveDateTime)>, Option<String>)> {
        let (page, status) = self
            .0
            .list_page(
                prefix.into(),
                None,
                after.map(str::to_owned),
                None,
                Some(256),
            )
            .await
            .map_err(unavailable)?;
        success(status)?;
        let next = if page.is_truncated {
            Some(
                page.next_continuation_token
                    .filter(|s| !s.is_empty())
                    .ok_or_else(Error::unavailable)?,
            )
        } else {
            None
        };
        let mut files = Vec::with_capacity(page.contents.len());
        for object in page.contents {
            let utc = OffsetDateTime::parse(&object.last_modified, &Rfc3339)
                .map_err(unavailable)?
                .to_offset(UtcOffset::UTC);
            files.push((object.key, PrimitiveDateTime::new(utc.date(), utc.time())));
        }
        Ok((files, next))
    }
}
