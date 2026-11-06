fn main() {
    println!("cargo:rerun-if-env-changed=QSCOPE_RESOURCES_ARCHIVE");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    let destination = output.join("resources.zip");
    if let Some(path) = std::env::var_os("QSCOPE_RESOURCES_ARCHIVE") {
        println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&path).display()
        );
        std::fs::copy(path, destination).expect("Unable to embed product resources");
    } else {
        std::fs::write(destination, []).expect("Unable to initialize development resources");
    }
}
