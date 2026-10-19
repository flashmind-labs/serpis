//! serpis-ai: a Rust port of pi-ai — one streaming interface over many LLM providers.

pub mod api;
pub mod registry;
pub mod stream;
pub mod transform;
pub mod types;

pub use api::stream;
pub use registry::{Paths, Registry};
pub use stream::{CacheRetention, EventStream, StreamEvent, StreamOptions};
pub use tokio_util::sync::CancellationToken;
pub use types::*;

pub const USER_AGENT: &str = concat!("serpis/", env!("CARGO_PKG_VERSION"));
