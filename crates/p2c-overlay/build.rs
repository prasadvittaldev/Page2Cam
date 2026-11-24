//! Embeds the built-in templates: every `templates/<id>.json` with its
//! `templates/<id>.html` becomes an entry of `builtin::SOURCES`.
use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .expect("templates folder")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();
    ids.sort();
    let mut out = String::from("pub(crate) const SOURCES: &[(&str, &str, &str)] = &[\n");
    for id in &ids {
        let meta = dir.join(format!("{id}.json"));
        let html = dir.join(format!("{id}.html"));
        if !html.exists() {
            println!("cargo:warning=templates/{id}.html is missing: {id} left out");
            continue;
        }
        println!("cargo:rerun-if-changed={}", meta.display());
        println!("cargo:rerun-if-changed={}", html.display());
        writeln!(
            out,
            "    ({id:?}, include_str!({:?}), include_str!({:?})),",
            meta.to_string_lossy(),
            html.to_string_lossy()
        )
        .unwrap();
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("builtin.rs");
    std::fs::write(dest, out).unwrap();
}
