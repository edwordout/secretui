use crate::domain::MetadataFile;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use tempfile::NamedTempFile;

pub fn write_metadata(path: &Path, metadata: &MetadataFile) -> Result<()> {
    let mut text = serde_json::to_string_pretty(&metadata.clone().sorted())?;
    text.push('\n');
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary file in {}", parent.display()))?;
    use std::io::Write;
    temp.write_all(text.as_bytes())
        .with_context(|| format!("write temporary metadata for {}", path.display()))?;
    temp.as_file().sync_all().context("sync metadata")?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

pub fn read_metadata(path: &Path) -> Result<MetadataFile> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let metadata: MetadataFile = serde_json::from_str(&text).context("parse metadata json")?;
    anyhow::ensure!(
        matches!(metadata.version, 1 | 2),
        "unsupported metadata version {}",
        metadata.version
    );
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CollectionMetadata, ItemMetadata};
    use std::collections::BTreeMap;

    #[test]
    fn metadata_json_is_deterministic() {
        let metadata = MetadataFile {
            version: 2,
            collections: vec![CollectionMetadata {
                path: "b".into(),
                label: "B".into(),
                locked: false,
                items: vec![
                    ItemMetadata {
                        path: "z".into(),
                        label: "Z".into(),
                        locked: false,
                        attributes: BTreeMap::new(),
                        created: None,
                        modified: None,
                    },
                    ItemMetadata {
                        path: "a".into(),
                        label: "A".into(),
                        locked: false,
                        attributes: BTreeMap::new(),
                        created: None,
                        modified: None,
                    },
                ],
            }],
        };
        let json = serde_json::to_string_pretty(&metadata.sorted()).unwrap();
        assert!(json.find("\"a\"").unwrap() < json.find("\"z\"").unwrap());
    }

    #[test]
    fn reads_v1_and_ignores_content_type() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"{"version":1,"collections":[{"path":"c","label":"C","locked":false,"items":[{"path":"i","label":"I","locked":false,"attributes":{},"content_type":"text/plain","created":null,"modified":null}]}]}"#,
        )
        .unwrap();
        assert_eq!(read_metadata(file.path()).unwrap().version, 1);
    }

    #[test]
    fn rejects_unknown_metadata_version() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), r#"{"version":99,"collections":[]}"#).unwrap();
        assert!(read_metadata(file.path()).is_err());
    }
}
