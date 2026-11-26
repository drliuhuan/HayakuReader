fn main() {
    let target = std::env::var("TARGET").expect("Cargo target");
    if !target.ends_with("windows-msvc") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-changed=assets/windows.rc");
    let resource =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"))
            .join("HayakuReader.res");
    let mut compiler = cc::windows_registry::find_tool(&target, "rc.exe").map_or_else(
        || std::process::Command::new("rc.exe"),
        |tool| tool.to_command(),
    );
    let status = compiler
        .args(["/nologo", "/fo"])
        .arg(&resource)
        .arg("assets/windows.rc")
        .status()
        .expect("run the Windows SDK resource compiler");
    assert!(status.success(), "could not compile the HayakuReader icon");
    println!("cargo:rustc-link-arg={}", resource.display());
}
