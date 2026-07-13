use super::*;

impl TuiApp {
    pub(super) fn expire_secret(&mut self) {
        if self
            .reveal
            .as_ref()
            .is_some_and(|reveal| Instant::now() >= reveal.expires_at)
        {
            self.reveal = None;
            self.message = "revealed secret cleared".into();
        }
        if self
            .clipboard_clear
            .as_ref()
            .is_some_and(|clear| Instant::now() >= clear.expires_at)
        {
            if let (Some(clipboard), Some(clear)) =
                (&mut self.clipboard, self.clipboard_clear.take())
            {
                if clipboard.get_text().ok().as_deref() == Some(clear.expected.as_str()) {
                    let _ = clipboard.clear();
                }
            }
            self.message = "clipboard clear attempted".into();
        }
    }

    pub(super) fn clear_reveal(&mut self) {
        self.reveal = None;
        self.reveal_scroll_pending = false;
    }
}

pub(super) fn preview_encoding(value: &SecretValue) -> PreviewEncoding {
    let mime_type = value
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    let safe_text = std::str::from_utf8(value.secret.as_slice())
        .ok()
        .is_some_and(|text| {
            text.chars()
                .all(|ch| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r'))
        });
    if mime_type.to_ascii_lowercase().starts_with("text/") && safe_text {
        PreviewEncoding::EscapedUtf8
    } else {
        PreviewEncoding::HexDump
    }
}

pub(super) fn secret_preview(value: &SecretValue) -> Vec<String> {
    let secret = value.secret.as_slice();
    match preview_encoding(value) {
        PreviewEncoding::EscapedUtf8 => {
            let mut preview_len = secret.len().min(SECRET_PREVIEW_LIMIT);
            while !std::str::from_utf8(&secret[..preview_len]).is_ok() {
                preview_len -= 1;
            }
            let text = std::str::from_utf8(&secret[..preview_len]).expect("valid UTF-8 boundary");
            let escaped = terminal_text::attribute_value(text);
            let mut lines = vec![if escaped.is_empty() {
                "<empty>".into()
            } else {
                escaped
            }];
            append_omitted_bytes(&mut lines, secret.len() - preview_len);
            lines
        }
        PreviewEncoding::HexDump => {
            let preview_len = secret.len().min(SECRET_PREVIEW_LIMIT);
            let mut lines = secret[..preview_len]
                .chunks(16)
                .enumerate()
                .map(|(row, bytes)| {
                    let mut line = format!("{:08x}:", row * 16);
                    for byte in bytes {
                        write!(line, " {byte:02x}").expect("write to string");
                    }
                    line
                })
                .collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push("<empty>".into());
            }
            append_omitted_bytes(&mut lines, secret.len() - preview_len);
            lines
        }
    }
}

pub(super) fn append_omitted_bytes(lines: &mut Vec<String>, omitted: usize) {
    if omitted > 0 {
        lines.push(format!("… {omitted} byte(s) omitted"));
    }
}

pub(super) async fn reveal_selected(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let item_path = item.path.clone();
        let target = ItemTarget::from(item);
        let outcome = store.reveal_secret(&target).await?;
        let message = outcome_message("secret revealed for 30s", &outcome);
        let value = outcome.value;
        app.secret_metadata = Some(secret_metadata(&item_path, &value));
        app.reveal = Some(RevealState {
            item_path,
            value,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.page = Page::Details;
        app.message = message;
        app.reveal_scroll_pending = true;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CopyEncoding {
    Text,
    Base64,
    Hex,
}

impl CopyEncoding {
    fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Base64 => "Base64",
            Self::Hex => "hex",
        }
    }
}

pub(super) fn secret_metadata(item_path: &str, value: &SecretValue) -> SecretMetadata {
    SecretMetadata {
        item_path: item_path.into(),
        content_type: value.content_type.clone(),
        size: value.secret.as_slice().len(),
        encoding: preview_encoding(value),
    }
}

pub(super) async fn copy_selected(
    store: &impl SecretStore,
    app: &mut TuiApp,
    encoding: CopyEncoding,
) -> Result<()> {
    if let Some(item) = app.selected_item() {
        let item_path = item.path.clone();
        let target = ItemTarget::from(item);
        let outcome = store.reveal_secret(&target).await?;
        let warning_suffix = if outcome.warnings.is_empty() {
            String::new()
        } else {
            format!("; WARNING: {}", warning_text(&outcome.warnings))
        };
        let value = outcome.value;
        app.secret_metadata = Some(secret_metadata(&item_path, &value));
        let expected = encode_clipboard(value.secret.as_slice(), encoding)?;
        let clipboard = match app.clipboard.as_mut() {
            Some(clipboard) => clipboard,
            None => app.clipboard.insert(ArboardClipboard::new()?),
        };
        clipboard.set_text(expected.to_string())?;
        app.clipboard_clear = Some(ClipboardClearState {
            expected,
            expires_at: Instant::now() + SECRET_TTL,
        });
        app.message = format!(
            "copied as {}; clipboard clear scheduled for 30s{}",
            encoding.label(),
            warning_suffix
        );
    }
    Ok(())
}

pub(super) fn clipboard_text(secret: &[u8]) -> Result<&str> {
    std::str::from_utf8(secret)
        .map_err(|_| anyhow::anyhow!("binary secret cannot be copied as text"))
}

pub(super) fn encode_clipboard(secret: &[u8], encoding: CopyEncoding) -> Result<Zeroizing<String>> {
    let encoded = match encoding {
        CopyEncoding::Text => clipboard_text(secret)?.to_owned(),
        CopyEncoding::Base64 => BASE64_STANDARD.encode(secret),
        CopyEncoding::Hex => {
            let mut encoded = String::with_capacity(secret.len().saturating_mul(2));
            for byte in secret {
                write!(encoded, "{byte:02x}").expect("write to string");
            }
            encoded
        }
    };
    Ok(Zeroizing::new(encoded))
}

pub(super) struct ArboardClipboard {
    clipboard: arboard::Clipboard,
}

impl ArboardClipboard {
    fn new() -> Result<Self> {
        Ok(Self {
            clipboard: arboard::Clipboard::new()?,
        })
    }

    fn clear(&mut self) -> Result<()> {
        Ok(self.clipboard.clear()?)
    }

    fn set_text(&mut self, text: String) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            use arboard::SetExtLinux;
            self.clipboard.set().exclude_from_history().text(text)?;
        }
        #[cfg(not(target_os = "linux"))]
        self.clipboard.set_text(text)?;
        Ok(())
    }

    fn get_text(&mut self) -> Result<String> {
        Ok(self.clipboard.get_text()?)
    }
}

pub(super) async fn lock_toggle(store: &impl SecretStore, app: &mut TuiApp) -> Result<()> {
    if let Some(collection) = app.selected_collection().cloned() {
        app.clear_reveal();
        let outcome = store
            .set_collection_locked(&collection.path, !collection.locked)
            .await?;
        let message = outcome_message("collection lock state changed", &outcome);
        app.refresh_all(store).await?;
        app.message = message;
    }
    Ok(())
}
