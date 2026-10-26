//! Generates the Kotlin bindings from the built library, e.g.
//!
//! ```sh
//! cargo build -p rtp-mobile --lib
//! cargo run -p rtp-mobile --features bindgen --bin uniffi-bindgen -- \
//!     generate --library target/debug/librtp_mobile.so --language kotlin --out-dir <dir>
//! ```

fn main() {
    uniffi::uniffi_bindgen_main()
}
