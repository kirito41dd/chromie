#![cfg_attr(
    not(test),
    deny(clippy::string_slice, clippy::expect_used, clippy::unwrap_used)
)]
#![cfg_attr(not(test), forbid(unsafe_code))]

#[tokio::main]
async fn main() {
    migration::cli::run_cli(migration::Migrator).await;
}
