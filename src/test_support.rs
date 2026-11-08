//! Helpers shared by unit tests.

/// A fresh temporary folder, deleted when the returned guard drops (also when a test panics).
pub fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("kairos-test-").tempdir().expect("create a temporary folder")
}

/// Whether `python3` can run the example plugins. Missing locally: the test is skipped with a
/// note. Missing on CI (`CI` is set): the test fails, so a skipped suite cannot pass silently.
pub fn python3() -> bool {
    let ok = std::process::Command::new("python3").arg("--version").output().is_ok();
    if !ok {
        assert!(std::env::var_os("CI").is_none(), "python3 is required for this test on CI");
        eprintln!("python3 missing; skipping");
    }
    ok
}

/// A 32×32 blue PNG written into `dir`, for tests that load an image from a path.
pub fn png_32(dir: &std::path::Path) -> std::path::PathBuf {
    use base64::Engine as _;
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAL0lEQVR42u3OIQEAAAgDMIpRjcw0gBg3E/Ornr2kEhAQEBAQEBAQEBAQEBAQSAceKNaol2HDjkgAAAAASUVORK5CYII=";
    let bytes = base64::engine::general_purpose::STANDARD.decode(PNG).expect("valid base64");
    let path = dir.join("square.png");
    std::fs::write(&path, bytes).expect("write test png");
    path
}
