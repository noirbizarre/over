use std::collections::BTreeMap;
use std::path::Path;

use clap::Args;
use termtree::Tree;

use crate::cli::CLI;
use crate::overlays::Repository;
use anyhow::Result;

#[derive(Args, Debug)]
pub struct Params {
    #[clap(short, long, help = "Display as tree")]
    tree: bool,
}

pub async fn execute(cli: &CLI, args: &Params) -> Result<()> {
    if cli.debug {
        tracing::debug!(?cli, ?args, "list command");
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home.clone());
    let overlays = repo.overlays()?;

    if args.tree {
        let root_label = root_label(&home);
        let tree = build_tree(&root_label, overlays.iter().map(|o| o.name.as_str()));
        print!("{tree}");
    } else {
        for overlay in &overlays {
            println!("{}", overlay.name);
        }
    }

    Ok(())
}

/// Derive the root node label from the home path (its directory name).
fn root_label(home: &Path) -> String {
    home.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| home.display().to_string())
}

/// A node in the intermediate tree structure used to group overlay name segments.
#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
}

impl TreeNode {
    /// Insert path segments into the tree, creating intermediate nodes as needed.
    fn insert(&mut self, segments: &[&str]) {
        if segments.is_empty() {
            return;
        }
        let child = self.children.entry(segments[0].to_owned()).or_default();
        if segments.len() > 1 {
            child.insert(&segments[1..]);
        }
    }

    /// Convert this node into a `termtree::Tree` with the given label.
    fn into_tree(self, label: String) -> Tree<String> {
        let mut tree = Tree::new(label);
        for (name, child) in self.children {
            tree.push(child.into_tree(name));
        }
        tree
    }
}

/// Build a `termtree::Tree` from overlay names split on `/`.
///
/// Intermediate segments that are not themselves overlays become branch nodes.
/// Overlay names are inserted in sorted order because `BTreeMap` keeps keys sorted.
fn build_tree<'a>(root_label: &str, names: impl Iterator<Item = &'a str>) -> Tree<String> {
    let mut root = TreeNode::default();
    for name in names {
        root.insert(&name.split('/').collect::<Vec<_>>());
    }
    root.into_tree(root_label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use clap::Parser;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::PathBuf;

    fn make_cli(home: PathBuf) -> CLI {
        CLI::parse_from(vec!["over", "--home", home.to_str().unwrap()])
    }

    #[tokio::test]
    async fn list_empty_repository() {
        let tmp = TempDir::new().unwrap();
        let cli = make_cli(tmp.path().to_path_buf());
        let params = Params { tree: false };
        let result = execute(&cli, &params).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_single_overlay() {
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("myoverlay");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let params = Params { tree: false };
        let result = execute(&cli, &params).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_multiple_overlays() {
        let tmp = TempDir::new().unwrap();
        for name in ["alpha", "beta", "gamma"] {
            let ov = tmp.path().join(name);
            fs::create_dir_all(&ov).unwrap();
            fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();
        }

        let cli = make_cli(tmp.path().to_path_buf());
        let params = Params { tree: false };
        let result = execute(&cli, &params).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_with_tree_flag() {
        let tmp = TempDir::new().unwrap();
        let ov = tmp.path().join("treeov");
        fs::create_dir_all(&ov).unwrap();
        fs::write(ov.join("over.toml"), "target = \"~\"").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let params = Params { tree: true };
        let result = execute(&cli, &params).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn list_skips_badly_formatted_overlay() {
        let tmp = TempDir::new().unwrap();
        let good = tmp.path().join("good");
        fs::create_dir_all(&good).unwrap();
        fs::write(good.join("over.toml"), "target = \"~\"").unwrap();

        let bad = tmp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("over.toml"), "{{invalid}}").unwrap();

        let cli = make_cli(tmp.path().to_path_buf());
        let params = Params { tree: false };
        let result = execute(&cli, &params).await;
        assert!(result.is_ok());
    }

    #[test]
    fn build_tree_flat_overlays() {
        let tree = build_tree(".over", ["git", "shell", "vim"].into_iter());
        let output = tree.to_string();
        let expected = "\
.over
├── git
├── shell
└── vim
";
        assert_eq!(output, expected);
    }

    #[test]
    fn build_tree_nested_overlays() {
        let tree = build_tree(
            ".over",
            ["git", "shell/bash", "shell/zsh", "vim"].into_iter(),
        );
        let output = tree.to_string();
        let expected = "\
.over
├── git
├── shell
│   ├── bash
│   └── zsh
└── vim
";
        assert_eq!(output, expected);
    }

    #[test]
    fn build_tree_deeply_nested() {
        let tree = build_tree(
            ".over",
            ["apps/dev/editor", "apps/dev/terminal", "apps/browser"].into_iter(),
        );
        let output = tree.to_string();
        let expected = "\
.over
└── apps
    ├── browser
    └── dev
        ├── editor
        └── terminal
";
        assert_eq!(output, expected);
    }

    #[test]
    fn build_tree_empty() {
        let tree = build_tree(".over", std::iter::empty());
        let output = tree.to_string();
        assert_eq!(output, ".over\n");
    }

    #[test]
    fn build_tree_single_overlay() {
        let tree = build_tree(".over", ["solo"].into_iter());
        let output = tree.to_string();
        let expected = "\
.over
└── solo
";
        assert_eq!(output, expected);
    }

    #[test]
    fn root_label_uses_directory_name() {
        let label = root_label(Path::new("/home/user/.over"));
        assert_eq!(label, ".over");
    }

    #[test]
    fn root_label_falls_back_to_full_path() {
        let label = root_label(Path::new("/"));
        assert_eq!(label, "/");
    }
}
