//! `docs/agent-guide/` must equal what the sources generate.
//! Regenerate: `P2C_WRITE_DOCS=1 cargo test -p p2c-guide --test docs_sync`.
use std::path::{Path, PathBuf};

fn files_under(dir: &Path, base: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, base, out);
        } else {
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

#[test]
fn agent_guide_docs_are_in_sync() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/agent-guide");
    let want = p2c_guide::docs_files();
    if std::env::var_os("P2C_WRITE_DOCS").is_some() {
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in &want {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
    }
    let mut have = Vec::new();
    files_under(&dir, &dir, &mut have);
    have.sort();
    let mut names: Vec<String> = want.iter().map(|(p, _)| p.clone()).collect();
    names.sort();
    assert_eq!(
        have, names,
        "docs/agent-guide/ files differ: run P2C_WRITE_DOCS=1 cargo test -p p2c-guide --test docs_sync"
    );
    for (p, text) in want {
        let on_disk = std::fs::read_to_string(dir.join(&p))
            .unwrap()
            .replace("\r\n", "\n");
        assert!(
            on_disk == text,
            "docs/agent-guide/{p} is out of date: run P2C_WRITE_DOCS=1 cargo test -p p2c-guide --test docs_sync"
        );
    }
}
