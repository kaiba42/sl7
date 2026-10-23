use clap::{Parser, Subcommand};
use sl7::Store;
use std::{io::Write, path::PathBuf};
#[derive(Parser)]
#[command(version, about = "Shared artifact storage prototype")]
struct Cli {
    #[arg(long, global = true, default_value = ".sl7")]
    store: PathBuf,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Initialize a store; destination must not exist.
    Init,
    /// Import one quiescent file under a unique view/name pair.
    Put {
        view: String,
        name: String,
        source: PathBuf,
    },
    /// Verify and export a file, without overwriting the destination.
    Get {
        view: String,
        name: String,
        destination: PathBuf,
    },
    /// Reconstruct bytes to stdout (maximum 16 MiB per request).
    Read {
        view: String,
        name: String,
        #[arg(long)]
        offset: u64,
        #[arg(long)]
        length: usize,
    },
    List,
    Stats,
    /// Verify catalog, packs, every chunk and full-file checksums.
    Check,
    /// Check native/mounted directory behavior using isolated temporary files.
    Probe {
        directory: PathBuf,
    },
    /// Serve immutable artifacts at <directory>/<view>/<name> in the foreground.
    #[cfg(feature = "fuse")]
    MountReadOnly {
        directory: PathBuf,
    },
    /// Linux-only writable adapter spike over an exclusively owned native tree.
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    MountStaging {
        backing: PathBuf,
        directory: PathBuf,
    },
    /// Mount a writable tree backed by deduplicated chunks (Linux).
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    Mount {
        directory: PathBuf,
    },
    /// Report live native allocation and shared chunk storage; unmount first.
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    LiveStats,
    /// Collect unreachable live files and compact packs; unmount first.
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    Gc,
}
fn json(value: &impl serde::Serialize) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => json(&Store::init(&cli.store)?.stats()?)?,
        Commands::Put { view, name, source } => {
            json(&Store::open(&cli.store)?.put(&view, &name, &source)?)?
        }
        Commands::Get {
            view,
            name,
            destination,
        } => Store::open(&cli.store)?.export(&view, &name, &destination)?,
        Commands::Read {
            view,
            name,
            offset,
            length,
        } => std::io::stdout()
            .lock()
            .write_all(&Store::open(&cli.store)?.read(&view, &name, offset, length)?)?,
        Commands::List => json(&Store::open(&cli.store)?.list()?)?,
        Commands::Stats => json(&Store::open(&cli.store)?.stats()?)?,
        Commands::Check => json(&Store::open(&cli.store)?.check()?)?,
        Commands::Probe { directory } => json(&sl7::probe::run(&directory)?)?,
        #[cfg(feature = "fuse")]
        Commands::MountReadOnly { directory } => sl7::readonly::mount(&cli.store, &directory)?,
        #[cfg(all(feature = "fuse", target_os = "linux"))]
        Commands::MountStaging { backing, directory } => sl7::staging::mount(&backing, &directory)?,
        #[cfg(all(feature = "fuse", target_os = "linux"))]
        Commands::Mount { directory } => sl7::staging::mount_store(&cli.store, &directory)?,
        #[cfg(all(feature = "fuse", target_os = "linux"))]
        Commands::LiveStats => json(&sl7::live::Live::open(Store::open(&cli.store)?)?.stats()?)?,
        #[cfg(all(feature = "fuse", target_os = "linux"))]
        Commands::Gc => json(&sl7::live::Live::open(Store::open(&cli.store)?)?.gc()?)?,
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("sl7: {error}");
        std::process::exit(1);
    }
}
