use crate::{
    error::{Error, Result},
    types::bounded,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    User,
    Assistant,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// 通用输入只承载消息；身份、凭据、事件 ID 和时间由 bin 自行处理。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Message {
    pub session_id: String,
    pub role: Role,
    pub body: String,
    pub turn_id: Option<String>,
    pub project_key: Option<String>,
}

#[derive(PartialEq, Eq)]
pub(crate) enum Output {
    Compatible,
    Quiet,
}

pub(crate) struct Input {
    pub message: Message,
    pub output: Output,
}

pub(crate) fn parse(value: Value) -> Result<Option<Input>> {
    let input = if let Some(event) = value.get("hook_event_name") {
        let event = event
            .as_str()
            .ok_or_else(|| Error::invalid("hook 事件类型必须为字符串"))?;
        let (role, body) = match event {
            "UserPromptSubmit" => (Role::User, value.get("prompt").and_then(Value::as_str)),
            "Stop" => (
                Role::Assistant,
                value.get("last_assistant_message").and_then(Value::as_str),
            ),
            // 原生事件优先判断，不把工具／其他事件里的 role、body 当作消息上传。
            _ => return Ok(None),
        };
        let Some(body) = body.filter(|body| !body.trim().is_empty()) else {
            return Ok(None);
        };
        let session_id = value
            .get("session_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("缺少会话 ID"))?;
        Input {
            message: Message {
                session_id: session_id.into(),
                role,
                body: body.into(),
                turn_id: value
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                project_key: None,
            },
            output: Output::Compatible,
        }
    } else {
        let message: Message = serde_json::from_value(value)
            .map_err(|_| Error::invalid("通用采集输入无效：须提供 session_id、role（user／assistant）和 body，可选 turn_id、project_key"))?;
        if message.body.trim().is_empty() {
            return Ok(None);
        }
        Input {
            message,
            output: Output::Quiet,
        }
    };
    bounded(&input.message.session_id, 128)?;
    bounded(&input.message.body, 60000)?;
    for field in [&input.message.turn_id, &input.message.project_key]
        .into_iter()
        .flatten()
    {
        bounded(field, 128)?;
    }
    Ok(Some(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compatible_hooks_preserve_messages_and_output_contract() {
        for (event, field, role) in [
            ("UserPromptSubmit", "prompt", Role::User),
            ("Stop", "last_assistant_message", Role::Assistant),
        ] {
            let mut value = json!({"hook_event_name":event,"session_id":"会话","turn_id":"轮次","cwd":"/客户端路径"});
            value[field] = json!("虚构消息");
            let input = parse(value).unwrap().unwrap();
            assert!(input.message.role == role && input.output == Output::Compatible);
            assert_eq!(input.message.body, "虚构消息");
        }
    }

    #[test]
    fn generic_input_has_no_client_specific_output_or_identity_fields() {
        let message = json!({"session_id":"通用会话","role":"assistant","body":"虚构最终回复","project_key":"demo-project"});
        let input = parse(message.clone()).unwrap().unwrap();
        assert!(input.message.role == Role::Assistant && input.output == Output::Quiet);
        assert_eq!(input.message.project_key.as_deref(), Some("demo-project"));
        for field in ["agent_id", "hook_key", "url", "id", "occurred_at"] {
            let mut invalid = message.clone();
            invalid[field] = json!("不得回显的虚构字段");
            let error = parse(invalid).err().unwrap();
            assert!(!error.message.contains("不得回显的虚构字段"));
        }
        for role in ["tool", "system", "USER"] {
            let mut invalid = message.clone();
            invalid["role"] = json!(role);
            assert!(parse(invalid).is_err());
        }
    }

    #[test]
    fn ignored_native_events_and_empty_messages_never_fall_back_to_generic_capture() {
        assert!(parse(json!({"hook_event_name":"PostToolUse","session_id":"会话","role":"user","body":"不应采集的工具输出"})).unwrap().is_none());
        assert!(
            parse(json!({"hook_event_name":"Stop","session_id":"会话"}))
                .unwrap()
                .is_none()
        );
        assert!(
            parse(json!({"session_id":"会话","role":"user","body":"  \n"}))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn generic_input_enforces_utf8_byte_budget_and_bounded_metadata_before_queuing() {
        let mut message = json!({"session_id":"会话","role":"user","body":"你".repeat(20000)});
        assert!(parse(message.clone()).is_ok());
        message["body"] = json!("你".repeat(20001));
        assert!(parse(message.clone()).is_err());
        message["body"] = json!("虚构消息");
        message["session_id"] = json!("a".repeat(129));
        assert!(parse(message.clone()).is_err());
        message["session_id"] = json!("会话");
        message["project_key"] = json!(" ");
        assert!(parse(message).is_err());
    }
}
