fn main() {
    let app_manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../deskunion-app/Cargo.toml");
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    let manifest = std::fs::read_to_string(app_manifest).expect("read application manifest");
    let package = manifest
        .split("[package]")
        .nth(1)
        .expect("application package section");
    let version = package
        .lines()
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "version").then(|| value.trim().trim_matches('"'))
        })
        .expect("application package version");
    println!("cargo:rustc-env=DESKUNION_APP_VERSION={version}");
    // composite_templates
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.gresource.xml",
        "deskunion.gresource",
    );
}
