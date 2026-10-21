use std::{env, fs, path::Path};

fn collect(root: &Path, directory: &Path, entries: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("read frontend assets") {
        let entry = entry.expect("read frontend asset entry");
        let kind = entry.file_type().expect("read frontend asset type");
        assert!(
            !kind.is_symlink(),
            "frontend assets must not contain symlinks"
        );
        if kind.is_dir() {
            collect(root, &entry.path(), entries);
        } else if kind.is_file() {
            entries.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
    }
}

fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    println!("cargo:rerun-if-changed=assets");
    let mut entries = Vec::new();
    collect(&root, &root, &mut entries);
    entries.sort();
    assert!(
        entries.iter().any(|p| p == "index.html"),
        "missing frontend index"
    );
    let html = fs::read_to_string(root.join("index.html")).expect("read frontend index");
    for reference in html.split('"').filter_map(|s| s.strip_prefix("/assets/")) {
        assert!(
            entries
                .iter()
                .any(|name| name == &format!("assets/{reference}")),
            "missing frontend asset: {reference}"
        );
    }
    let mut source = "pub const FILES: &[(&str, &[u8])] = &[\n".to_string();
    for name in entries {
        source.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            root.join(&name).to_str().unwrap()
        ));
    }
    source.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("frontend.rs"),
        source,
    )
    .unwrap();
}
