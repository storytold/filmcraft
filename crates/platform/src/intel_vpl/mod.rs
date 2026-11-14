//! Installed Intel VPL identity/session integration, Windows x64 MSVC.
//!
//! The declaration boundary alone does not load a driver or advertise codec support.

#[cfg_attr(test, allow(unsafe_code))] // Union-read tests stay within the FFI boundary (AGENTS.md §0.3).
mod ffi;

#[cfg(test)]
mod abi_tests;
