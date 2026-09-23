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

    #[cfg(target_env="msvc")]
    {
        println!("cargo:rustc-link-lib=static=libusb-1.0");

        let profile = std::env::var("PROFILE").unwrap_or_default();
        if profile == "debug" {
            println!("cargo:rustc-link-lib=libucrtd");
        } else {
            println!("cargo:rustc-link-arg=/DEFAULTLIB:ucrt");
            println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrt");
        }
    }
    #[cfg(target_vendor="apple")]
    {

        println!("cargo:rustc-link-lib=static=usb-1.0");
        println!("cargo:rustc-link-lib=c++");
    }

    println!("cargo:rerun-if-changed=c_src");
}
