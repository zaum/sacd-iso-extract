fn main() {
    // Embed the disc icon into the Windows binary so Explorer and the taskbar
    // show the same mark as the GUI. Skipped on other platforms.
    #[cfg(windows)]
    {
        let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("resources")
            .join("icon.ico");
        if ico.exists() {
            let mut res = winres::WindowsResource::new();
            if let Some(path) = ico.to_str() {
                res.set_icon(path);
            }
            res.compile().expect("failed to embed Windows resources");
        } else {
            println!("cargo:warning=icon not found at {}, skipping", ico.display());
        }
    }
    println!("cargo:rerun-if-changed=../resources/icon.ico");
}
