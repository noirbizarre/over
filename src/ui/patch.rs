//! Interactive presentation only; decisions do not themselves mutate an index.

use std::io::{IsTerminal as _, Write as _};

use anyhow::{Context as _, Result, ensure};
use dialoguer::Select;

use crate::actions::git::patch::Unit;
use crate::stage::patch::Decision;
use crate::ui::style::DialogTheme;

pub(crate) fn select(unit: &Unit, dry_run: bool) -> Result<Decision> {
    let _pause = super::progress::pause()?;
    ensure!(
        dry_run || std::io::stdin().is_terminal(),
        "over add --patch requires a terminal (use --dry-run to preview)"
    );
    let mut output = std::io::stderr().lock();
    writeln!(
        output,
        "\n{}: {}{}",
        unit.path.display(),
        unit.description,
        if dry_run { " (candidate)" } else { "" }
    )?;
    output.write_all(&unit.patch)?;
    output.flush()?;
    drop(output);
    if dry_run {
        return Ok(Decision::Skip);
    }
    let choice = Select::with_theme(&DialogTheme::default())
        .with_prompt("Stage this change? Finish applies accepted choices; Cancel discards all")
        .items(["Accept", "Skip", "Finish", "Cancel"])
        .default(1)
        .interact_opt()
        .context("patch selection interrupted; no choices staged")?;
    Ok(match choice {
        Some(0) => Decision::Accept,
        Some(1) => Decision::Skip,
        Some(2) => Decision::Finish,
        _ => Decision::Cancel,
    })
}
