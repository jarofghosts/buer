//! Project files: one `.buer` on disk holding every pattern and every setting.
//!
//! The plugin's state already carries all of that — a host saving its own project writes exactly
//! the same thing into its own file. This module puts it somewhere you can name, so a bank can move
//! between projects and between hosts, and so the standalone build has somewhere to keep one.
//!
//! The payload is nih-plug's own [`PluginState`] rather than a struct of our own. That is the whole
//! point: a project cannot drift out of step with what the plugin actually persists, because adding
//! a parameter or a persisted field adds it to both at once.

use nih_plug::prelude::PluginState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The extension a project is saved under.
pub const EXTENSION: &str = "buer";

/// Written into every file so a project can be told from any other JSON before it is interpreted.
const FORMAT: &str = "buer-project";

/// Bumped only if the envelope around the state changes shape. The state inside it carries its own
/// version, which is what nih-plug's `filter_state` would migrate.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Project {
    format: String,
    format_version: u32,
    /// Which build wrote it. Informational — the state has its own version for migrations.
    written_by: String,
    state: PluginState,
}

/// Encode a plugin state as the bytes of a project file.
pub fn encode(state: PluginState) -> Result<Vec<u8>, String> {
    let project = Project {
        format: FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        state,
    };
    // Pretty-printed. A note is one line of four numbers either way, and everything around them
    // being legible makes a project something you can open and read.
    serde_json::to_vec_pretty(&project).map_err(|e| format!("could not encode project: {e}"))
}

/// Decode a project file, or explain why it is not one.
pub fn decode(bytes: &[u8]) -> Result<PluginState, String> {
    let project: Project =
        serde_json::from_slice(bytes).map_err(|e| format!("not a readable project file: {e}"))?;

    if project.format != FORMAT {
        return Err(format!(
            "not a buer project — it calls itself {:?}",
            project.format
        ));
    }
    if project.format_version > FORMAT_VERSION {
        return Err(format!(
            "written by a newer buer (project format {}, this build reads {FORMAT_VERSION})",
            project.format_version
        ));
    }

    Ok(project.state)
}

pub fn write(path: &Path, state: PluginState) -> Result<(), String> {
    let bytes = encode(state)?;
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", label(path)))
}

pub fn read(path: &Path) -> Result<PluginState, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", label(path)))?;
    decode(&bytes)
}

/// Give a path the extension, if whoever typed it did not.
pub fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(EXTENSION)
    }
}

pub fn label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_state() -> PluginState {
        PluginState {
            version: "0.1.0".to_string(),
            params: Default::default(),
            fields: Default::default(),
        }
    }

    #[test]
    fn a_project_written_here_is_read_back_here() {
        let bytes = encode(a_state()).unwrap();
        assert_eq!(decode(&bytes).unwrap().version, "0.1.0");
    }

    #[test]
    fn any_other_json_is_refused_by_name() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format": "mater-project",
            "format_version": 1,
            "written_by": "0.1.0",
            "state": { "version": "0.1.0", "params": {}, "fields": {} },
        }))
        .unwrap();
        let error = decode(&bytes).unwrap_err();
        assert!(error.contains("not a buer project"), "{error}");
    }

    #[test]
    fn something_that_is_not_a_project_at_all_says_so_rather_than_panicking() {
        let error = decode(b"this is not json").unwrap_err();
        assert!(error.contains("not a readable project file"), "{error}");
    }

    #[test]
    fn a_file_from_a_newer_format_says_so_rather_than_half_loading() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format": FORMAT,
            "format_version": FORMAT_VERSION + 1,
            "written_by": "9.0.0",
            "state": { "version": "9.0.0", "params": {}, "fields": {} },
        }))
        .unwrap();
        let error = decode(&bytes).unwrap_err();
        assert!(error.contains("newer buer"), "{error}");
    }

    #[test]
    fn a_name_typed_without_an_extension_still_gets_one() {
        assert_eq!(
            with_extension(PathBuf::from("/tmp/rast")),
            PathBuf::from("/tmp/rast.buer")
        );
        assert_eq!(
            with_extension(PathBuf::from("/tmp/rast.json")),
            PathBuf::from("/tmp/rast.json")
        );
    }
}
