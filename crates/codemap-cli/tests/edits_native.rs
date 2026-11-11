//! The check after each edit for Swift, Kotlin and Rust: a name the code
//! says is the project's own (a path into its crates, an import from its
//! packages, a member of its enums) that nothing declares.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

struct Fixture {
    root: PathBuf,
    data: tempfile::TempDir,
    _dir: tempfile::TempDir,
}

fn fixture(name: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name),
        dir.path(),
    );
    let root = dir.path().canonicalize().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_codemap"))
        .args(["--data-dir", data.path().to_str().unwrap(), "scan"])
        .arg("--project")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Fixture {
        root,
        data,
        _dir: dir,
    }
}

impl Fixture {
    /// Writes a new file and runs the hook for it; what the assistant is told.
    fn write(&self, rel: &str, body: &str) -> String {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_codemap"))
            .args([
                "--data-dir",
                self.data.path().to_str().unwrap(),
                "hook",
                "edited",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let call = json!({
            "session_id": "s1", "cwd": self.root, "tool_name": "Write",
            "tool_input": { "file_path": path, "content": body },
            "tool_response": { "type": "create" },
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(call.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "the hook must exit 0");
        let text = String::from_utf8(out.stdout).unwrap();
        if text.trim().is_empty() {
            return String::new();
        }
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[test]
fn swift_members_of_the_projects_own_types() {
    let f = fixture("swift-app");
    let told = f.write(
        "Sources/Demo/Probe.swift",
        "import Foundation\n\nfunc probe() {\n    let a = Filter.all\n    let b = Filter.don\n    let c = Filter.allCases\n    // Filter.inAComment\n    print(\"Filter.inAString\", a, b, c)\n}\n",
    );
    assert!(
        told.contains("`Filter.don` does not exist (closest: `Filter.done`)"),
        "{told}"
    );
    for quiet in ["Filter.all`", "allCases", "inAComment", "inAString"] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
    // A member an extension adds in another file is a member.
    f.write(
        "Sources/Demo/FilterTitle.swift",
        "extension Filter {\n    static let fallback = Filter.all\n}\n",
    );
    let told = f.write(
        "Sources/Demo/Probe2.swift",
        "func probe2() {\n    _ = Filter.fallback\n}\n",
    );
    assert_eq!(told, "");
}

#[test]
fn kotlin_imports_and_enum_members() {
    let f = fixture("android-app");
    let told = f.write(
        "app/src/main/java/com/example/demo/ui/Probe.kt",
        "package com.example.demo.ui\n\nimport android.content.Context\nimport com.example.demo.Filter\nimport com.example.demo.Filtr\nimport com.example.demo.R\n\nfun probe(context: Context) {\n    val a = Filter.DONE\n    val b = Filter.DON\n    val c = Filter.values()\n    println(\"$a $b $c\")\n}\n",
    );
    assert!(
        told.contains(
            "`com.example.demo.Filtr` does not exist (closest: `com.example.demo.Filter`)"
        ),
        "{told}"
    );
    assert!(
        told.contains("`Filter.DON` does not exist (closest: `Filter.DONE`)"),
        "{told}"
    );
    for quiet in [
        "android.content",
        "`com.example.demo.R` does",
        "`Filter.DONE` does",
        "values",
    ] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
}

#[test]
fn rust_paths_into_the_projects_own_crates() {
    let f = fixture("rust-crate");
    let told = f.write(
        "src/probe.rs",
        "use crate::label;\nuse crate::labl;\nuse crate::store::Store;\nuse demo_tool::Missing;\nuse std::collections::HashMap;\n\npub fn probe() -> HashMap<u8, u8> {\n    // crate::in_a_comment::Thing\n    let _ = \"crate::in_a_string::Thing\";\n    HashMap::new()\n}\n",
    );
    assert!(
        told.contains("`crate::labl` does not exist (closest: `label`)"),
        "{told}"
    );
    assert!(
        told.contains("`demo_tool::Missing` does not exist"),
        "{told}"
    );
    for quiet in [
        "crate::label`",
        "Store",
        "HashMap",
        "in_a_comment",
        "in_a_string",
    ] {
        assert!(!told.contains(quiet), "{quiet}: {told}");
    }
    // A local trait put on another crate's type gives the index an item
    // under that crate's name; the crate is still not the project's.
    f.write(
        "src/ext.rs",
        "pub trait Shout {\n    fn shout(&self) -> String;\n}\n\nimpl Shout for serde_json::Value {\n    fn shout(&self) -> String {\n        self.to_string()\n    }\n}\n",
    );
    let told = f.write(
        "src/probe2.rs",
        "use serde_json::Map;\nuse serde_json::NoSuchThing;\n\npub fn probe2() {}\n",
    );
    assert_eq!(told, "");
}
