// Handlers module - API endpoint processors
//
// Requirements covered:
// - 2.1: OpenAI /v1/chat/completions
// - 2.2: Claude /v1/messages
// - 2.3: Gemini /v1beta/models/:model
// - 2.10: /v1/images/generations
// - 2.11: /v1/audio/transcriptions
// - 2.12: /v1/models
// - 2.13: /v1/completions
// - 2.14: /v1/images/edits
// - 2.15: /v1/messages/count_tokens

pub mod admin;
pub mod audio;
pub mod claude;
pub mod common;
pub mod gemini;
pub mod openai;
pub mod warmup;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::proxy::token_manager::TokenManager;
use crate::proxy::upstream::client::UpstreamClient;

/// Shared application state for Axum handlers
#[derive(Clone)]
pub struct AppState {
    pub token_manager: Arc<TokenManager>,
    pub custom_mapping: Arc<RwLock<HashMap<String, String>>>,
    pub upstream: Arc<UpstreamClient>,
}

impl AppState {
    pub fn new(
        token_manager: Arc<TokenManager>,
        custom_mapping: Arc<RwLock<HashMap<String, String>>>,
        upstream: Arc<UpstreamClient>,
    ) -> Self {
        Self {
            token_manager,
            custom_mapping,
            upstream,
        }
    }
}
