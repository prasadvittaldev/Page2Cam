//! Embeds `content/playbooks/*.md` and `content/howto/*.md` as
//! `(id, text)` tables.
use std::fmt::Write as _;
use std::path::Path;

fn table(name: &str, dir: &Path, out: &mut String) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p: &std::path::PathBuf| p.extension().is_some_and(|e| e == "md"));
    files.sort();
    writeln!(out, "pub(crate) const {name}: &[(&str, &str)] = &[").unwrap();
    for p in files {
        println!("cargo:rerun-if-changed={}", p.display());
        let id = p.file_stem().unwrap().to_string_lossy().into_owned();
        writeln!(
            out,
            "    ({id:?}, include_str!({:?})),",
            p.to_string_lossy()
        )
        .unwrap();
    }
    out.push_str("];\n");
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("content");
    let mut out = String::new();
    table("PLAYBOOKS", &root.join("playbooks"), &mut out);
    table("HOWTOS", &root.join("howto"), &mut out);
    println!(
        "cargo:rerun-if-changed={}",
        root.join("instructions.md").display()
    );
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("content.rs");
    std::fs::write(dest, out).unwrap();
}
