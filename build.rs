fn main() {
    // A .def file guarantees *undecorated* export names. Rust emits `_Name@N`
    // for `extern "system"` on i686-msvc; link.exe matches the plain names in
    // the .def against those decorated symbols and exports them undecorated,
    // which is what the game's import table expects.
    let def = format!("{}/src/exports.def", env!("CARGO_MANIFEST_DIR"));
    println!("cargo:rustc-cdylib-link-arg=/DEF:{def}");
    // LNK4222: link.exe objects to ordinals on the COM registration exports.
    // Matching the system DLL's ordinals is the whole point here.
    println!("cargo:rustc-cdylib-link-arg=/IGNORE:4222");
    println!("cargo:rerun-if-changed=src/exports.def");
}
