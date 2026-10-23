// Windows: puts the icon and version into the .exe (what Explorer, the taskbar and the
// installer show). Nothing to do elsewhere.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "Banana Photos");
        res.set("ProductName", "Banana Photos");
        res.compile().expect("couldn't add the icon to the .exe");
    }
}
