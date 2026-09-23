fn main() {
    let mut config= cmake::Config::new("c_src");

    #[cfg(target_env="msvc")]
    {
        config.define("CMAKE_MSVC_RUNTIME_LIBRARY","MultiThreaded$<$<CONFIG:Debug>:Debug>");
        config.cflag("/MT$<$<CONFIG:Debug>:d>");
        config.cxxflag("/MT$<$<CONFIG:Debug>:d>");
    }

    let dst = config.build();

    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-lib=static=openFPGAloader-chromatic");
    println!("cargo:rustc-link-lib=static=openFPGAloader");
    println!("cargo:rustc-link-lib=static=libusb-1.0");

    #[cfg(target_env="msvc")]
    {
        let profile = std::env::var("PROFILE").unwrap_or_default();
        if profile == "debug" {
            println!("cargo:rustc-link-lib=libucrtd");
        } else {
            println!("cargo:rustc-link-arg=/DEFAULTLIB:ucrt");
            println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrt");
        }
    }

    println!("cargo:rerun-if-changed=c_src");
}
