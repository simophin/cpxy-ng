//! Generates the Kotlin and Swift bindings from the built library, e.g.
//!
//! ```sh
//! cargo build -p mobile-engine --lib
//! cargo run -p mobile-engine --features bindgen --bin uniffi-bindgen -- \
//!     generate --library target/debug/libmobile_engine.so --language kotlin --out-dir <dir>
//! ```

fn main() {
    uniffi::uniffi_bindgen_main()
}
