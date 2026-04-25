// build.rs — locate the sandbox dylib at build time and bake the expected path
// into the binary as an environment variable.
//
// The dylib (`librage_sandbox.dylib`) is produced by the `sandbox-macos-dylib`
// workspace crate, which is a *sibling* crate — NOT a direct Cargo dependency
// of this crate.  We must not link against it.  Instead, we compute the path
// to where Cargo would place the output artifact and pass it through as a
// compile-time env var.  Consumers may override this at runtime by setting
// the `RAGE_SANDBOX_DYLIB` environment variable.

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

    let dylib = profile_dir.join("librage_sandbox.dylib");

    println!(
        "cargo:rustc-env=RAGE_SANDBOX_DYLIB_DEFAULT={}",
        dylib.display()
    );
}
