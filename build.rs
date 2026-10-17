// The web app is embedded from `ui/dist`. The folder must exist at compile time.
// If the app is not built yet, make a small placeholder page.
fn main() {
    if std::env::var_os("CARGO_FEATURE_UI").is_none() {
        return;
    }
    let dist = std::path::Path::new("ui/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("can create ui/dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=\"utf-8\"><title>AIKIDO EMAIL</title>\
             <p>The web app is not built. Run <code>pnpm install &amp;&amp; pnpm build</code> in <code>ui/</code>, then build the server again.</p>",
        )
        .expect("can write the placeholder page");
    }
    println!("cargo:rerun-if-changed=ui/dist");
    println!("cargo:rerun-if-changed=build.rs");
}
