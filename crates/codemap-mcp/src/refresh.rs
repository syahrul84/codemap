//! The one refresh path shared by the CLI and the MCP server: scan → PHP →
//! Laravel → Vue/Nuxt → Swift →
//! Android → Rust. Each adapter skips work when nothing relevant changed.

use std::path::Path;

use codemap_android::AndroidReport;
use codemap_core::{Config, Result, ScanOptions, ScanReport, Store};
use codemap_laravel::LaravelReport;
use codemap_php::PhpReport;
use codemap_rust::RustReport;
use codemap_swift::SwiftReport;
use codemap_vue::VueReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    pub scan: ScanReport,
    pub php: PhpReport,
    pub laravel: LaravelReport,
    pub vue: VueReport,
    #[serde(default)]
    pub swift: SwiftReport,
    #[serde(default)]
    pub android: AndroidReport,
    #[serde(default)]
    pub rust: RustReport,
}

impl RefreshReport {
    /// Files whose facts changed in this refresh.
    pub fn reindexed_files(&self) -> usize {
        self.scan.reparsed + self.scan.removed
    }
}

/// How long a refresh waits for another one of the same index to finish,
/// in milliseconds. A first index of a large project takes longer than
/// this; whoever waited then goes without (the MCP server answers from the
/// index as it is, a hook says nothing).
static TURN_WAIT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(20_000);

/// Sets how long this process waits its turn to refresh an index. The hook
/// after an edit waits less: the assistant is waiting for it.
pub fn set_turn_wait(ms: u64) {
    TURN_WAIT_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// The lock held while an index is refreshed: a file beside it, locked for
/// as long as the returned handle lives. An index in memory needs none.
fn take_turn(store: &Store) -> Result<Option<std::fs::File>> {
    let Some(index) = store.path() else {
        return Ok(None);
    };
    let path = index.with_extension("refreshing");
    let io = |source| codemap_core::Error::Io {
        path: path.clone(),
        source,
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(io)?;
    let started = std::time::Instant::now();
    let wait =
        std::time::Duration::from_millis(TURN_WAIT_MS.load(std::sync::atomic::Ordering::Relaxed));
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < wait => {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "another codemap process is still refreshing this index",
                )));
            }
            // A file system without locks (some network mounts): go on as before.
            Err(std::fs::TryLockError::Error(_)) => return Ok(None),
        }
    }
}

pub fn refresh(
    store: &mut Store,
    root: &Path,
    config: &Config,
    opts: ScanOptions,
) -> Result<RefreshReport> {
    // One refresh of an index at a time, across processes (the MCP server,
    // a hook, the CLI, a second AI session on the same project). Without
    // it two of them read the files at different moments and the older
    // reading can be the one written last, which then stays until another
    // file changes.
    let _turn = take_turn(store)?;
    let base = codemap_laravel::refresh(store, root, config, opts)?;
    let vue = codemap_vue::refresh(store, root, &base.scan, opts.full)?;
    let swift = codemap_swift::refresh(store, root, &base.scan, opts.full)?;
    let android = codemap_android::refresh(store, root, &base.scan, opts.full)?;
    let rust = codemap_rust::refresh(store, root, &base.scan, opts.full)?;
    // Supabase / plain SQL migrations give the schema of non-Laravel apps.
    if !root.join("artisan").is_file()
        && let Some(n) = codemap_laravel::schema::supabase::refresh(store, root)?
    {
        tracing::info!(tables = n, "schema replayed from supabase/migrations");
    }
    // Done: a file written from the scan's start until now may have been
    // read half written, and the next scan looks at those again.
    store.set_meta(
        codemap_core::store::meta_keys::REFRESH_DONE_NS,
        &codemap_core::scan::now_ns().to_string(),
    )?;
    Ok(RefreshReport {
        scan: base.scan,
        php: base.php,
        laravel: base.laravel,
        vue,
        swift,
        android,
        rust,
    })
}
