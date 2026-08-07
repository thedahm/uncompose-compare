use std::path::Path;

/// Fail the build loudly when the frontend bundle is absent at embed time.
///
/// rust-embed will happily embed an empty or missing folder, producing a
/// binary that serves 404s for its own UI. That failure would surface only at
/// runtime, far from its cause. Catch it here with an actionable message so the
/// packaging chain can never ship a wheel with absent assets (spec #1, story 6).
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dist = Path::new(&manifest_dir).join("frontend").join("dist");
    let index = dist.join("index.html");

    // Rebuild whenever the bundle changes (or (dis)appears).
    println!("cargo:rerun-if-changed=frontend/dist");
    println!("cargo:rerun-if-changed=frontend/dist/index.html");

    if !index.is_file() {
        panic!(
            "frontend bundle missing: expected {} to exist at embed time.\n\
             Build the frontend first:\n    \
             (cd frontend && npm install && npm run build)\n\
             or run the whole pipeline with `npm run build` from the repo root.",
            index.display()
        );
    }
}
