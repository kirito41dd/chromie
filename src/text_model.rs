//! 可选文本模型只接受有限 JSON 输入并返回结构化整理结果，不提供工具执行能力。
use crate::{
    error::{Error, Result},
    knowledge::{Link, MemoryType},
};
use p256::elliptic_curve::zeroize::Zeroizing;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub expected_revision: i64,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
}
fn default_concurrency() -> u32 {
    4
}

pub(crate) struct Connection {
    pub max_output_tokens: u32,
    pub base_url: String,
    pub model: String,
    pub key: Option<Zeroizing<String>>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Organize,
    Extract,
    Maintain,
}
impl JobKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Organize => "organize",
            Self::Extract => "extract",
            Self::Maintain => "maintain",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Queue {
    pub request_id: Uuid,
    pub kind: JobKind,
    #[serde(default)]
    pub memory_ids: Vec<Uuid>,
    #[serde(default)]
    pub event_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Output {
    pub notes: Vec<Draft>,
    #[serde(default)]
    pub conversation_title: Option<String>,
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
    #[serde(default)]
    pub revisions: Vec<MaintenanceRevision>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaintenanceRevision {
    pub memory_id: Uuid,
    pub expected_revision: i32,
    pub content: String,
    pub reason: String,
    pub feedback_refs: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Draft {
    pub memory_id: Option<Uuid>,
    pub expected_revision: Option<i32>,
    pub content: String,
    pub memory_type: MemoryType,
    #[serde(default)]
    pub source_refs: Vec<Uuid>,
    #[serde(default)]
    pub links: Vec<Link>,
    /// 仅合并已完整读取、同范围的主题；旧主题保留别名与历史。
    #[serde(default)]
    pub merge_from: Vec<Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conflict {
    pub memory_ids: Vec<Uuid>,
    pub reason: String,
}

pub(crate) const INSTRUCTIONS: &str = include_str!("text_model_instructions.md");

pub(crate) fn structural_error(code: &str) -> bool {
    matches!(
        code,
        "model_output_truncated"
            | "model_response_too_large"
            | "model_output_json"
            | "model_output_schema"
            | "model_evidence_invalid"
    )
}

impl Connection {
    pub(crate) async fn request(
        &self,
        input: &Value,
        usage: &mut Option<crate::embedding::CallUsage>,
    ) -> Result<Value> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("Chromie/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::unavailable())?;
        let mut request = client.post(format!("{}/chat/completions", self.base_url)).json(&json!({
            "model":self.model,"stream":false,"max_tokens":self.max_output_tokens,"response_format":{"type":"json_object"},
            "messages":[{"role":"system","content":INSTRUCTIONS},{"role":"user","content":input.to_string()}]
        }));
        if let Some(key) = &self.key {
            request = request.bearer_auth(key.as_str());
        }
        *usage = Some(crate::embedding::CallUsage {
            date: crate::types::now().date(),
            tokens: None,
            failed: false,
        });
        let mut response = request
            .send()
            .await
            .map_err(|_| error("model_unreachable", "无法连接文本模型或请求超时"))?;
        let status = response.status();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error("model_unreachable", "模型响应未完整接收"))?
        {
            if bytes.len().saturating_add(chunk.len()) > 512 * 1024 {
                return Err(error("model_response_too_large", "模型响应超过上限"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice::<Value>(&bytes);
        if let Ok(value) = &value
            && let Some(usage) = usage
        {
            usage.tokens = value["usage"]["total_tokens"]
                .as_u64()
                .or_else(|| {
                    value["usage"]["prompt_tokens"]
                        .as_u64()?
                        .checked_add(value["usage"]["completion_tokens"].as_u64()?)
                })
                .filter(|n| *n <= i64::MAX as u64);
        }
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => error("model_auth", "文本模型鉴权失败，请核对 API Key"),
                429 => error("model_rate_limited", "文本模型限流，将自动重试"),
                408 | 500..=599 => error("model_server_error", "文本模型暂时故障，将自动重试"),
                _ => error(
                    "model_request_rejected",
                    "文本模型拒绝请求，请核对 OpenAI 兼容 JSON 接口",
                ),
            });
        }
        let value = value.map_err(|_| error("model_http_json", "文本模型响应不是有效 JSON"))?;
        if value["choices"][0]["finish_reason"] != "stop" {
            return Err(if value["choices"][0]["finish_reason"] == "length" {
                error(
                    "model_output_truncated",
                    "模型输出达到长度上限，未写入记忆；请增加输出预算或减少每批数量",
                )
            } else {
                error("model_finish_reason", "模型未正常完成输出，未写入记忆")
            });
        }
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| error("model_output_missing", "模型未返回文本结果"))?;
        let parsed: Value = serde_json::from_str(content)
            .map_err(|_| error("model_output_json", "模型输出不是有效 JSON，未写入记忆"))?;
        let valid = match input["kind"].as_str() {
            Some("extract") => {
                serde_json::from_value::<crate::extraction::Generated>(parsed.clone()).is_ok()
            }
            Some("review") => {
                serde_json::from_value::<crate::extraction::Reviewed>(parsed.clone()).is_ok()
            }
            _ => serde_json::from_value::<Output>(parsed.clone()).is_ok(),
        };
        if !valid {
            return Err(error(
                "model_output_schema",
                "模型输出结构不符合整理约定，未写入记忆",
            ));
        }
        Ok(parsed)
    }
}

pub(crate) fn error(code: &'static str, message: &'static str) -> Error {
    Error {
        code,
        message,
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
    }
}
