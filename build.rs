fn main() {
    if let Err(e) = slint_build::compile("ui/main.slint") {
        panic!("ui/main.slint: {e}");
    }
    // Explorer, the taskbar and shortcuts take the icon from the exe's resources.
    // Regenerate it with `cargo run --example gen_icons`.
    println!("cargo:rerun-if-changed=packaging/kairos.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/kairos.ico");
        if let Err(e) = res.compile() {
            panic!("embedding the Windows icon: {e}");
        }
    }
}
