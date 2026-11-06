fn main() {
    println!("cargo:rerun-if-env-changed=QSCOPE_BUILD_ID");
    println!(
        "cargo:rustc-env=QSCOPE_BUILD_ID={}",
        std::env::var("QSCOPE_BUILD_ID").unwrap_or_else(|_| "development".into())
    );
}
