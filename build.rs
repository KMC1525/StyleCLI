fn main() {
    // Expõe o triple de compilação para o `style update` escolher o asset certo do release.
    println!(
        "cargo:rustc-env=STYLE_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
