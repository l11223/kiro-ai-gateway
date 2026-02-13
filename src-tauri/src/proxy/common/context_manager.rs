//! Context Manager Module
//!
//! Responsible for multi-level context compression (L1/L2/L3) to prevent
//! "Prompt is too long" errors and manage token budgets.
//!
//! Requirements covered:
//! - 9.5: L1 (tool result trimming), L2 (thinking compression), L3 (fork + summary)

use crate::proxy::mappers::claude::models::{
    ClaudeRequest, ContentBlock, Message, MessageContent, SystemPrompt,
};
use tracing::{debug, info};

/// Estimate tokens from text with multi-language awareness.
///
/// Algorithm:
/// - ASCII/English: ~4 characters per token
/// - Unicode/CJK: ~1.5 characters per token
/// - Adds 15% safety margin
fn estimate_tokens_from_str(s: &str) -> u32 {
    if s.is_empty() {
        return 0;
    }

    let mut ascii_chars = 0u32;
    let mut unicode_chars = 0u32;

    for c in s.chars() {
        if c.is_ascii() {
            ascii_chars += 1;
        } else {
            unicode_chars += 1;
        }
    }

    let ascii_tokens = (ascii_chars as f32 / 4.0).ceil() as u32;
    let unicode_tokens = (unicode_chars as f32 / 1.5).ceil() as u32;

    ((ascii_tokens + unicode_tokens) as f32 * 1.15).ceil() as u32
}

/// Strategy for context purification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurificationStrategy {
    /// Soft: retains recent thinking blocks (~2 turns), removes older ones
    Soft,
    /// Aggressive: removes ALL thinking blocks to save maximum tokens
    Aggressive,
}

/// Context Manager - handles multi-level context compression
pub struct ContextManager;

impl ContextManager {
    /// Estimate token usage for a Claude request.
    ///
    /// Lightweight estimation (not precise count). Iterates through all messages
    /// and blocks to sum up estimated tokens.
    pub fn estimate_token_usage(request: &ClaudeRequest) -> u32 {
        let mut total = 0;

        // System prompt
        if let Some(sys) = &request.system {
            match sys {
                SystemPrompt::String(s) => total += estimate_tokens_from_str(s),
                SystemPrompt::Array(blocks) => {
                    for block in blocks {
                        total += estimate_tokens_from_str(&block.text);
                    }
                }
            }
        }

        // Messages
        for msg in &request.messages {
            total += 4; // Message overhead

            match &msg.content {
                MessageContent::String(s) => {
                    total += estimate_tokens_from_str(s);
                }
                MessageContent::Array(blocks) => {
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                total += estimate_tokens_from_str(text);
                            }
                            ContentBlock::Thinking {
                                thinking,
                                signature,
                                ..
                            } => {
                                total += estimate_tokens_from_str(thinking);
                                if signature.is_some() {
                                    total += 100; // Signature overhead
                                }
                            }
                            ContentBlock::RedactedThinking { data } => {
                                total += estimate_tokens_from_str(data);
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                total += 20; // Function call overhead
                                total += estimate_tokens_from_str(name);
                                if let Ok(json_str) = serde_json::to_string(input) {
                                    total += estimate_tokens_from_str(&json_str);
                                }
                            }
                            ContentBlock::ToolResult { content, .. } => {
                                total += 10; // Result overhead
                                if let Some(s) = content.as_str() {
                                    total += estimate_tokens_from_str(s);
                                } else if let Some(arr) = content.as_array() {
                                    for item in arr {
                                        if let Some(text) =
                                            item.get("text").and_then(|t| t.as_str())
                                        {
                                            total += estimate_tokens_from_str(text);
                                        }
                                    }
                                } else if let Ok(s) = serde_json::to_string(content) {
                                    total += estimate_tokens_from_str(&s);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Tools definition overhead
        if let Some(tools) = &request.tools {
            for tool in tools {
                if let Ok(json_str) = serde_json::to_string(tool) {
                    total += estimate_tokens_from_str(&json_str);
                }
            }
        }

        // Thinking budget overhead
        if let Some(thinking) = &request.thinking {
            if let Some(budget) = thinking.budget_tokens {
                total += budget;
            }
        }

        total
    }

    // ===== [Layer 1] Tool Message Intelligent Trimming =====
    // Removes old tool call/result pairs while preserving recent ones.
    // Does NOT break Prompt Cache (only removes messages, doesn't modify content).

    /// Trim old tool messages, keeping only the last N rounds.
    ///
    /// A "tool round" consists of:
    /// - An assistant message with tool_use
    /// - One or more user messages with tool_result
    ///
    /// Returns true if any messages were removed.
    pub fn trim_tool_messages(messages: &mut Vec<Message>, keep_last_n_rounds: usize) -> bool {
        let tool_rounds = identify_tool_rounds(messages);

        if tool_rounds.len() <= keep_last_n_rounds {
            return false;
        }

        let rounds_to_remove = tool_rounds.len() - keep_last_n_rounds;
        let mut indices_to_remove = std::collections::HashSet::new();

        for round in tool_rounds.iter().take(rounds_to_remove) {
            for idx in &round.indices {
                indices_to_remove.insert(*idx);
            }
        }

        let mut removed_count = 0;
        for idx in (0..messages.len()).rev() {
            if indices_to_remove.contains(&idx) {
                messages.remove(idx);
                removed_count += 1;
            }
        }

        if removed_count > 0 {
            info!(
                "[ContextManager] [Layer-1] Trimmed {} tool messages, kept last {} rounds",
                removed_count, keep_last_n_rounds
            );
        }

        removed_count > 0
    }

    // ===== [Layer 2] Thinking Content Compression + Signature Preservation =====
    // Compresses thinking text but PRESERVES signatures.
    // Signature chain remains intact, tool calls won't break.

    /// Compress thinking content while preserving signatures.
    ///
    /// 1. Keeps signatures intact (critical for tool call chain)
    /// 2. Compresses thinking text to "..." placeholder
    /// 3. Protects the last N messages from compression
    ///
    /// Returns true if any thinking blocks were compressed.
    pub fn compress_thinking_preserve_signature(
        messages: &mut Vec<Message>,
        protected_last_n: usize,
    ) -> bool {
        let total_msgs = messages.len();
        if total_msgs == 0 {
            return false;
        }

        let start_protection_idx = total_msgs.saturating_sub(protected_last_n);
        let mut compressed_count = 0;
        let mut total_chars_saved = 0;

        for (i, msg) in messages.iter_mut().enumerate() {
            if i >= start_protection_idx {
                continue;
            }

            if msg.role == "assistant" {
                if let MessageContent::Array(blocks) = &mut msg.content {
                    for block in blocks.iter_mut() {
                        if let ContentBlock::Thinking {
                            thinking,
                            signature,
                            ..
                        } = block
                        {
                            if signature.is_some() && thinking.len() > 10 {
                                let original_len = thinking.len();
                                *thinking = "...".to_string();
                                compressed_count += 1;
                                total_chars_saved += original_len - 3;
                            }
                        }
                    }
                }
            }
        }

        if compressed_count > 0 {
            let estimated_tokens_saved = (total_chars_saved as f32 / 3.5).ceil() as u32;
            info!(
                "[ContextManager] [Layer-2] Compressed {} thinking blocks (saved ~{} tokens, signatures preserved)",
                compressed_count, estimated_tokens_saved
            );
        }

        compressed_count > 0
    }

    // ===== [Layer 3 Helper] Extract Last Valid Signature =====

    /// Extract the last valid thinking signature from message history.
    ///
    /// Used by Layer 3 (Fork + Summary) to preserve the signature chain.
    /// Returns None if no valid signature found (length >= 50).
    pub fn extract_last_valid_signature(messages: &[Message]) -> Option<String> {
        for msg in messages.iter().rev() {
            if msg.role == "assistant" {
                if let MessageContent::Array(blocks) = &msg.content {
                    for block in blocks {
                        if let ContentBlock::Thinking {
                            signature: Some(sig),
                            ..
                        } = block
                        {
                            if sig.len() >= 50 {
                                debug!(
                                    "[ContextManager] [Layer-3] Extracted last valid signature (len: {})",
                                    sig.len()
                                );
                                return Some(sig.clone());
                            }
                        }
                    }
                }
            }
        }

        debug!("[ContextManager] [Layer-3] No valid signature found in history");
        None
    }

    /// Purify message history by removing thinking blocks.
    ///
    /// Unlike compression (Layer 2), this completely removes thinking blocks.
    /// Used when context is critical or signatures are invalid.
    pub fn purify_history(messages: &mut Vec<Message>, strategy: PurificationStrategy) -> bool {
        let protected_last_n = match strategy {
            PurificationStrategy::Soft => 4,
            PurificationStrategy::Aggressive => 0,
        };

        Self::strip_thinking_blocks(messages, protected_last_n)
    }

    /// Strip thinking blocks from messages outside the protected range.
    fn strip_thinking_blocks(messages: &mut Vec<Message>, protected_last_n: usize) -> bool {
        let total_msgs = messages.len();
        if total_msgs == 0 {
            return false;
        }

        let start_protection_idx = total_msgs.saturating_sub(protected_last_n);
        let mut modified = false;

        for (i, msg) in messages.iter_mut().enumerate() {
            if i >= start_protection_idx {
                continue;
            }

            if msg.role == "assistant" {
                if let MessageContent::Array(blocks) = &mut msg.content {
                    let original_len = blocks.len();
                    blocks.retain(|b| !matches!(b, ContentBlock::Thinking { .. }));

                    if blocks.len() != original_len {
                        modified = true;
                        debug!(
                            "[ContextManager] Stripped {} thinking blocks from message {}",
                            original_len - blocks.len(),
                            i
                        );
                    }
                }
            }
        }

        modified
    }
}

/// Represents a tool call round (assistant tool_use + user tool_result(s))
#[derive(Debug)]
struct ToolRound {
    _assistant_index: usize,
    indices: Vec<usize>,
}

/// Identify tool call rounds in the message history
fn identify_tool_rounds(messages: &[Message]) -> Vec<ToolRound> {
    let mut rounds = Vec::new();
    let mut current_round: Option<ToolRound> = None;

    for (i, msg) in messages.iter().enumerate() {
        match msg.role.as_str() {
            "assistant" => {
                if has_tool_use(&msg.content) {
                    if let Some(round) = current_round.take() {
                        rounds.push(round);
                    }
                    current_round = Some(ToolRound {
                        _assistant_index: i,
                        indices: vec![i],
                    });
                }
            }
            "user" => {
                if let Some(ref mut round) = current_round {
                    if has_tool_result(&msg.content) {
                        round.indices.push(i);
                    } else {
                        rounds.push(current_round.take().unwrap());
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(round) = current_round {
        rounds.push(round);
    }

    debug!(
        "[ContextManager] Identified {} tool rounds in {} messages",
        rounds.len(),
        messages.len()
    );

    rounds
}

fn has_tool_use(content: &MessageContent) -> bool {
    if let MessageContent::Array(blocks) = content {
        blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    } else {
        false
    }
}

fn has_tool_result(content: &MessageContent) -> bool {
    if let MessageContent::Array(blocks) = content {
        blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_request() -> ClaudeRequest {
        ClaudeRequest {
            model: "claude-3-5-sonnet".into(),
            messages: vec![],
            system: None,
            tools: None,
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            thinking: None,
            metadata: None,
        }
    }

    #[test]
    fn test_estimate_tokens_basic() {
        let mut req = create_test_request();
        req.messages = vec![Message {
            role: "user".into(),
            content: MessageContent::String("Hello World".into()),
        }];

        let tokens = ContextManager::estimate_token_usage(&req);
        assert!(tokens > 0);
        assert!(tokens < 50);
    }

    #[test]
    fn test_estimate_tokens_with_system() {
        let mut req = create_test_request();
        req.system = Some(SystemPrompt::String("You are a helpful assistant.".into()));
        req.messages = vec![Message {
            role: "user".into(),
            content: MessageContent::String("Hi".into()),
        }];

        let tokens = ContextManager::estimate_token_usage(&req);
        assert!(tokens > 5);
    }

    #[test]
    fn test_estimate_tokens_unicode() {
        // CJK characters should estimate more tokens per character
        let ascii_tokens = estimate_tokens_from_str("Hello World");
        let cjk_tokens = estimate_tokens_from_str("你好世界测试");

        // CJK should have higher token-per-char ratio
        let ascii_ratio = ascii_tokens as f32 / 11.0;
        let cjk_ratio = cjk_tokens as f32 / 6.0;
        assert!(cjk_ratio > ascii_ratio);
    }

    #[test]
    fn test_purify_history_soft() {
        let mut messages = vec![
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![
                    ContentBlock::Thinking {
                        thinking: "ancient thought".into(),
                        signature: None,
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "A0".into(),
                    },
                ]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::String("Q1".into()),
            },
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![
                    ContentBlock::Thinking {
                        thinking: "old thought".into(),
                        signature: None,
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "A1".into(),
                    },
                ]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::String("Q2".into()),
            },
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![
                    ContentBlock::Thinking {
                        thinking: "recent thought".into(),
                        signature: None,
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "A2".into(),
                    },
                ]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::String("current".into()),
            },
        ];

        ContextManager::purify_history(&mut messages, PurificationStrategy::Soft);

        // Message 0 (ancient): thinking should be stripped
        if let MessageContent::Array(blocks) = &messages[0].content {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(blocks[0], ContentBlock::Text { .. }));
        }

        // Message 2 (old): protected (index 2 >= 6-4=2)
        if let MessageContent::Array(blocks) = &messages[2].content {
            assert_eq!(blocks.len(), 2);
        }
    }

    #[test]
    fn test_purify_history_aggressive() {
        let mut messages = vec![Message {
            role: "assistant".into(),
            content: MessageContent::Array(vec![
                ContentBlock::Thinking {
                    thinking: "thought".into(),
                    signature: None,
                    cache_control: None,
                },
                ContentBlock::Text {
                    text: "text".into(),
                },
            ]),
        }];

        ContextManager::purify_history(&mut messages, PurificationStrategy::Aggressive);

        if let MessageContent::Array(blocks) = &messages[0].content {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(blocks[0], ContentBlock::Text { .. }));
        }
    }

    #[test]
    fn test_compress_thinking_preserve_signature() {
        let mut messages = vec![
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![
                    ContentBlock::Thinking {
                        thinking: "This is a long thinking block that should be compressed".into(),
                        signature: Some("sig-abc-123".into()),
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "response".into(),
                    },
                ]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::String("follow up".into()),
            },
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![
                    ContentBlock::Thinking {
                        thinking: "Recent thinking that should be protected".into(),
                        signature: Some("sig-def-456".into()),
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "recent response".into(),
                    },
                ]),
            },
        ];

        let modified =
            ContextManager::compress_thinking_preserve_signature(&mut messages, 2);
        assert!(modified);

        // First message thinking should be compressed
        if let MessageContent::Array(blocks) = &messages[0].content {
            if let ContentBlock::Thinking {
                thinking,
                signature,
                ..
            } = &blocks[0]
            {
                assert_eq!(thinking, "...");
                assert!(signature.is_some()); // Signature preserved
            }
        }

        // Last message thinking should be protected
        if let MessageContent::Array(blocks) = &messages[2].content {
            if let ContentBlock::Thinking { thinking, .. } = &blocks[0] {
                assert_ne!(thinking, "...");
            }
        }
    }

    #[test]
    fn test_extract_last_valid_signature() {
        let long_sig = "a".repeat(60); // >= 50 chars
        let messages = vec![
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![ContentBlock::Thinking {
                    thinking: "thought".into(),
                    signature: Some(long_sig.clone()),
                    cache_control: None,
                }]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::String("hi".into()),
            },
        ];

        let sig = ContextManager::extract_last_valid_signature(&messages);
        assert_eq!(sig, Some(long_sig));
    }

    #[test]
    fn test_extract_last_valid_signature_none() {
        let messages = vec![Message {
            role: "assistant".into(),
            content: MessageContent::Array(vec![ContentBlock::Thinking {
                thinking: "thought".into(),
                signature: Some("short".into()), // < 50 chars
                cache_control: None,
            }]),
        }];

        let sig = ContextManager::extract_last_valid_signature(&messages);
        assert!(sig.is_none());
    }

    #[test]
    fn test_trim_tool_messages() {
        let mut messages = vec![
            // Round 1
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "a.txt"}),
                    signature: None,
                    cache_control: None,
                }]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolResult {
                    tool_use_id: "1".into(),
                    content: serde_json::json!("content of a.txt"),
                    is_error: None,
                }]),
            },
            // Round 2
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolUse {
                    id: "2".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "b.txt"}),
                    signature: None,
                    cache_control: None,
                }]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolResult {
                    tool_use_id: "2".into(),
                    content: serde_json::json!("content of b.txt"),
                    is_error: None,
                }]),
            },
            // Round 3
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolUse {
                    id: "3".into(),
                    name: "write_file".into(),
                    input: serde_json::json!({"path": "c.txt"}),
                    signature: None,
                    cache_control: None,
                }]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolResult {
                    tool_use_id: "3".into(),
                    content: serde_json::json!("ok"),
                    is_error: None,
                }]),
            },
        ];

        let trimmed = ContextManager::trim_tool_messages(&mut messages, 2);
        assert!(trimmed);
        // Should have removed round 1 (2 messages), keeping rounds 2 and 3
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn test_trim_tool_messages_no_trim_needed() {
        let mut messages = vec![
            Message {
                role: "assistant".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "test".into(),
                    input: serde_json::json!({}),
                    signature: None,
                    cache_control: None,
                }]),
            },
            Message {
                role: "user".into(),
                content: MessageContent::Array(vec![ContentBlock::ToolResult {
                    tool_use_id: "1".into(),
                    content: serde_json::json!("ok"),
                    is_error: None,
                }]),
            },
        ];

        let trimmed = ContextManager::trim_tool_messages(&mut messages, 5);
        assert!(!trimmed);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens_from_str(""), 0);
    }
}
