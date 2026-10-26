// Embeds the application icon and version details in Relay.exe.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/icon.ico")
            .set("ProductName", "Relay")
            .set("FileDescription", "Relay remote workspace");
        if let Err(error) = resource.compile() {
            panic!("Cannot embed the Relay icon: {error}");
        }
    }
}
