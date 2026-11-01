fn main() {
    println!(
        "cargo:rustc-env=GROW_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    tauri_build::build()
}
