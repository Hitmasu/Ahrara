pub mod api;
pub use ahrara_core::auth;
pub use ahrara_core::config;
pub mod core;
pub use ahrara_core::mail;
pub mod mcp;
pub mod mcp_stdio;
pub use ahrara_core::protocol;
pub use ahrara_core::secure;
pub use ahrara_core::storage;
pub use ahrara_core::transport;
pub mod update;
pub mod web;
pub mod web_state;

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {}
            };

            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
