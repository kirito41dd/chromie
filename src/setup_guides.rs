//! 分主题的 Agent 操作指南；只承载公开说明，不接收或返回凭据。
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    #[default]
    Setup,
    Secret,
    Capture,
    Diagnostics,
}
#[derive(Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// 按主题读取；secret 包含隐藏输入、已提供原文的授权代存、文件存取和权限指引。省略时为 setup。
    #[serde(default)]
    pub topic: Topic,
    /// 可选简短用途；只写任务说明，禁止填写密码或密钥原文。
    pub purpose: Option<String>,
    /// 上次收到的 guide_version；一致时省略静态指南，身份与权限仍实时检查。
    pub known_guide_version: Option<String>,
}

pub fn version(topic: Topic) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{}",
        Sha256::digest(guide(topic).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

pub fn payload(topic: Topic, known: Option<&str>) -> serde_json::Value {
    let version = version(topic);
    let cached = known == Some(version.as_str());
    let mut result = serde_json::json!({"topic":topic,"guide_version":version,"guide_cached":cached,"capability_version":1,
        "capabilities":{"secret_exec":1,"secret_access_audit":1}});
    if !cached {
        result["guide"] = guide(topic).into();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_match_omits_guide_and_wrong_or_other_topic_version_refreshes_it() {
        let version = version(Topic::Secret);
        assert!(
            payload(Topic::Secret, Some(&version))
                .get("guide")
                .is_none()
        );
        assert!(payload(Topic::Secret, None)["guide"].is_string());
        assert!(payload(Topic::Setup, Some(&version))["guide"].is_string());
        assert!(payload(Topic::Secret, Some("old"))["guide"].is_string());
    }
}
pub fn guide(topic: Topic) -> &'static str {
    match topic {
        Topic::Setup => include_str!("setup_guide.md"),
        Topic::Secret => include_str!("secret_guide.md"),
        Topic::Capture => include_str!("capture_guide.md"),
        Topic::Diagnostics => include_str!("diagnostics_guide.md"),
    }
}
