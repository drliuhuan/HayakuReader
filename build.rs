use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

fn find_pdfium_dll() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("PDFIUM_DLL") {
        candidates.push(PathBuf::from(p));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let root = PathBuf::from(profile).join(".cache").join("pdfium-bundled");
        candidates.push(root.join("pdfium-8066").join("pdfium.dll"));
        if let Ok(entries) = fs::read_dir(&root) {
            for e in entries.flatten() {
                candidates.push(e.path().join("pdfium.dll"));
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("HayakuReader").join("pdfium");
        candidates.push(root.join("pdfium-8066").join("pdfium.dll"));
        candidates.push(root.join("pdfium.dll"));
    }
    if let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") {
        candidates.push(PathBuf::from(manifest).join("dist").join("pdfium.dll"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn embed_pdfium(out_dir: &Path) {
    println!("cargo:rustc-check-cfg=cfg(embedded_pdfium)");
    println!("cargo:rerun-if-env-changed=PDFIUM_DLL");
    let Some(src) = find_pdfium_dll() else {
        return;
    };
    println!("cargo:rerun-if-changed={}", src.display());
    let gz_path = out_dir.join("pdfium.dll.gz");
    let len_path = out_dir.join("pdfium.len");
    let raw_len = fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    if raw_len == 0 {
        return;
    }

    let need_recompress = !gz_path.is_file()
        || fs::read_to_string(&len_path).ok().as_deref() != Some(raw_len.to_string().as_str())
        || fs::metadata(&gz_path)
            .and_then(|m| m.modified())
            .ok()
            .zip(fs::metadata(&src).and_then(|m| m.modified()).ok())
            .is_some_and(|(gz_t, src_t)| src_t > gz_t);

    if need_recompress {
        let mut input = File::open(&src).expect("open cached pdfium.dll");
        let output = File::create(&gz_path).expect("create OUT_DIR/pdfium.dll.gz");
        let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::best());
        io::copy(&mut input, &mut encoder).expect("compress pdfium.dll");
        encoder.finish().expect("finish gzip stream");
        fs::write(&len_path, raw_len.to_string()).expect("write OUT_DIR/pdfium.len");
    }
    println!("cargo:rustc-cfg=embedded_pdfium");
}

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    let target = std::env::var("TARGET").expect("Cargo target");
    if !target.ends_with("windows-msvc") {
        println!("cargo:rustc-check-cfg=cfg(embedded_pdfium)");
        return;
    }
    embed_pdfium(&out_dir);

    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-changed=assets/windows.rc");
    let resource = out_dir.join("HayakuReader.res");
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
