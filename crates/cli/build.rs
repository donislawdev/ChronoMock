//! Build script: embed the application icon (the red kidney bean) into chrono.exe on Windows.
//! winresource drives the Windows SDK resource compiler (rc.exe). It is a build-time dependency
//! only - no winresource code ships in the binary, so it needs no third-party notice, like cc.
//! The .ico is the shared branding asset under the repo's assets/ directory.

fn main() {
    // A build script runs on the host, so gate on the TARGET os, never a host cfg.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("assets")
        .join("chrono.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon path is valid UTF-8"));

    // Version-resource strings (the exe's Details tab in Explorer, and Task Manager). The version
    // numbers come from CARGO_PKG_VERSION, which winresource reads by default - it tracks the
    // workspace version in the root Cargo.toml, so a release bump there flows through with no edit
    // here. Only the identity strings, which have no Cargo equivalent, are set explicitly.
    res.set("ProductName", "Chrono Mock");
    res.set("FileDescription", "Chrono Mock CLI - fake-date app runner and test-date calculator");
    res.set("CompanyName", "DonislawDev");
    res.set("LegalCopyright", "Copyright (C) 2026 DonislawDev. Licensed under GPL-3.0-only.");
    res.set("OriginalFilename", "chrono.exe");

    res.compile().expect("embed the application icon and version resource into chrono.exe");
}
