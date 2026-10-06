//! Shared Ahrara protocol, receiver and isolated inbox API. No CLI or HTTP service.
//!
//! Browsers (wasm32) get only the pure modules: protocol, identity, mail and the
//! client session. Storage, configuration and network I/O are native.
pub mod auth;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod core;
mod email_html;
#[cfg(not(target_arch = "wasm32"))]
pub mod filter;
#[cfg(not(target_arch = "wasm32"))]
pub mod inbox;
pub mod mail;
pub mod model;
pub mod protocol;
#[cfg(not(target_arch = "wasm32"))]
pub mod query;
pub mod secure;
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod storage;
#[cfg(not(target_arch = "wasm32"))]
pub mod transport;
#[cfg(not(target_arch = "wasm32"))]
pub use filter::EmailFilter;
#[cfg(not(target_arch = "wasm32"))]
pub use inbox::{EmailPage, Inbox, InboxCredentials, InboxOptions, WaitOptions};
