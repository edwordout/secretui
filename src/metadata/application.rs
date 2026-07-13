use super::io::{create_restricted_json, read_metadata, replace_restricted_json};
use super::planning::{
    plan_metadata_import, ImportConflict, ImportConflictKind, ImportPlan, PlannedChange,
};
use crate::domain::ItemInfo;
use crate::store::{ItemTarget, SecretStore, StoreError, StoreWarning};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    InProgress,
    Complete,
    Blocked,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedOperation {
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyFailure {
    pub collection_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_path: Option<String>,
    pub field: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyReport {
    pub status: ApplyStatus,
    pub collections_changed: usize,
    pub items_changed: usize,
    pub field_operations_attempted: usize,
    pub field_operations_applied: usize,
    pub field_operations_skipped: usize,
    pub conflicts: Vec<ImportConflict>,
    pub failures: Vec<ApplyFailure>,
    pub relock_failures: Vec<String>,
    pub operations: Vec<AppliedOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_file: Option<PathBuf>,
}

impl ApplyReport {
    pub fn is_complete(&self) -> bool {
        self.status == ApplyStatus::Complete
    }

    pub fn is_partial(&self) -> bool {
        self.status == ApplyStatus::Partial
    }

    fn new(plan: &ImportPlan) -> Self {
        Self {
            status: ApplyStatus::InProgress,
            collections_changed: 0,
            items_changed: 0,
            field_operations_attempted: 0,
            field_operations_applied: 0,
            field_operations_skipped: plan.field_operations_skipped,
            conflicts: Vec::new(),
            failures: Vec::new(),
            relock_failures: Vec::new(),
            operations: Vec::new(),
            recovery_file: None,
            report_file: None,
        }
    }

    fn finish_failed(&mut self) {
        self.status = if self.field_operations_attempted == 0 {
            ApplyStatus::Blocked
        } else {
            ApplyStatus::Partial
        };
    }
}

/// Apply an exact plan after repeating the full preflight.
pub async fn apply_metadata_plan(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
) -> Result<ApplyReport> {
    apply_metadata_plan_inner(store, plan, None, None).await
}

/// Apply a plan while durably replacing a JSON report after every field write.
pub async fn apply_metadata_plan_with_report(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
    report_path: &Path,
    recovery_path: Option<&Path>,
) -> Result<ApplyReport> {
    apply_metadata_plan_inner(store, plan, Some(report_path), recovery_path).await
}

async fn apply_metadata_plan_inner(
    store: &(impl SecretStore + ?Sized),
    plan: &ImportPlan,
    report_path: Option<&Path>,
    recovery_path: Option<&Path>,
) -> Result<ApplyReport> {
    if let Some(path) = recovery_path {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect recovery metadata {}", path.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "recovery metadata must be an existing regular, non-symlink file: {}",
            path.display()
        );
        let recovery = read_metadata(path)
            .with_context(|| format!("verify recovery metadata {}", path.display()))?;
        anyhow::ensure!(
            recovery == plan.recovery.clone().sorted(),
            "recovery metadata does not contain this exact plan's original values"
        );
    }
    let mut report = ApplyReport::new(plan);
    report.report_file = report_path.map(Path::to_path_buf);
    report.recovery_file = recovery_path.map(Path::to_path_buf);
    if let Some(path) = report_path {
        create_restricted_json(path, &report)
            .with_context(|| format!("create apply report {}", path.display()))?;
    }

    if !plan.conflicts.is_empty() {
        report.conflicts = plan.conflicts.clone();
        report.finish_failed();
        persist_report(report_path, &mut report);
        return Ok(report);
    }

    let fresh_plan = match plan_metadata_import(store, &plan.requested).await {
        Ok(fresh_plan) => fresh_plan,
        Err(error) => {
            report.failures.push(ApplyFailure {
                collection_path: String::new(),
                item_path: None,
                field: "preflight".into(),
                error: format!("second full preflight failed: {error:#}"),
            });
            report.finish_failed();
            persist_report(report_path, &mut report);
            return Ok(report);
        }
    };
    if fresh_plan != *plan {
        report.conflicts = second_preflight_conflicts(plan, &fresh_plan);
        report.finish_failed();
        persist_report(report_path, &mut report);
        return Ok(report);
    }

    for change in &plan.changes {
        if let Err(error) = verify_change_target(store, change).await {
            report.conflicts.push(ImportConflict {
                kind: ImportConflictKind::ConcurrentChange,
                collection_path: change.collection_path.clone(),
                item_path: change.item_path.clone(),
                message: error.to_string(),
            });
            report.finish_failed();
            persist_report(report_path, &mut report);
            return Ok(report);
        }

        if change.is_collection() {
            let proposed_label = change
                .proposed_label
                .as_deref()
                .expect("collection changes always contain a proposed label");
            report.field_operations_attempted += 1;
            match store
                .set_collection_label(&change.collection_path, proposed_label)
                .await
            {
                Ok(outcome) => {
                    report.collections_changed += 1;
                    record_applied(&mut report, change, "label");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "label", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
            continue;
        }

        let target = ItemTarget {
            collection_path: change.collection_path.clone(),
            item_path: change.item_path.clone().expect("item path was checked"),
        };
        let mut item_counted = false;
        let mut post_label_snapshot = None;
        if let Some(proposed_label) = change.proposed_label.as_deref() {
            report.field_operations_attempted += 1;
            match store.set_item_label(&target, proposed_label).await {
                Ok(outcome) => {
                    item_counted = true;
                    report.items_changed += 1;
                    record_applied(&mut report, change, "label");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "label", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
            if change.proposed_attributes.is_some() {
                match read_after_own_label(store, change).await {
                    Ok(snapshot) => post_label_snapshot = Some(snapshot),
                    Err(error) => {
                        record_failure(&mut report, change, "post_label_verification", error);
                        report.finish_failed();
                        persist_report(report_path, &mut report);
                        return Ok(report);
                    }
                }
            }
        }

        if let Some(proposed_attributes) = &change.proposed_attributes {
            if let Some(post_label_snapshot) = post_label_snapshot.as_ref() {
                if let Err(error) = verify_unchanged_item(store, change, post_label_snapshot).await
                {
                    report.conflicts.push(ImportConflict {
                        kind: ImportConflictKind::ConcurrentChange,
                        collection_path: change.collection_path.clone(),
                        item_path: change.item_path.clone(),
                        message: error.to_string(),
                    });
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            report.field_operations_attempted += 1;
            match store
                .set_item_attributes(&target, proposed_attributes.clone())
                .await
            {
                Ok(outcome) => {
                    if !item_counted {
                        report.items_changed += 1;
                    }
                    record_applied(&mut report, change, "attributes");
                    record_warnings(&mut report, &outcome.warnings);
                }
                Err(error) => {
                    record_failure(&mut report, change, "attributes", error);
                    report.finish_failed();
                    persist_report(report_path, &mut report);
                    return Ok(report);
                }
            }
            if !persist_report(report_path, &mut report) {
                return Ok(report);
            }
            if !report.relock_failures.is_empty() {
                report.finish_failed();
                persist_report(report_path, &mut report);
                return Ok(report);
            }
        }
    }

    report.status = ApplyStatus::Complete;
    persist_report(report_path, &mut report);
    Ok(report)
}

fn second_preflight_conflicts(plan: &ImportPlan, fresh_plan: &ImportPlan) -> Vec<ImportConflict> {
    if !fresh_plan.conflicts.is_empty() {
        return fresh_plan
            .conflicts
            .iter()
            .cloned()
            .map(|mut conflict| {
                conflict.message = format!("second preflight: {}", conflict.message);
                conflict
            })
            .collect();
    }

    let old_collections = plan
        .baseline
        .collections
        .iter()
        .map(|collection| (collection.path.as_str(), collection))
        .collect::<BTreeMap<_, _>>();
    let fresh_collections = fresh_plan
        .baseline
        .collections
        .iter()
        .map(|collection| (collection.path.as_str(), collection))
        .collect::<BTreeMap<_, _>>();
    let mut conflicts = Vec::new();
    for requested_collection in &plan.requested.collections {
        let old_collection = old_collections
            .get(requested_collection.path.as_str())
            .copied();
        let fresh_collection = fresh_collections
            .get(requested_collection.path.as_str())
            .copied();
        if old_collection.map(|collection| (&collection.label, collection.locked))
            != fresh_collection.map(|collection| (&collection.label, collection.locked))
        {
            conflicts.push(ImportConflict {
                kind: ImportConflictKind::ConcurrentChange,
                collection_path: requested_collection.path.clone(),
                item_path: None,
                message: "collection label, lock state, or identity changed after planning".into(),
            });
        }

        let old_items = old_collection
            .map(|collection| {
                collection
                    .items
                    .iter()
                    .map(|item| (item.path.as_str(), item))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let fresh_items = fresh_collection
            .map(|collection| {
                collection
                    .items
                    .iter()
                    .map(|item| (item.path.as_str(), item))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        for requested_item in &requested_collection.items {
            if old_items.get(requested_item.path.as_str())
                != fresh_items.get(requested_item.path.as_str())
            {
                conflicts.push(ImportConflict {
                    kind: ImportConflictKind::ConcurrentChange,
                    collection_path: requested_collection.path.clone(),
                    item_path: Some(requested_item.path.clone()),
                    message: "item label, attributes, lock state, identity, or timestamp changed after planning"
                        .into(),
                });
            }
        }
    }
    if conflicts.is_empty() {
        conflicts.push(ImportConflict {
            kind: ImportConflictKind::ConcurrentChange,
            collection_path: String::new(),
            item_path: None,
            message: "the exact plan changed during the second full preflight".into(),
        });
    }
    conflicts
}

async fn verify_change_target(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<()> {
    if change.is_collection() {
        let matching = store
            .list_collections()
            .await
            .context("re-read collections immediately before write")?
            .into_iter()
            .filter(|collection| collection.path == change.collection_path)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matching.len() == 1,
            "collection identity is no longer unique"
        );
        let current = &matching[0];
        anyhow::ensure!(
            current.label == change.current_label && current.locked == change.locked,
            "collection label or lock state changed after preflight"
        );
        return Ok(());
    }

    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        current.label == change.current_label
            && Some(&current.attributes) == change.current_attributes.as_ref()
            && current.locked == change.locked
            && current.created == change.created
            && current.modified == change.modified,
        "item identity, label, attributes, lock state, creation, or modification timestamp changed after preflight"
    );
    Ok(())
}

async fn read_after_own_label(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<ItemInfo> {
    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        Some(current.label.as_str()) == change.proposed_label.as_deref()
            && Some(&current.attributes) == change.current_attributes.as_ref()
            && current.locked == change.locked
            && current.created == change.created,
        "item changed concurrently between its label and attribute operations"
    );
    Ok(current)
}

async fn verify_unchanged_item(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
    expected: &ItemInfo,
) -> Result<()> {
    let current = read_exact_item(store, change).await?;
    anyhow::ensure!(
        &current == expected,
        "item label, attributes, lock state, identity, or timestamp changed between field operations"
    );
    Ok(())
}

async fn read_exact_item(
    store: &(impl SecretStore + ?Sized),
    change: &PlannedChange,
) -> Result<ItemInfo> {
    let item_path = change.item_path.as_deref().context("missing item path")?;
    let matching = store
        .list_items(&change.collection_path)
        .await
        .context("re-read collection immediately before item write")?
        .into_iter()
        .filter(|item| item.path == item_path && item.collection_path == change.collection_path)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        matching.len() == 1,
        "item identity or parent collection changed"
    );
    Ok(matching.into_iter().next().expect("length checked"))
}

fn record_applied(report: &mut ApplyReport, change: &PlannedChange, field: &str) {
    report.field_operations_applied += 1;
    report.operations.push(AppliedOperation {
        collection_path: change.collection_path.clone(),
        item_path: change.item_path.clone(),
        field: field.into(),
    });
}

fn record_failure(
    report: &mut ApplyReport,
    change: &PlannedChange,
    field: &str,
    error: anyhow::Error,
) {
    let error_text = if let Some(store_error) = error.downcast_ref::<StoreError>() {
        record_warnings(report, &store_error.warnings);
        store_error.operation_error.clone()
    } else {
        format!("{error:#}")
    };
    report.failures.push(ApplyFailure {
        collection_path: change.collection_path.clone(),
        item_path: change.item_path.clone(),
        field: field.into(),
        error: error_text,
    });
}

fn record_warnings(report: &mut ApplyReport, warnings: &[StoreWarning]) {
    report
        .relock_failures
        .extend(warnings.iter().map(ToString::to_string));
}

fn persist_report(path: Option<&Path>, report: &mut ApplyReport) -> bool {
    let Some(path) = path else {
        return true;
    };
    if let Err(error) = replace_restricted_json(path, report) {
        report.failures.push(ApplyFailure {
            collection_path: String::new(),
            item_path: None,
            field: "report_journal".into(),
            error: format!("durable apply report update failed: {error:#}"),
        });
        report.finish_failed();
        return false;
    }
    true
}
