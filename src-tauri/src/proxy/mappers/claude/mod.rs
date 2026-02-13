// Claude mapper module
// Handles Claude/Anthropic ↔ Gemini protocol conversion
//
// Requirements covered:
// - 2.2: Anthropic Messages → Gemini generateContent
// - 2.15: /v1/messages/count_tokens

pub mod models;
pub mod request;
pub mod response;
pub mod streaming;

pub use models::*;
pub use request::{
    clean_cache_control_from_messages, merge_consecutive_messages, transform_claude_request,
    estimate_token_count,
};
pub use response::transform_response;
pub use streaming::{create_claude_sse_stream, PartProcessor, StreamingState};
