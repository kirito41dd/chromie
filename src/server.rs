//! 复用服务运行与容器停止信号，不改变各入口的鉴权规则。
use crate::{
    error::{Error, Result},
    store::Store,
    web_session::WebSessions,
};
use std::{net::SocketAddr, time::Duration};

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("收到停止信号，正在结束服务");
}

pub async fn serve(
    store: Store,
    listen: SocketAddr,
    origin: Option<String>,
    sessions: WebSessions,
) -> Result<()> {
    let admin = if let Ok(key) = std::env::var("CHROMIE_ADMIN_SPACE") {
        let key = crate::space_key::SpaceKey::parse(&key)
            .ok_or_else(|| Error::invalid("管理空间标识无效"))?;
        Some(store.resolve_space(&key).await?)
    } else {
        None
    };
    store.initialize_model_admin(admin).await?;
    let router = crate::http::router_with_sessions(store.clone(), origin, sessions).await?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|_| Error::invalid("无法绑定监听地址"))?;
    let vector_store = store.clone();
    let vector_worker = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            timer.tick().await;
            if let Err(error) = vector_store.sync_embedding_due().await {
                tracing::warn!(code = error.code, "向量任务暂未完成，将重试");
            }
        }
    });
    let worker = tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            timer.tick().await;
            if let Err(error) = store.sync_due().await {
                tracing::warn!(code = error.code, "索引任务暂未完成，将重试");
            }
        }
    });
    tracing::info!(address = %listen, "Chromie 服务已启动");
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await;
    worker.abort();
    vector_worker.abort();
    let _ = worker.await;
    let _ = vector_worker.await;
    result.map_err(|_| Error::unavailable())
}
