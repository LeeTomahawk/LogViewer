//! Embeds the icon and version information into logview.exe on Windows
//! (needs `rc.exe` from the Windows SDK, which the MSVC toolchain installs).

fn main() {
    println!("cargo:rerun-if-changed=../../assets/logview.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/logview.ico")
        .set("ProductName", "logview")
        .set(
            "FileDescription",
            "logview – przeglądarka dużych plików logów",
        )
        .set("OriginalFilename", "logview.exe")
        .set("LegalCopyright", "MIT");
    if let Err(e) = res.compile() {
        // A missing resource compiler only costs the icon; do not fail the build.
        println!("cargo:warning=icon not embedded: {e}");
    }
}
