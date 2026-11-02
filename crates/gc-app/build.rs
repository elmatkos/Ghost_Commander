//! Embeds the application icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Optional DLLs missing from WinPE: load them on first use so the exe still starts.
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
        for dll in ["dxgi.dll", "tbs.dll"] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        winresource::WindowsResource::new()
            .set_icon("../../assets/icon.ico")
            .set("ProductName", "Ghost Commander")
            .set("FileDescription", "Ghost Commander - dual-pane file manager")
            .compile()
            .expect("failed to embed Windows resources (is the Windows SDK rc.exe installed?)");
    }
}
