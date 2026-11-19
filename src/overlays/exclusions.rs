//! Deployment exclusions deliberately leave metadata resolution intact.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, bail};
use globset::{GlobBuilder, GlobMatcher};

use crate::exec::Context;
use crate::overlays::Overlay;
use crate::ui;

#[derive(Default)]
pub(crate) struct Exclusions {
    patterns: Vec<(String, GlobMatcher)>,
    matched: BTreeSet<usize>,
    skipped: BTreeSet<String>,
}

impl Exclusions {
    pub(crate) fn new(patterns: &[String]) -> Result<Self> {
        let patterns = patterns
            .iter()
            .map(|pattern| {
                let glob = GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .build()
                    .with_context(|| format!("invalid --skip pattern '{pattern}'"))?;
                Ok((pattern.clone(), glob.compile_matcher()))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            patterns,
            ..Self::default()
        })
    }

    fn matching(&mut self, name: &str) -> Option<String> {
        let mut first = None;
        for (index, (pattern, matcher)) in self.patterns.iter().enumerate() {
            if matcher.is_match(name) {
                self.matched.insert(index);
                first.get_or_insert_with(|| pattern.clone());
            }
        }
        first
    }

    pub(crate) fn excludes(&mut self, name: &str) -> bool {
        if self.matching(name).is_some() {
            self.skipped.insert(name.to_owned());
            true
        } else {
            false
        }
    }

    /// Check the operational graph before desired-tree construction can prepare content.
    pub(crate) fn validate(&mut self, ctx: &Context, roots: &[Overlay]) -> Result<()> {
        // Metadata-only dependencies still count as matches, but are never deployment conflicts.
        for dependency in ctx.resolved_dependencies.iter() {
            if ctx.no_uses {
                self.excludes(&dependency.name);
            } else {
                self.matching(&dependency.name);
            }
        }
        if ctx.no_uses {
            return Ok(());
        }
        let mut visited = BTreeSet::new();
        for root in roots {
            self.visit(ctx, root, &mut vec![root.name.clone()], &mut visited)?;
        }
        Ok(())
    }

    fn visit(
        &mut self,
        ctx: &Context,
        overlay: &Overlay,
        chain: &mut Vec<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<()> {
        if !visited.insert(overlay.name.clone()) {
            return Ok(());
        }
        for name in overlay.uses.iter().flatten() {
            let name = ctx.repository.canonical_name(name)?;
            chain.push(name.clone());
            if let Some(pattern) = self.matching(&name) {
                bail!(
                    "overlay '{}' requires skipped overlay '{name}' via {}; matched --skip '{pattern}'; remove the exclusion, select different roots, or use --no-uses for metadata-only dependencies",
                    overlay.name,
                    chain.join(" -> ")
                );
            }
            self.visit(ctx, &ctx.resolved_overlay(&name)?, chain, visited)?;
            chain.pop();
        }
        Ok(())
    }

    pub(crate) fn report(&self) -> Result<()> {
        for name in &self.skipped {
            ui::info(format!("Skipped overlay '{name}' (--skip)"))?;
        }
        for (index, (pattern, _)) in self.patterns.iter().enumerate() {
            if !self.matched.contains(&index) {
                ui::warn(format!(
                    "--skip '{pattern}' matched no overlay in this apply selection"
                ))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::Exclusions;

    #[rstest]
    #[case("neovim", "neovim", true)]
    #[case("neovim", "neovim/plugins", false)]
    #[case("neovim", "Neovim", false)]
    #[case("workspace/*", "workspace/private", true)]
    #[case("workspace/*", "workspace/team/private", false)]
    #[case("workspace/**", "workspace/team/private", true)]
    #[case("workspace/?", "workspace/x", true)]
    #[case("workspace/?", "workspace/xx", false)]
    #[case("{apps,workspace}/[ab]", "apps/a", true)]
    #[case(r"apps/\*", "apps/*", true)]
    fn patterns_match_whole_case_sensitive_canonical_names(
        #[case] pattern: &str,
        #[case] name: &str,
        #[case] expected: bool,
    ) {
        let mut exclusions = Exclusions::new(&[pattern.to_owned()]).unwrap();
        assert_eq!(exclusions.excludes(name), expected);
    }

    #[test]
    fn overlapping_patterns_are_each_matched_and_names_are_reported_once() {
        let mut exclusions = Exclusions::new(&["apps/*".into(), "apps/**".into()]).unwrap();
        assert!(exclusions.excludes("apps/a"));
        assert!(exclusions.excludes("apps/a"));
        assert_eq!(exclusions.matched.len(), 2);
        assert_eq!(exclusions.skipped.len(), 1);
    }
}
