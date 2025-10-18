#[cfg(windows)]
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon("icon.ico");

    if let Err(e) = res.compile() {
        eprintln!("cargo:warning=Failed to embed Windows icon: {e}");
    }
}

#[cfg(not(windows))]
fn main() {}
