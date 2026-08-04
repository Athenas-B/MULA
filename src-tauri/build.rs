fn main() {
    let now = chrono::Local::now();
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", now.format("%Y-%m-%d %H:%M"));
    tauri_build::build()
}
