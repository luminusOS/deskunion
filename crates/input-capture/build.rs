fn main() {
    let target_family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = target_family.split(',').any(|family| family == "unix");
    let layer_shell = cfg!(feature = "layer_shell");
    let libei = cfg!(feature = "libei");
    let x11 = cfg!(feature = "x11");
    let macos = target_os == "macos";

    let libei = unix && !macos && libei;
    let layer_shell = unix && !macos && layer_shell;
    let x11 = unix && !macos && x11;

    println!("cargo::rustc-check-cfg=cfg(layer_shell)");
    println!("cargo::rustc-check-cfg=cfg(libei)");
    println!("cargo::rustc-check-cfg=cfg(x11)");

    if layer_shell {
        println!("cargo::rustc-cfg=layer_shell");
    }
    if libei {
        println!("cargo::rustc-cfg=libei");
    }
    if x11 {
        println!("cargo::rustc-cfg=x11");
    }
}
