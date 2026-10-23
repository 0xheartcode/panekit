//! Failure artefacts: on a failed run, write a small bundle to a directory so
//! the failure can be inspected without re-running. The bundle holds the final
//! rendered screen, the last seam snapshot, the event track, and a manifest that
//! points at them and carries the failure summary.
//!
//! Secret-safe: the events are already redacted upstream (a `type --from-env`
//! value is never recorded), and nothing here reads the environment. Bounded:
//! the screen is one capture and the seam is the last snapshot, both small.

use crate::script::RunEvent;
use serde_json::{Value, json};
use std::io;
use std::path::{Path, PathBuf};

/// The files [`write_bundle`] wrote, for the run summary to point at. A field is
/// `None` when that piece was not available (for example a run with no seam).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactPaths {
    /// The bundle directory.
    pub dir: PathBuf,
    /// The manifest file (always written).
    pub manifest: PathBuf,
    /// The final rendered screen, when a capture was available.
    pub screen: Option<PathBuf>,
    /// The last seam snapshot, when one was read.
    pub seam: Option<PathBuf>,
    /// The event track (always written, possibly empty).
    pub events: Option<PathBuf>,
}

impl ArtifactPaths {
    /// As a JSON object for the run summary's `artifacts` key. Only the pieces
    /// that were written are present.
    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("dir".into(), json!(self.dir.display().to_string()));
        m.insert(
            "manifest".into(),
            json!(self.manifest.display().to_string()),
        );
        for (key, path) in [
            ("screen", &self.screen),
            ("seam", &self.seam),
            ("events", &self.events),
        ] {
            if let Some(p) = path {
                m.insert(key.into(), json!(p.display().to_string()));
            }
        }
        Value::Object(m)
    }
}

/// Write a failure bundle into `dir` (created if absent): the final rendered
/// `screen` (when captured), the last `seam` snapshot (when read), the `events`
/// track, and a `manifest.json` that references them and embeds the `failure`
/// summary. Returns the paths written.
pub fn write_bundle(
    dir: &Path,
    screen: Option<&str>,
    seam: Option<&Value>,
    events: &[RunEvent],
    failure: &Value,
) -> io::Result<ArtifactPaths> {
    std::fs::create_dir_all(dir)?;
    let mut paths = ArtifactPaths {
        dir: dir.to_path_buf(),
        manifest: dir.join("manifest.json"),
        ..Default::default()
    };
    if let Some(s) = screen {
        let p = dir.join("screen.txt");
        std::fs::write(&p, s)?;
        paths.screen = Some(p);
    }
    if let Some(v) = seam {
        let p = dir.join("seam.json");
        std::fs::write(&p, serde_json::to_vec_pretty(v)?)?;
        paths.seam = Some(p);
    }
    // Always written, even when empty, so the bundle's shape is stable.
    let events_path = dir.join("events.jsonl");
    let body: String = events
        .iter()
        .map(|e| format!("{}\n", e.to_json()))
        .collect();
    std::fs::write(&events_path, body)?;
    paths.events = Some(events_path);

    // The manifest lives inside the bundle, so it names its members by bare
    // filename, never the (often absolute) path they were written at. That keeps
    // the bundle relocatable: uploaded as a CI artifact and downloaded on another
    // machine, the manifest still points at its siblings. (The run summary's
    // `artifacts` object, emitted live on the machine that ran, keeps full paths
    // so an operator can open the local bundle.)
    let member = |p: &Option<PathBuf>| -> Option<String> {
        p.as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string)
    };
    let manifest = json!({
        "failure": failure,
        "screen": member(&paths.screen),
        "seam": member(&paths.seam),
        "events": member(&paths.events),
    });
    std::fs::write(&paths.manifest, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "panedrive-artifacts-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_every_piece_and_a_manifest_that_points_at_them() {
        let dir = tmpdir("full");
        let events = Vec::new();
        let failure = json!({ "kind": "child-exit", "step": 2 });
        let paths = write_bundle(
            &dir,
            Some("READY\n"),
            Some(&json!({ "ready": true })),
            &events,
            &failure,
        )
        .unwrap();

        assert_eq!(
            paths.dir, dir,
            "the bundle records the directory it wrote to"
        );
        assert_eq!(
            paths.to_json()["dir"],
            json!(dir.display().to_string()),
            "the summary's dir must be the real bundle path, not an empty default"
        );
        assert!(paths.screen.as_ref().unwrap().exists());
        assert!(paths.seam.as_ref().unwrap().exists());
        assert!(paths.events.as_ref().unwrap().exists());
        assert!(paths.manifest.exists());

        assert_eq!(
            std::fs::read_to_string(paths.screen.unwrap()).unwrap(),
            "READY\n"
        );
        let seam: Value =
            serde_json::from_slice(&std::fs::read(paths.seam.unwrap()).unwrap()).unwrap();
        assert_eq!(seam, json!({ "ready": true }));

        let manifest: Value =
            serde_json::from_slice(&std::fs::read(&paths.manifest).unwrap()).unwrap();
        assert_eq!(manifest["failure"], failure);

        // The manifest names its members by bare basename, not the absolute path
        // they were written at (`dir` is absolute), so the bundle is relocatable.
        assert_eq!(manifest["screen"], json!("screen.txt"));
        assert_eq!(manifest["seam"], json!("seam.json"));
        assert_eq!(manifest["events"], json!("events.jsonl"));
        for key in ["screen", "seam", "events"] {
            let v = manifest[key].as_str().unwrap();
            assert!(
                !v.contains('/') && !v.contains('\\'),
                "manifest member `{key}` must be a bundle-relative basename, got {v:?}"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn omits_absent_pieces_from_the_paths_and_json() {
        // No screen and no seam: those keys must be absent, but events and the
        // manifest are always present, so the bundle shape stays predictable.
        let dir = tmpdir("sparse");
        let paths = write_bundle(&dir, None, None, &[], &json!({ "kind": "assert" })).unwrap();
        assert!(paths.screen.is_none());
        assert!(paths.seam.is_none());
        assert!(paths.events.is_some());

        let j = paths.to_json();
        assert!(j.get("screen").is_none());
        assert!(j.get("seam").is_none());
        assert!(j.get("events").is_some());
        assert!(j.get("dir").is_some());
        assert!(j.get("manifest").is_some());

        std::fs::remove_dir_all(&dir).ok();
    }
}
