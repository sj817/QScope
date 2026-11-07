fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .flag("/utf-8")
        .flag("/EHsc")
        .file("native/session.cpp")
        .file("native/guard.cpp")
        .compile("qscope-session");
    for library in ["advapi32", "user32", "shell32", "ole32", "uuid", "ws2_32"] {
        println!("cargo:rustc-link-lib={library}");
    }
    for file in ["native/session.cpp", "native/guard.cpp"] {
        println!("cargo:rerun-if-changed={file}");
    }
}
