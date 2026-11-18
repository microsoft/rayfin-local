//! Shared orchestration for the CLI and native Node.js bindings.

pub mod analysis;
pub mod commands;
mod connectors;
mod control;
pub mod errors;
pub mod events;
pub mod integrations;
pub mod ports;
pub mod reload;
pub mod runtime;
mod seed;
mod shutdown;
mod tokens;
pub mod ui;

pub use runtime::{Runtime, RuntimeError, RuntimeOptions, StopInfo};
