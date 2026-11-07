fn main() {
    // Embed a manifest that opts the GUI into v6 common controls (modern look).
    println!("cargo:rustc-link-arg-bin=ShadowPlay_Patcher_GUI=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=ShadowPlay_Patcher_GUI=/MANIFESTDEPENDENCY:type='win32' \
         name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
         processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
}
