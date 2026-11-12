use console::{Emoji, StyledObject};

use crate::ui::style;

// Text glyphs accept terminal-palette coloring instead of fixed emoji colors.
pub fn checkmark() -> StyledObject<&'static str> {
    style::green("✔")
}

pub fn crossmark() -> StyledObject<&'static str> {
    style::red("✘")
}

pub fn warning() -> StyledObject<&'static str> {
    style::yellow("⚠")
}

// Unicode
pub static DIRECTORY: Emoji<'_, '_> = Emoji("📁", "");
pub static LINK: Emoji<'_, '_> = Emoji("🔗", "");
pub static PACKAGE: Emoji<'_, '_> = Emoji("📦", "");
pub static RECYCLE: Emoji<'_, '_> = Emoji("♻️", "");
pub static TRAFFIC_LIGHT: Emoji<'_, '_> = Emoji("🚦", "");
pub static TOOLS: Emoji<'_, '_> = Emoji("🛠️", "");
pub static THREAD: Emoji<'_, '_> = Emoji("🧵", "");
pub static GREEN_CIRCLE: Emoji<'_, '_> = Emoji("🟢", "");
pub static SPARKLE: Emoji<'_, '_> = Emoji("✨", "");
pub static MOVE_FILE: Emoji<'_, '_> = Emoji("📃", "");
pub static TRASH: Emoji<'_, '_> = Emoji("🗑️", "");
pub static BLOCK: Emoji<'_, '_> = Emoji("🧩", "");
pub static MIGRATE: Emoji<'_, '_> = Emoji("🔀", "");
pub static LOCK: Emoji<'_, '_> = Emoji("🔒", "");
pub static STETHOSCOPE: Emoji<'_, '_> = Emoji("🩺", "");
// static LOOKING_GLASS: Emoji<'_, '_> = Emoji("🔍  ", "");
// static TRUCK: Emoji<'_, '_> = Emoji("🚚  ", "");
// static CLIP: Emoji<'_, '_> = Emoji("🔗  ", "");
// static PAPER: Emoji<'_, '_> = Emoji("📃  ", "");

// Nerd fonts
// pub static CLONE: Emoji<'_, '_> = Emoji("", "");
// ✔
