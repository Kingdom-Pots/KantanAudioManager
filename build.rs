//! Embeds the icon and file details into the Windows exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo/exports/kantan.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/logo/exports/kantan.ico")
            .set("ProductName", "Kantan Audio Manager")
            .set("FileDescription", "Kantan Audio Manager")
            .set("CompanyName", "Kingdom Pots");
        resource.compile().expect("embedding the exe icon failed");
    }
}
