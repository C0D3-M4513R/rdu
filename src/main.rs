use clap::Parser;
use humansize::format_size;
use humansize::FormatSizeOptions;
use std::{env, error::Error, fs::Metadata, io, path::PathBuf};
use std::collections::HashSet;
use std::io::ErrorKind;
use tokio::fs;
#[cfg(all(not(target_os = "hermit"), unix))]
use std::os::unix::fs::MetadataExt as MetadataExtUnix;

#[derive(Parser, Debug)]
#[command(author, version)]
/// Calculate space usage of a directory tree
pub struct Opts {
    /// Directory to start from (default = current directory)
    pub dir: Option<PathBuf>,
    #[clap(short, long)]
    pub human_readable: bool,
    // #[clap(short, long)]
    // pub summarize: bool,
}

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opts = Opts::parse();
    let start_dir = match opts.dir {
        Some(dir) => dir,
        _ => env::current_dir()?,
    };

    let usage = calc_space_usage(start_dir.clone()).await?;
    let human_usage = if opts.human_readable {
        format_size(usage, FormatSizeOptions::default())
    }else {
        usage.to_string()
    };

    println!("{}\t{}", human_usage, start_dir.display());
    Ok(())
}

async fn calc_space_usage(path: PathBuf) -> Result<u64, io::Error> {
    let mut meta_js:tokio::task::JoinSet<(PathBuf, std::io::Result<Metadata>)> = tokio::task::JoinSet::new();
    let mut hashmap:HashSet<(u64, u64)> = HashSet::new();
    meta_js.spawn(async{meta_with_path(path).await});
    let mut size = 0;

    while let Some(value) = meta_js.join_next().await {
        match value {
            Ok((path, Err(meta)))=>{
                match meta.kind(){
                    ErrorKind::NotFound => continue,
                    ErrorKind::OutOfMemory => {
                        meta_js.spawn(async{meta_with_path(path).await});
                        continue;
                    }
                    _ => return Err(meta),
                }
            },
            Ok((path, Ok(meta))) => {
                #[cfg(all(not(target_os = "hermit"), unix))]
                {
                    //if meta.nlink == 1, then we know we don't have multiple hardlinked files.
                    //in that case we don't even need to save that inode number.
                    if meta.nlink()>1 && !hashmap.insert((meta.dev(), meta.ino())) {continue};
                }
                let file_type = meta.file_type();

                if file_type.is_symlink() {
                    // don't follow symlinks
                }else if file_type.is_file() {
                    size += meta.len();
                } else if file_type.is_dir() {
                    let mut entries = fs::read_dir(&path).await?;
                    while let Some(entry) = entries.next_entry().await? {
                        meta_js.spawn(async move {meta_with_path(entry.path()).await});
                    }
                }
            },
            Err(err) => {
                panic!("{}", err)
            }
        }
    }

    Ok(size)
}

async fn meta_with_path(path: PathBuf) -> (PathBuf, io::Result<Metadata>) {
    let meta = fs::symlink_metadata(&path).await;
    (path, meta)
}
