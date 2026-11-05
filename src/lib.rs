#![cfg_attr(
    not(test),
    deny(clippy::string_slice, clippy::expect_used, clippy::unwrap_used)
)]
#![cfg_attr(not(test), forbid(unsafe_code))]

#[cfg(feature = "server")]
pub mod agent;
#[cfg(feature = "server")]
mod agent_http;
#[cfg(feature = "server")]
pub mod auth;
#[cfg(feature = "client")]
pub mod capture;
#[cfg(feature = "client")]
mod capture_input;
#[cfg(feature = "client")]
pub mod cli_setup;
#[cfg(feature = "client")]
mod client_config;
#[cfg(feature = "client")]
pub mod codex_check;
#[cfg(feature = "client")]
pub mod codex_setup;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod dashboard;
#[cfg(feature = "server")]
pub mod deployment;
#[cfg(feature = "server")]
pub mod embedding;
#[cfg(feature = "server")]
pub mod embedding_batch;
#[cfg(feature = "server")]
pub mod entity;
pub mod error;
#[cfg(feature = "server")]
pub mod http;
#[cfg(feature = "server")]
pub mod mcp;
#[cfg(feature = "server")]
pub mod messages;
#[cfg(feature = "server")]
mod model_runtime;
#[cfg(feature = "server")]
mod pairing;
#[cfg(feature = "server")]
pub mod recall_usage;
#[cfg(feature = "server")]
pub mod retrieval_capabilities;
#[cfg(feature = "server")]
pub mod retrieval_policy;
#[cfg(feature = "server")]
pub mod search;
pub mod secret_audit;
#[cfg(feature = "client")]
pub mod secret_cli;
pub mod secret_contract;
#[cfg(feature = "server")]
pub mod secret_crypto;
#[cfg(feature = "client")]
mod secret_exec;
#[cfg(feature = "client")]
mod secret_terminal;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod server_key;
#[cfg(feature = "server")]
pub mod setup_guides;
pub mod shell_commands;
pub mod space_key;
#[cfg(feature = "server")]
pub mod store;
#[cfg(feature = "server")]
mod system_http;
mod transport;
pub mod types;
#[cfg(feature = "server")]
mod web;
#[cfg(feature = "server")]
mod web_auth;
#[cfg(feature = "server")]
pub mod web_session;

mod agent_key;
pub mod command_line;
mod environment;

#[cfg(feature = "server")]
pub mod transfer;

#[cfg(feature = "server")]
mod transfer_http;

#[cfg(feature = "server")]
mod transfer_unlock;
