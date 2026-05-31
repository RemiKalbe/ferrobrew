//! Platform detection: architecture, operating system, and the resulting bottle tag.

pub mod macos_version;
pub mod tag;

pub use tag::{Arch, Os, Tag};
