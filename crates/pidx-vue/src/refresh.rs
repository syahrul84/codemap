//! Vue / Nuxt refresh: detection → per-file extraction → registry pass.
//! Runs after the shared scan; cheap when nothing relevant changed.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use pidx_core::store::{get_meta, meta_keys, set_meta, set_meta_opt};
use pidx_core::{Result, ScanReport, Store};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::detect::{VueDetection, detect};
use crate::extract::{self, JsReport};
use crate::registry::{self, REGISTRY_VERSION};

/// `warnings.adapter` for Vue warnings (detection, registry).
pub const ADAPTER: &str = "vue";

/// Bump when detection output changes.
pub const VUE_DETECTION_VERSION: i64 = 1;

/// Root files whose change can change detection.
const DETECTION_FILES: &[&str] = &[
    "package.json",
    "nuxt.config.ts",
    "nuxt.config.js",
    "nuxt.config.mjs",
    "vite.config.ts",
    "vite.config.js",
    "vite.config.mjs",
    "vue.config.js",
    "tsconfig.json",
    "jsconfig.json",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VueReport {
    pub detected: bool,
    pub vue: Option<String>,
    pub nuxt: Option<String>,
    pub js: JsReport,
    pub registry_ran: bool,
    pub components: usize,
    pub duration_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct StoredDetection {
    version: i64,
    detection: VueDetection,
}

pub fn stored_detection(store: &Store) -> Result<Option<VueDetection>> {
    Ok(get_meta(store.conn(), meta_keys::VUE_DETECTION)?
        .and_then(|j| serde_json::from_str::<StoredDetection>(&j).ok())
        .map(|s| s.detection))
}

fn is_js(path: &str) -> bool {
    [
        ".vue", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx",
    ]
    .iter()
    .any(|e| path.ends_with(e))
}

pub fn refresh(store: &mut Store, root: &Path, scan: &ScanReport, full: bool) -> Result<VueReport> {
    let started = Instant::now();
    let stored = get_meta(store.conn(), meta_keys::VUE_DETECTION)?
        .and_then(|j| serde_json::from_str::<StoredDetection>(&j).ok())
        .filter(|s| s.version == VUE_DETECTION_VERSION);
    let config_changed = scan
        .changed_paths
        .iter()
        .chain(&scan.removed_paths)
        .any(|p| DETECTION_FILES.contains(&p.as_str()));
    let (detection, detection_changed) = match stored {
        Some(s) if !full && !config_changed => (s.detection, false),
        prev => {
            let d = detect(root);
            let changed = prev.as_ref().map(|p| &p.detection) != Some(&d);
            set_meta(
                store.conn(),
                meta_keys::VUE_DETECTION,
                &serde_json::to_string(&StoredDetection {
                    version: VUE_DETECTION_VERSION,
                    detection: d.clone(),
                })?,
            )?;
            set_meta_opt(
                store.conn(),
                meta_keys::PACKAGE_NAME,
                d.package_name.as_deref(),
            )?;
            (d, changed)
        }
    };

    let mut report = VueReport {
        detected: detection.vue,
        vue: detection.vue_package.as_ref().map(|p| p.display()),
        nuxt: detection.nuxt.as_ref().map(|n| n.package.display()),
        ..VueReport::default()
    };
    if !detection.vue {
        clear(store)?;
        return Ok(report);
    }
    report.js = extract::index_pending(store, root)?;

    let dts_mtime = std::fs::metadata(root.join(".nuxt/components.d.ts"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let state = format!("{REGISTRY_VERSION}|{dts_mtime}");
    let js_removed = scan.removed_paths.iter().any(|p| is_js(p));
    let needed = full
        || detection_changed
        || report.js.parsed > 0
        || js_removed
        || get_meta(store.conn(), meta_keys::VUE_REGISTRY_STATE)?.as_deref()
            != Some(state.as_str());
    if needed {
        let modules = registry::load_modules(store)?;
        let files: HashSet<String> = store.list_files()?.into_iter().map(|f| f.path).collect();
        let reg = registry::build(root, &detection, &modules, &files);
        report.components = registry::store(store, &modules, &reg)?;
        report.registry_ran = true;
        let conn = store.conn();
        conn.execute("DELETE FROM warnings WHERE adapter = ?1", [ADAPTER])?;
        for w in detection.warnings.iter().chain(&reg.notes) {
            conn.execute(
                "INSERT INTO warnings (file_id, path, line, adapter, message) VALUES (NULL, NULL, NULL, ?1, ?2)",
                params![ADAPTER, w],
            )?;
        }
        set_meta(conn, meta_keys::VUE_REGISTRY_STATE, &state)?;
    } else {
        report.components = store.conn().query_row(
            "SELECT COUNT(*) FROM js_modules WHERE kind <> 'module'",
            [],
            |r| r.get(0),
        )?;
    }
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// The project is not (or no longer) a Vue app: drop JS facts.
fn clear(store: &mut Store) -> Result<()> {
    let has_rows: bool =
        store
            .conn()
            .query_row("SELECT EXISTS (SELECT 1 FROM js_modules)", [], |r| r.get(0))?;
    if !has_rows {
        return Ok(());
    }
    let tx = store.conn_mut().transaction()?;
    tx.execute(
        "DELETE FROM symbols WHERE file_id IN (SELECT id FROM files WHERE lang IN ('vue', 'js', 'ts'))",
        [],
    )?;
    tx.execute("DELETE FROM js_modules", [])?;
    tx.execute("DELETE FROM component_uses", [])?;
    tx.execute(
        "DELETE FROM extractor_state WHERE extractor = ?1",
        [extract::EXTRACTOR],
    )?;
    tx.execute(
        "DELETE FROM warnings WHERE adapter IN (?1, ?2)",
        [extract::EXTRACTOR, ADAPTER],
    )?;
    tx.execute(
        "DELETE FROM meta WHERE key = ?1",
        [meta_keys::VUE_REGISTRY_STATE],
    )?;
    tx.commit()?;
    Ok(())
}
