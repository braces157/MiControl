fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=app.debug.manifest");
    println!("cargo:rerun-if-changed=assets/micontrol.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let is_release = std::env::var("PROFILE").as_deref() == Ok("release");
        let manifest = if is_release {
            "app.manifest"
        } else {
            "app.debug.manifest"
        };

        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file(manifest);
        if std::path::Path::new("assets/micontrol.ico").exists() {
            res.set_icon("assets/micontrol.ico");
        }
        res.set("ProductName", "MiControl");
        res.set(
            "FileDescription",
            "Ultra-lightweight Rust hardware controller for Xiaomi/Redmi laptops",
        );
        if let Err(err) = res.compile() {
            println!("cargo:warning=winresource compile failed: {err}");
        }
    }
}
