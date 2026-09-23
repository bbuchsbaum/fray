#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("Fray currently supports Unix (Linux/macOS) only.");
pub mod attention;
pub mod client;
pub mod diagnostics;
pub mod model;
pub mod notification;
pub mod server;
pub mod session;
pub mod skill;
pub mod store;
