use clap::Parser;
use humansize::format_size;
use humansize::FormatSizeOptions;
use std::{env, error::Error, fs::Metadata, io, path::PathBuf};
use std::collections::HashSet;
use std::io::ErrorKind;
use tokio::fs;
#[cfg(all(not(target_os = "hermit"), unix))]
use std::os::unix::fs::MetadataExt as MetadataExtUnix;
use std::sync::Arc;

#[derive(Parser, Debug, Clone)]
#[command(author, version)]
/// Calculate space usage of a directory tree
pub struct Opts {
    /// Directory to start from (default = current directory)
    pub dir: Option<PathBuf>,
    #[clap(short, long)]
    pub human_readable: bool,
    #[clap(short, long)]
    pub ignore_hardlinks: bool,
    #[clap(short, long)]
    pub follow_symlinks: bool,
    // #[clap(short, long)]
    // pub summarize: bool,
}

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opts = Opts::parse();
    let start_dir = match opts.dir.as_ref() {
        Some(dir) => dir.clone(),
        _ => env::current_dir()?,
    };

    let usage = calc_space_usage(start_dir.clone(), opts.clone()).await?;
    let human_usage = if opts.human_readable {
        format_size(usage, FormatSizeOptions::default())
    }else {
        usage.to_string()
    };

    println!("{}\t{}", human_usage, start_dir.display());
    Ok(())
}

async fn calc_space_usage(path: PathBuf, opts: Opts) -> Result<u64, io::Error> {
    let meta_js:Arc<tokio::sync::Mutex<tokio::task::JoinSet<(PathBuf, std::io::Result<Metadata>)>>> = Arc::new(tokio::sync::Mutex::const_new(tokio::task::JoinSet::new()));
    let mut dir_js:tokio::task::JoinSet<(PathBuf, std::io::Result<()>)> = tokio::task::JoinSet::new();
    let mut hashmap:HashSet<(u64, u64)> = HashSet::new();
    meta_js.lock().await.spawn(async{meta_with_path(path).await});
    let mut size = 0;

    loop {
        while let Some(value) = std::future::poll_fn(|ctx|dir_js.poll_join_next(ctx)).await {
            match value {
                Err(err) => panic!("{}", err),
                Ok((_, Ok(()))) => {},
                Ok((path, Err(err))) => {
                    handle_err(path, &meta_js, err).await?
                },
            }
        }
        if meta_js.lock().await.is_empty() {
            match dir_js.join_next().await {
                None => break,
                Some(Err(err)) => panic!("{}", err),
                Some(Ok((_, Ok(())))) => {},
                Some(Ok((path, Err(err)))) => {
                    handle_err(path, &meta_js, err).await?
                },
            }
        }
        match meta_js.lock().await.join_next().await {
            None => break,
            Some(Ok((path, Err(meta)))) => {
                handle_err(path, &meta_js, meta).await?;
            },
            Some(Ok((path, Ok(meta)))) => {
                #[cfg(all(not(target_os = "hermit"), unix))]
                {
                    //we only track hardlinks, if !opts.ignore_hardlinks. If opts.ignore_hardlinks, we just count them duplicate.
                    //
                    //if meta.nlink == 1, then we know we don't have multiple hardlinked files.
                    //in that case we don't even need to save that inode number.
                    if !opts.ignore_hardlinks && meta.nlink() > 1 && !hashmap.insert((meta.dev(), meta.ino())) { continue };
                }
                let file_type = meta.file_type();

                if file_type.is_file() {
                    size += meta.len();
                } else if file_type.is_dir() {
                    let meta_js = meta_js.clone();
                    dir_js.spawn(async move {
                        let path_clone = path.clone();
                        let inner = || async {
                            let mut entries = fs::read_dir(path_clone).await?;
                            let mut vec = Vec::new();
                            while let Some(entry) = entries.next_entry().await? {
                                vec.push(async move { meta_with_path(entry.path()).await });
                            }
                            let mut mutex = meta_js.lock().await;
                            for fut in vec {
                                mutex.spawn(fut);
                            }
                            drop(mutex);
                            Ok::<(), std::io::Error>(())
                        };
                        (path, inner().await)
                    });
                } else if opts.follow_symlinks && file_type.is_symlink() {
                    meta_js.lock().await.spawn(async move {
                        let meta = fs::metadata(&path).await;
                        (path, meta)
                    });
                } else if !opts.follow_symlinks && file_type.is_symlink() {
                    // don't follow symlinks
                }
            },
            Some(Err(err)) => {
                panic!("{}", err)
            }
        }
    }

    Ok(size)
}

async fn handle_err(path: PathBuf, meta_js:&tokio::sync::Mutex<tokio::task::JoinSet<(PathBuf, std::io::Result<Metadata>)>>, err: std::io::Error) -> std::io::Result<()>{
    match err.kind(){
        ErrorKind::NotFound => Ok(()),
        ErrorKind::OutOfMemory => {
            meta_js.lock().await.spawn(async{meta_with_path(path).await});
            Ok(())
        },
        _ => return Err(err),
    }
}

async fn meta_with_path(path: PathBuf) -> (PathBuf, io::Result<Metadata>) {
    let meta = fs::symlink_metadata(&path).await;
    (path, meta)
}
