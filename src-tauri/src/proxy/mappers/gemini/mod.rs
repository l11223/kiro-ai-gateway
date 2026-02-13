// Gemini mapper module
// Handles Gemini native format passthrough with v1internal wrapping/unwrapping
//
// Requirements covered:
// - 2.3: Gemini native format passthrough via /v1beta/models/:model

pub mod collector;
pub mod models;
pub mod wrapper;

pub use wrapper::*;
