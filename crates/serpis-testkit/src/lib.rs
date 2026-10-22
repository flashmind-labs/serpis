//! Test support for serpis' prompt caching: a strict model of provider caches ([`sim`]) and a
//! local HTTP provider that answers with it ([`mock`]). Never talks to a real provider.

pub mod mock;
pub mod sim;

pub use mock::{LogEntry, Mock, Reply, Script, commands};
pub use sim::{Config, Miss, Outcome, Simulator, Wire};
