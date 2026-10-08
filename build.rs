fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // On Windows, only moving items to the Recycle Bin needs these DLLs, but
    // loading them at startup took 2.6 ms of a 7 ms start on the CI runner.
    // Delay loading maps each one on its first call instead.
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc {
        for dll in [
            "combase.dll",
            "ole32.dll",
            "oleaut32.dll",
            "propsys.dll",
            "shell32.dll",
            "user32.dll",
        ] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
