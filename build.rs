extern crate gcc;

fn main() {
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=CFNetwork");
    gcc::compile_library("libtls.a", &["src/tls.c"]);
}