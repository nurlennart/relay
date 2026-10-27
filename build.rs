// Embeds the application icon and version details in Relay.exe, and the commit Relay is
// built from, which Settings shows beside the version.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    commit();
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

/// Passes the short hash of the checked-out commit as RELAY_COMMIT, when building from git.
fn commit() {
    for path in [".git/HEAD", ".git/refs/heads", ".git/packed-refs"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    if let Some(hash) = hash.map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) {
        println!("cargo:rustc-env=RELAY_COMMIT={hash}");
    }
}
