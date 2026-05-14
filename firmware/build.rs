use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    // NASM-assemble the DOS Stage 0 binaries we embed via
    // `include_bytes!`. Keeps the .COM out of git (the .asm under
    // dos/stage0/ is the canonical artifact) and makes the firmware
    // build self-contained as long as `nasm` is on the PATH.
    assemble_stage0("s0_at.asm", out.join("s0_at.bin"));
    assemble_stage0("s0_xt.asm", out.join("s0_xt.bin"));

    println!("cargo:rerun-if-changed=../dos/stage0/s0_at.asm");
    println!("cargo:rerun-if-changed=../dos/stage0/s0_xt.asm");
    println!("cargo:rerun-if-changed=../dos/stage0/s0_atps2_core.inc");
    println!("cargo:rerun-if-changed=../dos/stage0/lpt_nibble.inc");
}

fn assemble_stage0(src_basename: &str, dst: PathBuf) {
    // NASM runs from `../dos/` so the `%include "stage0/..."` paths
    // inside the .asm resolve correctly.
    let dst_str = dst.to_str().expect("output path is UTF-8");
    let src_rel = format!("stage0/{src_basename}");
    let status = Command::new("nasm")
        .current_dir("../dos")
        .args(["-f", "bin", "-o", dst_str, &src_rel])
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("nasm failed for {src_rel} with status {s}"),
        Err(e) => panic!(
            "failed to invoke nasm for {src_rel}: {e}. Install NASM \
             (e.g. `zb install nasm`) to build the firmware."
        ),
    }
}
