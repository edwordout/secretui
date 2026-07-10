use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use secretui::metadata::{
    read_encrypted_backup, read_metadata, write_encrypted_backup, write_metadata,
};
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
    },
    /// Export encrypted backup including secret values.
    Backup {
        #[arg(long)]
        encrypted: PathBuf,
    },
    /// Restore encrypted backup. Requires --confirm-restore.
    Restore {
        #[arg(long)]
        encrypted: PathBuf,
        #[arg(long)]
        confirm_restore: bool,
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
        Some(Command::Import { metadata }) => {
            let data = read_metadata(&metadata)?;
            let changed = store.import_metadata(data).await?;
            println!("updated {changed} item(s)");
            Ok(())
        }
        Some(Command::Backup { encrypted }) => {
            let passphrase = read_passphrase_confirmed("backup passphrase")?;
            let data = store.export_secret_backup().await?;
            write_encrypted_backup(&encrypted, &data, passphrase)?;
            println!("wrote encrypted backup to {}", encrypted.display());
            Ok(())
        }
        Some(Command::Restore {
            encrypted,
            confirm_restore,
        }) => {
            if !confirm_restore {
                bail!("restore is destructive; rerun with --confirm-restore");
            }
            let passphrase =
                rpassword::prompt_password("backup passphrase: ").context("read passphrase")?;
            let data = read_encrypted_backup(&encrypted, passphrase)?;
            let changed = store.restore_secret_backup(data).await?;
            println!("restored {changed} item(s)");
            Ok(())
        }
        Some(Command::Completions { .. }) | Some(Command::Man) => {
            unreachable!("handled before store connection")
        }
    }
}

fn read_passphrase_confirmed(label: &str) -> Result<String> {
    let first = rpassword::prompt_password(format!("{label}: ")).context("read passphrase")?;
    let second = rpassword::prompt_password(format!("confirm {label}: "))
        .context("read passphrase confirmation")?;
    if first != second {
        bail!("passphrases did not match");
    }
    Ok(first)
}
