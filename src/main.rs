use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use secretui::domain::Attributes;
use secretui::metadata::{
    apply_metadata_plan_with_report, plan_metadata_import, read_metadata, write_metadata,
    write_metadata_with_options, ApplyReport, ApplyStatus, ImportPlan, PlannedChange,
};
use secretui::store::{SecretStore, StoreWarning};
use secretui::store_secret_service::SecretServiceStore;
use secretui::terminal;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    /// Export deterministic, secret-free version 2 metadata JSON.
    Export {
        #[arg(long)]
        metadata: PathBuf,
        /// Replace an existing non-directory path instead of refusing it.
        #[arg(long)]
        force: bool,
    },
    /// Plan or apply same-store metadata repair by exact object path.
    Import {
        #[arg(long)]
        metadata: PathBuf,
        /// Apply the displayed exact plan after recovery/report creation and a second preflight.
        #[arg(long)]
        apply: bool,
        /// No-clobber path for directly reusable original version 2 metadata.
        #[arg(long, requires = "apply")]
        recovery: Option<PathBuf>,
        /// No-clobber path for the durable per-operation JSON report.
        #[arg(long, requires = "apply")]
        report: Option<PathBuf>,
    },
    /// Generate shell completion script to stdout.
    Completions { shell: Shell },
    /// Generate a man page to stdout.
    Man,
}

#[tokio::main]
async fn main() {
    if let Err(error) = entry().await {
        eprintln!("error: {}", terminal::error(&format!("{error:#}")));
        std::process::exit(1);
    }
}

async fn entry() -> Result<()> {
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
            let is_tui = command.is_none();
            let operation = run_store_command(command, &store).await;
            if is_tui {
                return operation;
            }
            let cleanup = store.cleanup_temporary_unlocks().await;
            combine_operation_and_cleanup(operation, cleanup)
        }
    }
}

async fn run_store_command(command: Option<Command>, store: &impl SecretStore) -> Result<()> {
    match command {
        None => secretui::tui::run_tui(store).await,
        Some(Command::Export { metadata, force }) => {
            let exported = store
                .export_metadata()
                .await
                .context("export metadata without reading secrets")?;
            write_metadata_with_options(&metadata, &exported, force)?;
            println!("wrote secret-free metadata to {}", display_path(&metadata));
            Ok(())
        }
        Some(Command::Import {
            metadata,
            apply,
            recovery,
            report,
        }) => {
            let requested = read_metadata(&metadata)?;
            let plan = plan_metadata_import(store, &requested).await?;
            print_import_plan(&plan);
            if !apply {
                if plan.conflicts.is_empty() {
                    println!("preview only; rerun with --apply to consume this plan");
                    return Ok(());
                }
                return Err(anyhow!(
                    "same-store metadata repair is blocked by {} conflict(s); zero writes performed",
                    plan.conflicts.len()
                ));
            }

            let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let recovery_path = recovery.unwrap_or_else(|| {
                adjacent_journal_path(&metadata, "recovery", timestamp.as_str())
            });
            let report_path = report
                .unwrap_or_else(|| adjacent_journal_path(&metadata, "report", timestamp.as_str()));
            anyhow::ensure!(
                recovery_path != report_path,
                "recovery and report paths must be different"
            );

            write_metadata(&recovery_path, &plan.recovery).with_context(|| {
                format!(
                    "create no-clobber recovery metadata {}",
                    display_path(&recovery_path)
                )
            })?;
            println!(
                "created secret-free recovery metadata: {}",
                display_path(&recovery_path)
            );
            let apply_report =
                apply_metadata_plan_with_report(store, &plan, &report_path, Some(&recovery_path))
                    .await
                    .with_context(|| {
                        format!(
                            "apply exact metadata plan with report {}",
                            display_path(&report_path)
                        )
                    })?;
            print_apply_report(&apply_report);
            if !apply_report.is_complete() {
                return Err(anyhow!(match apply_report.status {
                    ApplyStatus::Partial => format!(
                        "metadata application was only partially completed: {} field operation(s) attempted and {} verified applied; a failed provider call may already have written metadata; no secret values were modified; review {}",
                        apply_report.field_operations_attempted,
                        apply_report.field_operations_applied,
                        display_path(&report_path)
                    ),
                    ApplyStatus::Blocked | ApplyStatus::InProgress => format!(
                        "metadata application was blocked; zero writes were performed; review {}",
                        display_path(&report_path)
                    ),
                    ApplyStatus::Complete => unreachable!(),
                }));
            }
            Ok(())
        }
        Some(Command::Completions { .. }) | Some(Command::Man) => {
            unreachable!("handled before store connection")
        }
    }
}

fn combine_operation_and_cleanup(
    operation: Result<()>,
    cleanup: Result<secretui::store::StoreOutcome<()>>,
) -> Result<()> {
    let cleanup_failure = match cleanup {
        Ok(outcome) if outcome.warnings.is_empty() => None,
        Ok(outcome) => Some(format_store_warnings(&outcome.warnings)),
        Err(error) => Some(format!("{error:#}")),
    };
    match (operation, cleanup_failure) {
        (Ok(()), None) => Ok(()),
        (Err(error), None) => Err(error),
        (Ok(()), Some(cleanup_error)) => Err(anyhow!(
            "operation finished, but temporary-unlock cleanup failed: {cleanup_error}"
        )),
        (Err(operation_error), Some(cleanup_error)) => Err(anyhow!(
            "operation failed: {operation_error:#}; temporary-unlock cleanup also failed: {cleanup_error}"
        )),
    }
}

fn format_store_warnings(warnings: &[StoreWarning]) -> String {
    warnings
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn adjacent_journal_path(metadata_path: &Path, kind: &str, timestamp: &str) -> PathBuf {
    let parent = metadata_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut name = metadata_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("metadata.json"));
    name.push(format!(".secretui-{kind}-{timestamp}.json"));
    parent.join(name)
}

fn print_import_plan(plan: &ImportPlan) {
    println!("same-store metadata repair plan (matching: exact provider object paths)");
    for change in &plan.changes {
        print_planned_change(change);
    }
    if plan.changes.is_empty() {
        println!("  no metadata field changes planned");
    }
    for conflict in &plan.conflicts {
        let item = conflict
            .item_path
            .as_deref()
            .map(|path| format!(" item={}", terminal::path(path)))
            .unwrap_or_default();
        println!(
            "  CONFLICT {:?}: collection={}{}: {}",
            conflict.kind,
            terminal::path(&conflict.collection_path),
            item,
            terminal::error(&conflict.message)
        );
    }
    println!(
        "summary: {} target change(s), {} unchanged field operation(s), {} conflict(s)",
        plan.changes.len(),
        plan.field_operations_skipped,
        plan.conflicts.len()
    );
}

fn print_planned_change(change: &PlannedChange) {
    match &change.item_path {
        None => println!(
            "  COLLECTION {} label: “{}” -> “{}” (exact path)",
            terminal::path(&change.collection_path),
            terminal::label(&change.current_label),
            terminal::label(change.proposed_label.as_deref().unwrap_or(""))
        ),
        Some(item_path) => {
            println!(
                "  ITEM collection={} path={} (exact path and parent)",
                terminal::path(&change.collection_path),
                terminal::path(item_path)
            );
            if let Some(proposed_label) = &change.proposed_label {
                println!(
                    "    label: “{}” -> “{}”",
                    terminal::label(&change.current_label),
                    terminal::label(proposed_label)
                );
            }
            if let Some(proposed_attributes) = &change.proposed_attributes {
                println!(
                    "    attributes: {} -> {}",
                    render_attributes(
                        change
                            .current_attributes
                            .as_ref()
                            .unwrap_or(&Attributes::new())
                    ),
                    render_attributes(proposed_attributes)
                );
            }
        }
    }
}

fn render_attributes(attributes: &Attributes) -> String {
    let mut rendered = attributes
        .iter()
        .take(terminal::DISPLAYED_ATTRIBUTE_LIMIT)
        .map(|(key, value)| {
            format!(
                "“{}”=“{}”",
                terminal::attribute_key(key),
                terminal::attribute_value(value)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    if attributes.len() > terminal::DISPLAYED_ATTRIBUTE_LIMIT {
        rendered.push_str(&format!(
            ", … {} attribute(s) omitted",
            attributes.len() - terminal::DISPLAYED_ATTRIBUTE_LIMIT
        ));
    }
    format!("{{{rendered}}}")
}

fn print_apply_report(report: &ApplyReport) {
    println!(
        "apply status: {:?}; collections changed: {}; items changed: {}; fields attempted: {}; fields verified applied: {}; fields skipped: {}",
        report.status,
        report.collections_changed,
        report.items_changed,
        report.field_operations_attempted,
        report.field_operations_applied,
        report.field_operations_skipped
    );
    for conflict in &report.conflicts {
        println!(
            "  conflict {:?} on {}: {}",
            conflict.kind,
            display_report_target(&conflict.collection_path, conflict.item_path.as_deref()),
            terminal::error(&conflict.message)
        );
    }
    for failure in &report.failures {
        println!(
            "  failure on {} field={}: {}",
            display_report_target(&failure.collection_path, failure.item_path.as_deref()),
            terminal::terminal_safe(&failure.field, 64),
            terminal::error(&failure.error)
        );
    }
    for failure in &report.relock_failures {
        println!("  RELOCK FAILURE: {}", terminal::error(failure));
    }
    if let Some(path) = &report.report_file {
        println!("durable report: {}", display_path(path));
    }
}

fn display_report_target(collection_path: &str, item_path: Option<&str>) -> String {
    if collection_path.is_empty() {
        return "preflight/report journal".into();
    }
    match item_path {
        Some(item_path) => format!(
            "collection={} item={}",
            terminal::path(collection_path),
            terminal::path(item_path)
        ),
        None => format!("collection={}", terminal::path(collection_path)),
    }
}

fn display_path(path: &Path) -> String {
    terminal::path(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secretui::domain::{
        CollectionInfo, CollectionMetadata, ItemInfo, ItemMetadata, MetadataFile,
    };
    use secretui::store::MemorySecretStore;

    #[test]
    fn journal_names_are_beside_input_and_deterministic() {
        assert_eq!(
            adjacent_journal_path(
                Path::new("/tmp/backup.json"),
                "recovery",
                "20260711T120000Z"
            ),
            Path::new("/tmp/backup.json.secretui-recovery-20260711T120000Z.json")
        );
    }

    #[test]
    fn rendered_attributes_escape_controls() {
        let attributes = Attributes::from([("key\x1b".into(), "line\nvalue".into())]);
        let rendered = render_attributes(&attributes);
        assert!(rendered.contains("\\x1b"));
        assert!(rendered.contains("\\n"));
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\n'));
    }

    fn command_store() -> MemorySecretStore {
        let store = MemorySecretStore::new();
        store.insert_collection(CollectionInfo {
            path: "/collection/cli".into(),
            label: "Current".into(),
            locked: false,
        });
        store
            .insert_item(
                ItemInfo {
                    collection_path: "/collection/cli".into(),
                    path: "/collection/cli/item".into(),
                    label: "Item".into(),
                    locked: false,
                    attributes: Attributes::new(),
                    created: Some(1),
                    modified: Some(1),
                },
                vec![0, 0xff],
                "application/octet-stream",
            )
            .unwrap();
        store
    }

    #[tokio::test]
    async fn metadata_cli_export_and_apply_smoke_test() {
        let directory = tempfile::tempdir().unwrap();
        let export_path = directory.path().join("export.json");
        let store = command_store();
        run_store_command(
            Some(Command::Export {
                metadata: export_path.clone(),
                force: false,
            }),
            &store,
        )
        .await
        .unwrap();
        let exported = read_metadata(&export_path).unwrap();
        assert_eq!(exported.version, 2);

        let repair_path = directory.path().join("repair.json");
        let recovery_path = directory.path().join("recovery.json");
        let report_path = directory.path().join("report.json");
        let repair = MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: "/collection/cli".into(),
                label: "Repaired".into(),
                locked: false,
                items: vec![ItemMetadata {
                    path: "/collection/cli/item".into(),
                    label: "Item".into(),
                    locked: false,
                    attributes: Attributes::new(),
                    created: Some(1),
                    modified: Some(1),
                }],
            }],
        };
        write_metadata(&repair_path, &repair).unwrap();
        run_store_command(
            Some(Command::Import {
                metadata: repair_path,
                apply: true,
                recovery: Some(recovery_path.clone()),
                report: Some(report_path.clone()),
            }),
            &store,
        )
        .await
        .unwrap();
        assert_eq!(
            store.collection("/collection/cli").unwrap().label,
            "Repaired"
        );
        assert_eq!(read_metadata(&recovery_path).unwrap().version, 2);
        let report: ApplyReport =
            serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
        assert!(report.is_complete());
    }
}
