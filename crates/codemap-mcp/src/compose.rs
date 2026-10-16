//! Docker Compose: which service runs an app, so test commands can run
//! inside it (`docker compose -f ../development/docker-compose.yml exec -T
//! backend php artisan test`). Read with a small line reader, enough for the
//! `services:` → `volumes:` part of a compose file.

use std::path::{Component, Path, PathBuf};

const NAMES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

/// Services that run background work rather than the app itself.
const WORKERS: &[&str] = &[
    "queue",
    "worker",
    "scheduler",
    "cron",
    "reverb",
    "horizon",
    "websocket",
];

/// `(service, container path)` mounts of one compose file, resolved to host paths.
fn mounts(file: &Path) -> Vec<(String, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let dir = file.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    let mut in_services = false;
    let mut service_indent: Option<usize> = None;
    let mut service: Option<String> = None;
    let mut in_volumes = false;
    for raw in text.lines() {
        let line = raw.split(" #").next().unwrap_or(raw);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if indent == 0 {
            in_services = t == "services:";
            service = None;
            service_indent = None;
            continue;
        }
        if !in_services {
            continue;
        }
        let si = *service_indent.get_or_insert(indent);
        if indent == si {
            service = t
                .strip_suffix(':')
                .map(|s| s.trim_matches(['"', '\'']).to_owned());
            in_volumes = false;
            continue;
        }
        let Some(svc) = &service else {
            continue;
        };
        if indent > si && !t.starts_with('-') && t.ends_with(':') {
            in_volumes = t == "volumes:";
            continue;
        }
        if indent > si && t.contains(':') && !t.starts_with('-') {
            in_volumes =
                in_volumes && t.starts_with("source:") || in_volumes && t.starts_with("target:");
        }
        if !in_volumes {
            continue;
        }
        // Short syntax `- ../backend:/var/www/html[:ro]`, or long `source: ../backend`.
        let entry = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("source:"))
            .map(|e| e.trim().trim_matches(['"', '\'']));
        let Some(entry) = entry else {
            continue;
        };
        let src = entry.split(':').next().unwrap_or(entry);
        if src.starts_with('.') || src.starts_with('/') {
            out.push((svc.clone(), normalize(&dir.join(src))));
        }
    }
    out
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` from `from` (both absolute): `../development/docker-compose.yml`.
fn relative(path: &Path, from: &Path) -> String {
    let p: Vec<_> = path.components().collect();
    let f: Vec<_> = from.components().collect();
    let common = p.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..f.len() {
        parts.push("..".into());
    }
    for c in &p[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

/// The compose service that mounts `app_root`, and the compose file as seen
/// from the app: `(service, file relative to the app)`.
pub fn service_for(app_root: &Path, app_name: Option<&str>) -> Option<(String, String)> {
    let app = normalize(app_root);
    let mut dirs: Vec<PathBuf> = vec![app.clone()];
    if let Some(parent) = app.parent() {
        dirs.push(parent.to_owned());
        // One level down from the workspace root (`development/`, `docker/`).
        if let Ok(rd) = std::fs::read_dir(parent) {
            let mut subs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && *p != app)
                .collect();
            subs.sort();
            dirs.extend(subs);
        }
    }
    let mut best: Option<(i32, String, String)> = None;
    for d in dirs {
        for n in NAMES {
            let file = d.join(n);
            if !file.is_file() {
                continue;
            }
            for (svc, src) in mounts(&file) {
                if src != app {
                    continue;
                }
                let lower = svc.to_lowercase();
                let mut score = 1;
                if app_name.is_some_and(|a| lower == a.to_lowercase()) {
                    score += 10;
                }
                if ["app", "php", "backend", "api", "laravel", "web"].contains(&lower.as_str()) {
                    score += 3;
                }
                if WORKERS.iter().any(|w| lower.contains(w)) {
                    score -= 5;
                }
                if best.as_ref().is_none_or(|b| score > b.0) {
                    best = Some((score, svc, relative(&file, &app)));
                }
            }
        }
    }
    best.filter(|b| b.0 > 0).map(|(_, s, f)| (s, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_service_mounting_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("backend")).unwrap();
        std::fs::create_dir_all(root.join("development")).unwrap();
        std::fs::write(
            root.join("development/docker-compose.yml"),
            "services:\n  postgres:\n    image: postgres\n  backend:\n    build: .\n    volumes:\n      - ../backend:/var/www/html\n  queue:\n    volumes:\n      - ../backend:/var/www/html\nvolumes:\n  data:\n",
        )
        .unwrap();
        let got = service_for(&root.join("backend"), Some("backend"));
        assert_eq!(
            got,
            Some(("backend".into(), "../development/docker-compose.yml".into()))
        );
        assert_eq!(service_for(&root.join("development"), None), None);
    }
}
