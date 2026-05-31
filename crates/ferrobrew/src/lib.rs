//! ferrobrew — a from-scratch Rust reimplementation of Homebrew's `brew`.
//!
//! This is the library half of the crate; the `ferrobrew` binary (`src/main.rs`) is a thin
//! dispatcher over it. Modules map onto Homebrew subsystems (the full roadmap lives in
//! `specs/architecture.md`): configuration & paths, platform/bottle-tag detection, the JSON API
//! client, the formula model, dependency resolution, bottle download, relocation, keg linking,
//! receipts, and the install pipeline.
//!
//! Implemented so far: [`config`], [`system`], [`error`], [`api`], [`bottle`], [`formula`].
//! Remaining subsystems are ported one at a time against the specs in `specs/`. Unlike the
//! abandoned Ruby-frontend design, ferrobrew is standalone: paths it cannot yet handle surface as
//! [`FerroError::Unsupported`] rather than deferring to Ruby.

pub mod api;
pub mod bottle;
pub mod commands;
pub mod config;
pub mod deps;
pub mod download;
pub mod error;
pub mod formula;
pub mod install;
pub mod keg;
pub mod relocate;
pub mod system;
pub mod tab;

pub use error::{FerroError, Result};
