use std::path::Path;

fn main() {
    // Ensure web/dist/ directory exists so rust-embed can reference it
    // even when the web UI hasn't been built yet.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dist_dir = Path::new(&manifest_dir).join("../../web/dist");
    if !dist_dir.exists() {
        std::fs::create_dir_all(dist_dir).ok();
    }
}
