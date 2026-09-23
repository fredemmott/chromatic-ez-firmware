fn main() {
    let dst = cmake::Config::new("c_src").build();

    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=openFPGAloader-chromatic");
    println!("cargo:rustc-link-lib=static=openFPGAloader");
    println!("cargo:rustc-link-lib=static=libusb-1.0");

    #[cfg(target_env="msvc")]
    {
        let profile = std::env::var("PROFILE").unwrap_or_default();
        if profile == "debug" {
            println!("cargo:rustc-link-lib=ucrtd");
        } else {
            println!("cargo:rustc-link-lib=ucrt");
        }
    }

    println!("cargo:rerun-if-changed=c_src");
}
