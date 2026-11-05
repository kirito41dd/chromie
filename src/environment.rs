use crate::error::{Error, Result};
use std::env;

pub fn required(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::invalid("缺少必要环境变量，请检查本地配置"))
}
