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
    res.compile().expect("embed the application icon into chrono.exe");
}
