#[cfg(feature = "server")]
use axum::{
    Json,
    response::{IntoResponse, Response},
};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: &'static str,
    pub message: &'static str,
    pub status: StatusCode,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn invalid(message: &'static str) -> Self {
        Self {
            code: "invalid_argument",
            message,
            status: StatusCode::BAD_REQUEST,
        }
    }
    pub fn missing() -> Self {
        Self {
            code: "not_found",
            message: "当前空间中没有可访问的对象",
            status: StatusCode::NOT_FOUND,
        }
    }
    pub fn conflict(code: &'static str) -> Self {
        Self {
            code,
            message: "版本或幂等参数冲突，请重新读取后判断",
            status: StatusCode::CONFLICT,
        }
    }
    pub fn unauthorized() -> Self {
        Self {
            code: "unauthorized",
            message: "空间或密码无效",
            status: StatusCode::UNAUTHORIZED,
        }
    }
    pub fn unavailable() -> Self {
        Self {
            code: "unavailable",
            message: "服务暂不可用，请稍后重试",
            status: StatusCode::SERVICE_UNAVAILABLE,
        }
    }
    pub fn agent_unauthorized() -> Self {
        Self {
            code: "unauthorized",
            message: "接入密钥无效、已停用或已重置",
            status: StatusCode::UNAUTHORIZED,
        }
    }
    pub fn limited() -> Self {
        Self {
            code: "rate_limited",
            message: "鉴权请求过于频繁，请稍后重试",
            status: StatusCode::TOO_MANY_REQUESTS,
        }
    }
    pub fn json(&self) -> Value {
        json!({"code":self.code,"message":self.message})
    }
}

#[cfg(feature = "server")]
impl From<sea_orm::DbErr> for Error {
    fn from(_: sea_orm::DbErr) -> Self {
        // 数据库错误可能包含语句与参数，不能直接记录或返回给客户端。
        Self {
            code: "storage_error",
            message: "数据库操作失败",
            status: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[cfg(feature = "server")]
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.status, Json(self.json())).into_response()
    }
}
