//! Embeds the icon and file details into the Windows exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo/exports/kantan.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // The build script runs on Windows even when building for the web, so check the target.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/logo/exports/kantan.ico")
            .set("ProductName", "Kantan Audio Manager")
            .set("FileDescription", "Kantan Audio Manager")
            .set("CompanyName", "Kingdom Pots");
        resource.compile().expect("embedding the exe icon failed");
    }
}
