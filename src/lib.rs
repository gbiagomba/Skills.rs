//! `skill`: a cross-agent skill manager.
//!
//! The binary is a thin shell over this library so that the acceptance tests can
//! drive the engine directly as well as through the command line.
//!
//! Module map, in dependency order:
//!
//! * [`exit`] and [`error`]: the stable exit-code contract and typed failures.
//! * [`safepath`]: containment, entry classification, atomic replacement. Every
//!   write goes through it.
//! * [`pkg`]: package discovery, `SKILL.md` validation, and tree hashing.
//! * [`agent`]: per-agent discovery, precedence, capabilities, and classification.
//! * [`cli`]: the command surface, output rendering, and command implementations.
//! * [`config`]: layered configuration and the platform directories we use.
//! * [`state`]: the schema-versioned SQLite store holding provenance and baselines.
//! * [`source`]: locator parsing and the transport backends.
//! * [`recon`]: the three-way comparison truth table and the plans it produces.
//! * [`txn`]: locking, the operation journal, atomic apply, and rollback.
//!
//! Nothing in this crate ever executes skill content. Acquired packages are data.

pub mod agent;
pub mod cli;
pub mod config;
pub mod error;
pub mod exit;
pub mod pkg;
pub mod recon;
pub mod safepath;
pub mod source;
pub mod state;
pub mod txn;

pub use error::{Error, Result};
pub use exit::ExitCode;
