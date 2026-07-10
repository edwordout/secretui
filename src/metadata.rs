use crate::domain::{MetadataFile, SecretBackupFile};
use age::secrecy::SecretString;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

pub fn write_metadata(path: &Path, metadata: &MetadataFile) -> Result<()> {
    let text = serde_json::to_string_pretty(&metadata.clone().sorted())?;
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

pub fn read_metadata(path: &Path) -> Result<MetadataFile> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).context("parse metadata json")
}

pub fn write_encrypted_backup(
    path: &Path,
    backup: &SecretBackupFile,
    passphrase: String,
) -> Result<()> {
    let mut plain = serde_json::to_vec_pretty(&backup.clone().sorted())?;
    let recipient = age::scrypt::Recipient::new(SecretString::from(passphrase));
    let encrypted = age::encrypt(&recipient, &plain).context("encrypt backup")?;
    plain.zeroize();
    fs::write(path, encrypted).with_context(|| format!("write {}", path.display()))
}

pub fn read_encrypted_backup(path: &Path, passphrase: String) -> Result<SecretBackupFile> {
    let encrypted = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let identity = age::scrypt::Identity::new(SecretString::from(passphrase));
    let mut plain = age::decrypt(&identity, &encrypted).context("decrypt backup")?;
    let backup = serde_json::from_slice(&plain).context("parse backup json")?;
    plain.zeroize();
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CollectionMetadata, ItemMetadata};
    use std::collections::BTreeMap;

    #[test]
    fn metadata_json_is_deterministic() {
        let metadata = MetadataFile {
            version: 1,
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
                        content_type: None,
                        created: None,
                        modified: None,
                    },
                    ItemMetadata {
                        path: "a".into(),
                        label: "A".into(),
                        locked: false,
                        attributes: BTreeMap::new(),
                        content_type: None,
                        created: None,
                        modified: None,
                    },
                ],
            }],
        };
        let json = serde_json::to_string_pretty(&metadata.sorted()).unwrap();
        assert!(json.find("\"a\"").unwrap() < json.find("\"z\"").unwrap());
    }
}
