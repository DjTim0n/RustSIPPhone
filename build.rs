// Встраивает иконку в .exe на Windows. На других системах ничего не делает.
fn main() {
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("ProductName", "Телефон");
        resource.set("FileDescription", "Телефон");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=не удалось встроить иконку: {err}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
