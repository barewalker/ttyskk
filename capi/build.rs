//! C のヘッダを cbindgen で作る。
//!
//! 出力は `capi/ttyskk.h`。**リポジトリに置いたまま**にしてあるので、C++ 側は
//! cargo を通さずに読める (fcitx5 の addon は CMake でビルドするため)。手で書くと
//! 実装とずれるので、ここで作って中身の食い違いを無くす。

fn main() {
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo が渡す");
    let out = std::path::Path::new(&dir).join("ttyskk.h");

    // 生成できなくてもビルドは通す。ヘッダが要るのは C++ 側だけで、
    // Rust のテストや実行ファイルには関係しない。
    match cbindgen::generate(&dir) {
        Ok(bindings) => {
            bindings.write_to_file(&out);
        }
        Err(e) => println!("cargo:warning=ヘッダを作れなかった: {e}"),
    }
}
