//! Configure Codex's native footer without replacing the user's other settings.

use anyhow::{Context, Result, bail};
use std::{fs, io::Write, path::Path};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// Codex accepts built-in item identifiers, not a status-line command. Its
/// limit items display remaining allowance; costsee's dashboard displays used.
const CODEX_ITEMS: &[&str] = &[
    "model-with-reasoning",
    "current-dir",
    "git-branch",
    "context-used",
    "five-hour-limit",
    "weekly-limit",
];

/// Returns false when this exact preset is already installed. Custom settings
/// (including an explicitly empty list) are left untouched.
pub fn configure_codex(settings: &Path) -> Result<bool> {
    // Follow an existing symlink so installing a preset doesn't sever dotfile
    // management. Resolve before reading as well as before replacing the file.
    let destination = if fs::symlink_metadata(settings).is_ok() {
        fs::canonicalize(settings)
            .with_context(|| format!("failed to resolve {}", settings.display()))?
    } else {
        settings.to_path_buf()
    };
    let raw = match fs::read_to_string(&destination) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("failed to read Codex config"),
    };
    let Some(updated) = codex_config(&raw)? else {
        return Ok(false);
    };
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = fs::metadata(&destination) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.write_all(updated.as_bytes())?;
    temporary.persist(&destination)?;
    Ok(true)
}

fn codex_config(raw: &str) -> Result<Option<String>> {
    let mut doc = raw
        .parse::<DocumentMut>()
        .context("invalid Codex config; it was not changed")?;
    if doc.get("tui").is_none() {
        doc["tui"] = Item::Table(Table::new());
    }
    let tui = doc["tui"]
        .as_table_like_mut()
        .context("Codex tui setting must be a table; config was not changed")?;
    if let Some(existing) = tui.get("status_line") {
        let items = existing.as_array().and_then(|array| {
            array
                .iter()
                .map(|item| item.as_str())
                .collect::<Option<Vec<_>>>()
        });
        if items.as_deref() == Some(CODEX_ITEMS) {
            return Ok(None);
        }
        bail!(
            "Codex already has a custom tui.status_line; it was not overwritten.\n\
             Use /statusline in Codex to add model, directory, branch, context, and usage limits."
        );
    }
    let mut items = Array::new();
    for item in CODEX_ITEMS {
        items.push(*item);
    }
    tui.insert("status_line", value(items));
    Ok(Some(doc.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn installs_a_native_preset_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("codex/config.toml");
        assert!(configure_codex(&path).unwrap());
        let raw = fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&raw).unwrap();
        let items = parsed["tui"]["status_line"].as_array().unwrap();
        assert_eq!(items.len(), CODEX_ITEMS.len());
        assert_eq!(items[4].as_str(), Some("five-hour-limit"));
        assert_eq!(items[5].as_str(), Some("weekly-limit"));
        assert!(!configure_codex(&path).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
    }

    #[test]
    fn preserves_comments_and_other_settings_in_all_table_styles() {
        for raw in [
            "# my config\nmodel = 'example' # keep\n[tui]\nnotifications = false\n",
            "# my config\nmodel = 'example' # keep\ntui = { notifications = false }\n",
            "# my config\nmodel = 'example' # keep\ntui.notifications = false\n",
        ] {
            let updated = codex_config(raw).unwrap().unwrap();
            assert!(updated.starts_with("# my config\nmodel = 'example' # keep\n"));
            let parsed: toml::Value = toml::from_str(&updated).unwrap();
            assert_eq!(parsed["tui"]["notifications"].as_bool(), Some(false));
            assert!(parsed["tui"]["status_line"].is_array());
        }
    }

    #[test]
    fn custom_disabled_and_invalid_configs_are_never_written() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for raw in [
            "[tui]\nstatus_line = ['model']\n",
            "tui = { status_line = [] }\n",
            "tui.status_line = ['weekly-limit']\n",
            "tui = false\n",
            "invalid = [\n",
        ] {
            fs::write(&path, raw).unwrap();
            assert!(configure_codex(&path).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_dotfile_symlinks_and_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempdir().unwrap();
        let source = dir.path().join("source.toml");
        let settings = dir.path().join("config.toml");
        fs::write(&source, "# managed config\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&source, &settings).unwrap();
        assert!(configure_codex(&settings).unwrap());
        assert!(fs::symlink_metadata(&settings).unwrap().is_symlink());
        assert!(fs::read_to_string(&source).unwrap().contains("status_line"));
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
