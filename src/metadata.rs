//! Strict, deterministic metadata import and export workflows.

mod application;
mod format;
mod io;
mod planning;

pub use application::{
    apply_metadata_plan, apply_metadata_plan_with_report, AppliedOperation, ApplyFailure,
    ApplyReport, ApplyStatus,
};
pub use format::validate_metadata;
pub use io::{
    create_restricted_json, read_metadata, replace_restricted_json, write_metadata,
    write_metadata_with_options,
};
pub use planning::{
    plan_metadata_import, ImportConflict, ImportConflictKind, ImportPlan, PlannedChange,
};

pub const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_COLLECTIONS: usize = 10_000;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_ATTRIBUTES: usize = 1_024;
pub const MAX_LABEL_BYTES: usize = 16 * 1024;
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const MAX_ATTRIBUTE_KEY_BYTES: usize = 4 * 1024;
pub const MAX_ATTRIBUTE_VALUE_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests;
