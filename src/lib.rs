#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("Fray currently supports Unix (Linux/macOS) only.");
pub mod archive;
pub mod attention;
pub mod client;
pub mod diagnostics;
pub mod keepalive;
pub mod model;
pub mod mote;
pub mod notification;
pub mod owner;
pub mod presence;
pub mod recovery;
pub mod retention;
pub mod review;
pub mod routing;
pub mod server;
pub mod session;
pub mod skill;
pub mod snapshot;
pub mod stats;
pub mod store;
