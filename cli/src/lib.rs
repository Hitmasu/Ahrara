pub mod api;
pub mod auth;
pub mod config;
pub mod core;
mod email_html;
pub mod mail;
pub mod mcp;
pub mod mcp_stdio;
pub mod protocol;
pub mod secure;
pub mod storage;
pub mod transport;
pub mod update;
pub mod web;
pub mod web_state;

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}};
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
