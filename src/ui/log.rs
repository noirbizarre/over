use console::Term;

use anyhow::Result;

pub fn info(msg: impl AsRef<str>) -> Result<()> {
    let term = Term::stdout();
    term.write_line(msg.as_ref())?;
    Ok(())
}
