use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use secretui::metadata::{read_metadata, write_metadata};
use secretui::store::SecretStore;
use secretui::store_secret_service::SecretServiceStore;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "secretui",
    version,
    about = "Safe TUI/CLI for Freedesktop Secret Service"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Export metadata-only deterministic JSON.
    Export {
        #[arg(long)]
        metadata: PathBuf,
    },
    /// Import metadata-only JSON, updating matching object paths only.
    Import {
        #[arg(long)]
        metadata: PathBuf,
        /// Apply changes. Without this flag, only show the import plan.
        #[arg(long)]
        apply: bool,
    },
    /// Generate shell completion script to stdout.
    Completions { shell: Shell },
    /// Generate a man page to stdout.
    Man,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Completions { shell }) => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "secretui", &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Man) => {
            let man = clap_mangen::Man::new(Cli::command());
            man.render(&mut std::io::stdout())?;
            Ok(())
        }
        command => {
            let store = SecretServiceStore::connect().await?;
            run_store_command(command, &store).await
        }
    }
}

async fn run_store_command(command: Option<Command>, store: &impl SecretStore) -> Result<()> {
    match command {
        None => secretui::tui::run_tui(store).await,
        Some(Command::Export { metadata }) => {
            let data = store.export_metadata().await?;
            write_metadata(&metadata, &data)?;
            println!("wrote metadata to {}", metadata.display());
            Ok(())
        }
        Some(Command::Import { metadata, apply }) => {
            let data = read_metadata(&metadata)?;
            if !apply {
                let summary = store.preview_metadata_import(&data).await?;
                println!(
                    "preview: {} collection(s) and {} item(s) would change; {} path(s) missing; rerun with --apply",
                    summary.collections_changed, summary.items_changed, summary.paths_missing
                );
                return Ok(());
            }
            let changed = store.import_metadata(data).await?;
            println!("updated {changed} item(s)");
            Ok(())
        }
        Some(Command::Completions { .. }) | Some(Command::Man) => {
            unreachable!("handled before store connection")
        }
    }
}
