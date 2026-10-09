//! Compile the two vendored parsers and their external scanners, as upstream's own build script
//! does.

use std::path::Path;

fn main() {
    let common = Path::new("common");
    let mut build = cc::Build::new();
    build
        .include("typescript/src")
        .flag_if_supported("-std=c11")
        .flag_if_supported("-Wno-unused-parameter");

    for language in ["typescript", "tsx"] {
        let src = Path::new(language).join("src");
        for file in ["parser.c", "scanner.c"] {
            let path = src.join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            build.file(path);
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        common.join("scanner.h").display()
    );

    build.compile("lanekeep-tree-sitter-typescript");
}
