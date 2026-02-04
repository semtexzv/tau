//! tau-rt: Shared async runtime library.
//!
//! This crate is compiled as a cdylib ONLY. All access is through the C ABI
//! functions exported in `ffi.rs`. No Rust crate can `use tau_rt::*` directly —
//! they must use `tau-iface` which declares the extern functions.
//!
//! The reactor and executor are process-global singletons (behind OnceLock),
//! shared by the host binary and all plugin cdylibs through dynamic linking.

mod executor;
mod ffi;
mod reactor;
