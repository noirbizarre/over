//! A git-based file overlay manager (dotfiles as overlays)
//!
//! The engine lives here; `src/main.rs` (the `over` binary) is a thin CLI
//! over it.

pub mod actions;
pub mod cli;
pub mod commit;
pub mod desired;
pub mod diff;
pub mod exec;
pub mod lint;
pub mod materialize;
pub mod overlays;
pub mod plan;
pub mod status;
pub mod sync;
pub mod ui;
pub mod unapply;
pub mod xdg;

pub mod utils;
