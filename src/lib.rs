//! A git-based file overlay manager (dotfiles as overlays)
//!
//! The engine lives here; `src/main.rs` (the `over` binary) and
//! `src/bin/git-over.rs` (the `git-over` binary) are thin CLIs over it.

pub mod actions;
pub mod cli;
pub mod exec;
pub mod lint;
pub mod overlays;
pub mod ui;

pub mod utils;
