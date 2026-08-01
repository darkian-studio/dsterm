#[cfg(feature = "llama")]
fn build_llama() {
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    let mut cfg = cmake::Config::new("third_party");
    cfg.profile(&profile)
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("BUILD_SHARED_LIBS", "OFF")
        // OpenMP adds a libomp runtime dependency (only a static lib on
        // Termux, absent elsewhere); ggml-cpu threads fine without it.
        .define("GGML_OPENMP", "OFF");
    if std::process::Command::new("ninja")
        .arg("--version")
        .output()
        .is_ok()
    {
        cfg.generator("Ninja");
    }

    // The cmake crate builds serially otherwise; llama.cpp is a large C++ tree.
    std::env::set_var("CMAKE_BUILD_PARALLEL_LEVEL", "4");

    let dst = cfg.build();

    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    // Order matters for static archives: each lib must be scanned before the
    // libs that satisfy its unresolved symbols.
    println!("cargo:rustc-link-lib=static=dsterm_shim");
    println!("cargo:rustc-link-lib=static=dsterm_common");
    println!("cargo:rustc-link-lib=static=llama");
    println!("cargo:rustc-link-lib=static=ggml-cpu");
    println!("cargo:rustc-link-lib=static=ggml-base");
    println!("cargo:rustc-link-lib=static=ggml");

    // Rust never links a C++ standard library by default; the llama.cpp
    // archives are C++.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        // On Android, `-lc++` resolves to the system /system/lib64/libc++.so,
        // which exports the legacy `std::__1` ABI while llama.cpp is compiled
        // against the NDK `std::__ndk1` ABI. `libc++_shared.so` exports it.
        "android" => println!("cargo:rustc-link-lib=c++_shared"),
        "macos" | "ios" => println!("cargo:rustc-link-lib=c++"),
        "linux" => println!("cargo:rustc-link-lib=stdc++"),
        _ => {}
    }
}

fn main() {
    #[cfg(feature = "llama")]
    build_llama();
}
