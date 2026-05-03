// build.rs — locate the sandbox dynamic-library at build time and bake the
// expected path into the binary as a compile-time env var.
//
// The dylib (`librage_sandbox.dylib` on macOS) is produced by the
// `sandbox-macos-dylib` workspace crate, and the DLL (`rage_sandbox.dll` on
// Windows) by the `sandbox-windows-detours` workspace crate.  Both are
// *sibling* crates — NOT direct Cargo dependencies of this crate.  We must not
// link against them.  Instead, we compute the path Cargo would place the
// output artifact at and pass it through as a compile-time env var.  Consumers
// may override this at runtime by setting the corresponding `RAGE_SANDBOX_*`
// runtime variable.
//
// Each env var is emitted only for the platform that actually uses it:
//   RAGE_SANDBOX_DYLIB_DEFAULT  — macOS only
//   RAGE_SANDBOX_DLL_DEFAULT    — Windows only

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set by Cargo");
    let out_path = std::path::Path::new(&out_dir);

    // OUT_DIR has the form:
    //   .../target/<profile>/build/sandbox-<hash>/out
    //
    // ancestors().nth(3) steps back three levels to land on:
    //   .../target/<profile>/
    let profile_dir = out_path
        .ancestors()
        .nth(3)
        .expect("OUT_DIR does not have the expected directory depth");

    // Use CARGO_CFG_TARGET_OS (the *target* OS, not the host) so that
    // cross-compilation (e.g. macOS host → Windows target) emits the right var.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    match target_os.as_str() {
        "macos" => {
            let dylib = profile_dir.join("librage_sandbox.dylib");
            println!(
                "cargo:rustc-env=RAGE_SANDBOX_DYLIB_DEFAULT={}",
                dylib.display()
            );
        }
        "windows" => {
            // lib name is `rage_sandbox` per
            // crates/sandbox-windows-detours/Cargo.toml [lib] name = "rage_sandbox"
            let dll = profile_dir.join("rage_sandbox.dll");
            println!(
                "cargo:rustc-env=RAGE_SANDBOX_DLL_DEFAULT={}",
                dll.display()
            );
        }
        _ => {
            // Linux (eBPF) and other platforms do not need a baked artifact path.
        }
    }
}
