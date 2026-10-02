//! Shared Ahrara protocol, receiver and isolated inbox API. No CLI or HTTP service.
pub mod auth;
pub mod config;
pub mod core;
mod email_html;
pub mod filter;
pub mod inbox;
pub mod mail;
pub mod protocol;
pub mod query;
pub mod secure;
pub mod storage;
pub mod transport;
pub use filter::EmailFilter;
pub use inbox::{EmailPage, Inbox, InboxCredentials, InboxOptions, WaitOptions};
