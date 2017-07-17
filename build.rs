extern crate gcc;

fn main() {
    gcc::compile_library("libtls.a", &["src/tls.c"]);
}