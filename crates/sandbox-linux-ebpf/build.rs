//! Build script for `sandbox-linux-ebpf`.
//!
//! On Linux: compiles the `sandbox-linux-ebpf-prog` eBPF kernel crate using
//! the `bpfel-unknown-none` target and embeds the resulting object file so
//! that `src/linux.rs` can use `include_bytes_aligned!` to load it.
//!
//! On non-Linux: does nothing (the eBPF paths are cfg-guarded).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os != "linux" {
        // No eBPF compilation on non-Linux; stub module handles the API.
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prog_dir = manifest_dir
        .parent()
        .expect("expected parent of sandbox-linux-ebpf")
        .join("sandbox-linux-ebpf-prog");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Tell Cargo to rebuild if the eBPF source changes.
    println!("cargo:rerun-if-changed={}", prog_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        prog_dir.join("Cargo.toml").display()
    );

    // Build the eBPF kernel program.
    // Requires: nightly Rust toolchain + `rust-src` component.
    let status = Command::new("cargo")
        .args([
            "+nightly",
            "build",
            "--release",
            "-Z",
            "build-std=core",
            "--target",
            "bpfel-unknown-none",
            "--manifest-path",
            prog_dir.join("Cargo.toml").to_str().unwrap(),
            "--target-dir",
            out_dir.to_str().unwrap(),
        ])
        .status()
        .expect("failed to spawn cargo to build eBPF program");

    assert!(
        status.success(),
        "cargo build of sandbox-linux-ebpf-prog failed"
    );

    // Copy the resulting ELF object to the expected path.
    let obj_src = out_dir
        .join("bpfel-unknown-none")
        .join("release")
        .join("sandbox-linux-ebpf-prog");

    let obj_dst = out_dir.join("sandbox-linux-ebpf-prog");

    std::fs::copy(&obj_src, &obj_dst).expect("failed to copy eBPF object to OUT_DIR");
}
