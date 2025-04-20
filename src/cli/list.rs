use std::path::PathBuf;

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

    // let repo = Repository::new( PathBuf::from(&cli.home) );

    let home = cli.home.as_ref().expect("Home directory not set");
    let repo = Repository::new(PathBuf::from(home));

    for overlay in repo.overlays()? {
        println!("{}", overlay.name);
    }

    Ok(())
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
