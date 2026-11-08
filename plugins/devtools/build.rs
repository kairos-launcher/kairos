fn main() {
    // Version info (product, description, original file name) and the icon. Unsigned binaries
    // without any of it look like malware to Defender's heuristics.
    println!("cargo:rerun-if-changed=../../packaging/kairos.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/kairos.ico")
            .set("ProductName", "Kairos")
            .set("FileDescription", "Kairos devtools plugin")
            .set("CompanyName", "Kairos")
            .set("LegalCopyright", "MIT License")
            .set("InternalName", "kairos-devtools")
            .set("OriginalFilename", "kairos-devtools.exe");
        if let Err(e) = res.compile() {
            panic!("embedding the Windows resources: {e}");
        }
    }
}
