use clap::Args;

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
        println!("{:#?}", cli);
        println!("{:#?}", args);
    }

    let home = cli.resolve_home()?;
    let repo = Repository::new(home);

    for overlay in repo.overlays()? {
        println!("{}", overlay.name);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use clap::Parser;
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
}

// use termtree::Tree;

// use std::path::Path;
// use std::{env, fs, io};

// fn label<P: AsRef<Path>>(p: P) -> String {
//     p.as_ref().file_name().unwrap().to_str().unwrap().to_owned()
// }

// fn tree<P: AsRef<Path>>(p: P) -> io::Result<Tree<String>> {
//     let result = fs::read_dir(&p)?.filter_map(|e| e.ok()).fold(
//         Tree::root(label(p.as_ref().canonicalize()?)),
//         |mut root, entry| {
//             let dir = entry.metadata().unwrap();
//             if dir.is_dir() {
//                 root.push(tree(entry.path()).unwrap());
//             } else {
//                 root.push(Tree::root(label(entry.path())));
//             }
//             root
//         },
//     );
//     Ok(result)
// }
