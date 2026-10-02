//! Application interfaces around the shared, embeddable Rust receiver.
use crate::{config::Config, web_state::WebState};
pub use ahrara_core::core::{ConnectionState, Event, Owner, Ownership, Status, WaitResult};
use anyhow::Result;
use std::{ops::Deref, sync::Arc};

pub struct Core {
    receiver: Arc<ahrara_core::core::Core>,
    pub web: WebState,
}

impl Deref for Core {
    type Target = Arc<ahrara_core::core::Core>;

    fn deref(&self) -> &Self::Target {
        &self.receiver
    }
}

impl Core {
    pub fn open(config: Config) -> Result<(Arc<Self>, bool)> {
        let web = WebState::load(&config)?;
        let (receiver, created) = ahrara_core::core::Core::open(config)?;
        Ok((Arc::new(Self { receiver, web }), created))
    }
}
