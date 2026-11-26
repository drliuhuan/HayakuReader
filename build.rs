fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/logo.ico");
        println!("cargo:rerun-if-changed=assets/windows.rc");
        embed_resource::compile("assets/windows.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compile the HayakuReader Windows icon");
    }
}
