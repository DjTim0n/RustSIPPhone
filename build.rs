// Embeds the icon into the .exe on Windows. Does nothing on other systems.
fn main() {
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("ProductName", "RustSIPPhone");
        resource.set("FileDescription", "RustSIPPhone");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=could not embed the icon: {err}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
