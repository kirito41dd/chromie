//! 可选模型的编码身份与有限 HTTP 边界；连接地址和密钥不参与向量身份。
use crate::error::{Error, Result};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub model: String,
    #[serde(default)]
    pub model_version: String,
    pub dimensions: u32,
    pub encoding: String,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || self.model_version.len() > 256
            || self.model_version.chars().any(char::is_control)
            || !(1..=8192).contains(&self.dimensions)
            || self.encoding != "plain-v1"
        {
            return Err(Error::invalid("模型、版本、维度或编码方式无效"));
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Error::unavailable())?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: i64,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub model_version: String,
    pub dimensions: u32,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(
        default,
        deserialize_with = "crate::embedding_batch::deserialize_update"
    )]
    pub batch_size: Option<Option<crate::embedding_batch::BatchSize>>,
    #[serde(
        default,
        deserialize_with = "crate::retrieval_policy::deserialize_update"
    )]
    pub similarity_threshold: Option<Option<crate::retrieval_policy::SimilarityThreshold>>,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub confirm_rebuild: bool,
    #[serde(default)]
    pub force_rebuild: bool,
}

impl Save {
    pub fn profile(&self) -> Profile {
        Profile {
            model: self.model.clone(),
            model_version: self.model_version.clone(),
            dimensions: self.dimensions,
            encoding: "plain-v1".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub expected_revision: i64,
}

pub(crate) struct Connection {
    pub base_url: String,
    pub profile: Profile,
    pub key: Option<Zeroizing<String>>,
}

pub(crate) struct CallUsage {
    pub date: time::Date,
    pub tokens: Option<u64>,
    pub failed: bool,
}

pub(crate) struct CallReport {
    pub result: Result<Vec<Vec<f32>>>,
    pub usage: Option<CallUsage>,
}

fn reported_tokens(value: &serde_json::Value) -> Option<u64> {
    let usage = value.get("usage")?;
    let tokens = match usage.get("total_tokens") {
        Some(value) => value.as_u64(),
        None => usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_u64),
    };
    tokens.filter(|tokens| *tokens <= i64::MAX as u64)
}

pub(crate) fn validate_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).map_err(|_| Error::invalid("模型服务地址无效"))?;
    if value.len() > 2048
        || !crate::transport::allows(&url)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid(
            "模型地址须为 HTTP 或 HTTPS Base URL，不能包含凭据、查询或片段",
        ));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

impl Connection {
    pub(crate) async fn embed_batch(&self, texts: &[&str], timeout: Duration) -> CallReport {
        let mut usage = None;
        let result = self.request_embeddings(texts, timeout, &mut usage).await;
        if let Some(usage) = &mut usage {
            usage.failed = result.is_err();
        }
        CallReport { result, usage }
    }

    async fn request_embeddings(
        &self,
        texts: &[&str],
        timeout: Duration,
        usage: &mut Option<CallUsage>,
    ) -> Result<Vec<Vec<f32>>> {
        self.profile.validate()?;
        if texts.is_empty()
            || texts.len() > crate::embedding_batch::MAX_BATCH_SIZE as usize
            || texts.iter().any(|text| text.trim().is_empty())
        {
            return Err(Error::invalid("单次 embedding 须包含 1～10 段非空文本"));
        }
        let client = reqwest::Client::builder()
            .user_agent(concat!("Chromie/", env!("CARGO_PKG_VERSION")))
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(3)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::unavailable())?;
        let mut request = client.post(format!("{}/embeddings", self.base_url)).json(&serde_json::json!({
            "model":self.profile.model,"input":texts,"dimensions":self.profile.dimensions,"encoding_format":"float"
        }));
        if let Some(key) = &self.key {
            request = request.bearer_auth(key.as_str());
        }
        // 以实际发送前的 UTC 日期归属用量；跨午夜返回仍计入开始日。
        *usage = Some(CallUsage {
            date: crate::types::now().date(),
            tokens: None,
            failed: false,
        });
        let mut response = request
            .send()
            .await
            .map_err(|_| model_error("model_unreachable", "无法连接向量模型或请求超时"))?;
        let status = response.status();
        let maximum_response = 512 * 1024 * texts.len();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| model_error("model_unreachable", "模型响应未完整接收"))?
        {
            if bytes.len().saturating_add(chunk.len()) > maximum_response {
                return Err(model_error("model_response", "模型响应超过允许大小"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes);
        if let Ok(value) = &value
            && let Some(usage) = usage
        {
            usage.tokens = reported_tokens(value);
        }
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => model_error("model_auth", "模型服务鉴权失败，请核对 API Key"),
                429 => model_error("model_rate_limited", "模型服务限流，将稍后重试"),
                408 | 500..=599 => {
                    model_error("model_server_error", "模型服务暂时故障，将自动重试")
                }
                _ if texts.len() > 1 => batch_protocol_error(),
                _ => model_error("model_response", "模型服务拒绝请求，请核对模型和接口配置"),
            });
        }
        parse_vectors(
            value.map_err(|_| protocol_error(texts.len()))?,
            texts.len(),
            self.profile.dimensions as usize,
        )
    }
}

fn batch_protocol_error() -> Error {
    model_error(
        "model_batch_response",
        "模型拒绝批量输入或返回对应关系无效，请调小每批文本数或检查接口",
    )
}
fn protocol_error(count: usize) -> Error {
    if count > 1 {
        batch_protocol_error()
    } else {
        model_error("model_response", "模型返回的向量数量、索引或格式无效")
    }
}

/// 不相信响应顺序；只有全部 index 唯一、完整且所有向量有效才交给任务发布层。
fn parse_vectors(
    value: serde_json::Value,
    count: usize,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    #[derive(Deserialize)]
    struct Response {
        data: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        index: usize,
        embedding: Vec<f32>,
    }
    let parsed: Response = serde_json::from_value(value).map_err(|_| protocol_error(count))?;
    if parsed.data.len() != count {
        return Err(protocol_error(count));
    }
    let mut ordered = vec![None; count];
    for item in parsed.data {
        let slot = ordered
            .get_mut(item.index)
            .ok_or_else(|| protocol_error(count))?;
        if slot.is_some() {
            return Err(protocol_error(count));
        }
        if item.embedding.len() != dimensions
            || item.embedding.iter().any(|n| !n.is_finite())
            || item.embedding.iter().all(|n| *n == 0.0)
        {
            return Err(model_error(
                "model_dimensions",
                "模型返回的向量维度或数值无效",
            ));
        }
        *slot = Some(item.embedding);
    }
    ordered
        .into_iter()
        .map(|slot| slot.ok_or_else(|| protocol_error(count)))
        .collect()
}

fn model_error(code: &'static str, message: &'static str) -> Error {
    Error {
        code,
        message,
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_response_maps_by_index_and_rejects_incomplete_or_invalid_batches() {
        use serde_json::json;
        let valid =
            json!({"data":[{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}]});
        assert_eq!(
            parse_vectors(valid.clone(), 2, 2).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );
        for value in [
            json!({"data":[]}),
            json!({"data":[{"index":0,"embedding":[1.0,0.0]}]}),
            json!({"data":[{"index":0,"embedding":[1.0,0.0]},{"index":0,"embedding":[0.0,1.0]}]}),
            json!({"data":[{"index":2,"embedding":[1.0,0.0]},{"index":0,"embedding":[0.0,1.0]}]}),
            json!({"data":[{"embedding":[1.0,0.0]},{"index":0,"embedding":[0.0,1.0]}]}),
        ] {
            assert_eq!(
                parse_vectors(value, 2, 2).unwrap_err().code,
                "model_batch_response"
            );
        }
        assert_eq!(
            parse_vectors(valid.clone(), 2, 3).unwrap_err().code,
            "model_dimensions"
        );
        let mut zero = valid;
        zero["data"][0]["embedding"] = json!([0.0, 0.0]);
        assert_eq!(
            parse_vectors(zero, 2, 2).unwrap_err().code,
            "model_dimensions"
        );
    }
    #[test]
    fn batch_update_distinguishes_omitted_auto_and_override() {
        let mut value = serde_json::json!({"expected_revision":0,"base_url":"http://127.0.0.1/v1","model":"test","dimensions":3,"api_key":null});
        assert_eq!(
            serde_json::from_value::<Save>(value.clone())
                .unwrap()
                .batch_size,
            None
        );
        value["batch_size"] = serde_json::Value::Null;
        assert_eq!(
            serde_json::from_value::<Save>(value.clone())
                .unwrap()
                .batch_size,
            Some(None)
        );
        value["batch_size"] = serde_json::json!(4);
        assert_eq!(
            serde_json::from_value::<Save>(value.clone())
                .unwrap()
                .batch_size
                .unwrap()
                .unwrap()
                .value(),
            4
        );
        for n in [0, 11] {
            value["batch_size"] = serde_json::json!(n);
            assert!(serde_json::from_value::<Save>(value.clone()).is_err());
        }
    }
    #[test]
    fn threshold_update_distinguishes_omitted_auto_and_explicit_zero() {
        let base = serde_json::json!({"expected_revision":0,"base_url":"http://127.0.0.1/v1","model":"test","dimensions":3,"api_key":null});
        let omitted: Save = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(omitted.similarity_threshold, None);
        let mut value = base.clone();
        value["similarity_threshold"] = serde_json::Value::Null;
        assert_eq!(
            serde_json::from_value::<Save>(value.clone())
                .unwrap()
                .similarity_threshold,
            Some(None)
        );
        value["similarity_threshold"] = serde_json::json!(0.0);
        assert_eq!(
            serde_json::from_value::<Save>(value.clone())
                .unwrap()
                .similarity_threshold
                .unwrap()
                .unwrap()
                .value(),
            0.0
        );
        for n in [-1.0, 1.1] {
            value["similarity_threshold"] = serde_json::json!(n);
            assert!(serde_json::from_value::<Save>(value.clone()).is_err());
        }
    }
    #[test]
    fn usage_counts_total_once_and_keeps_missing_or_invalid_values_unknown() {
        use serde_json::json;
        assert_eq!(
            reported_tokens(&json!({"usage":{"total_tokens":23,"prompt_tokens":23}})),
            Some(23)
        );
        assert_eq!(
            reported_tokens(&json!({"usage":{"prompt_tokens":0}})),
            Some(0)
        );
        for value in [
            json!({}),
            json!({"usage":null}),
            json!({"usage":{"total_tokens":-1}}),
            json!({"usage":{"total_tokens":-1,"prompt_tokens":23}}),
            json!({"usage":{"total_tokens":"23"}}),
            json!({"usage":{"total_tokens":u64::MAX}}),
        ] {
            assert_eq!(reported_tokens(&value), None);
        }
    }
    #[test]
    fn identity_includes_version_and_encoding_but_never_connection() {
        let a = Profile {
            model: "text-embedding-v4".into(),
            model_version: "".into(),
            dimensions: 1024,
            encoding: "plain-v1".into(),
        };
        let mut b = a.clone();
        b.model_version = "new-weights".into();
        assert_ne!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
        b = a.clone();
        b.model = "another-model".into();
        assert_ne!(a.fingerprint().unwrap(), b.fingerprint().unwrap());
        assert_eq!(
            validate_url("http://localhost:1234/v1/").unwrap(),
            "http://localhost:1234/v1"
        );
        assert!(validate_url("https://user:password@example.com/v1").is_err());
    }
}
