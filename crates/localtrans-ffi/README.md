# localtrans-ffi

uniFFI bindings for LocalTrans Android client.

## Build chain

This crate uses uniFFI 0.28 proc-macro mode (no UDL files required).

### Host tests
```bash
cargo test -p localtrans-ffi
```

### Android cross-compile (via cargo-ndk)
```bash
export ANDROID_NDK_HOME="C:/Users/<user>/Desktop/work/deepseek_use/tools/android-sdk/ndk/27.0.12077973"
cargo ndk -t arm64-v8a -t x86_64 -o ./target/android cargo build -p localtrans-ffi --release
```

Output:
- `target/android/arm64-v8a/liblocaltrans_ffi.so`
- `target/android/x86_64/liblocaltrans_ffi.so`

### Generate Kotlin bindings
```bash
cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate --library target/android/x86_64/liblocaltrans_ffi.so --language kotlin --out-dir ../android-uniffi-tmp
```

This generates:
- `uniffi/localtrans/localtrans.kt`
- `uniffi/localtrans/AppEvent.kt`
- `uniffi/localtrans/LocalTransApp.kt`
- `uniffi/localtrans/LocalTransCallback.kt`
