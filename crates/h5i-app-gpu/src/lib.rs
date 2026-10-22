#![doc = include_str!("../README.md")]
//! Optional execution of pure application computations. Application kernels and
//! their verification remain independent of CUDA. Layouts and device entry
//! points belong to applications; this crate never commits application state.
pub mod batch;
pub mod differential;
pub mod repr;
pub mod runtime;
