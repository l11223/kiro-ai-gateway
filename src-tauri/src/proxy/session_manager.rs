use sha2::{Digest, Sha256};
use serde_json::Value;

/// 会话管理器 - 基于请求内容生成稳定的会话指纹
///
/// 设计理念:
/// - 只哈希第一条有效用户消息内容，不混入模型名称或时间戳
/// - 确保同一对话的所有轮次使用相同的 session_id
/// - 最大化 prompt caching 的命中率
///
/// 有效消息判定：≥3字符，排除系统标记
/// 输出格式：`sid-{sha256_hex[:16]}`
pub struct SessionManager;

/// 判断消息文本是否为有效的用户消息
/// 有效条件：≥3字符，且不包含系统标记
fn is_valid_user_message(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.len() >= 3
        && !trimmed.contains("<system-reminder>")
        && !trimmed.contains("[System")
}

/// 从 SHA256 哈希生成 session ID
fn hash_to_session_id(hasher: Sha256) -> String {
    let hash = format!("{:x}", hasher.finalize());
    format!("sid-{}", &hash[..16])
}

impl SessionManager {
    /// 根据 Claude 协议请求生成稳定的会话指纹
    ///
    /// 优先级:
    /// 1. metadata.user_id (客户端显式提供)
    /// 2. 第一条有效用户消息的 SHA256 哈希
    pub fn extract_session_id(request: &Value) -> String {
        // 1. 优先使用 metadata 中的 user_id
        if let Some(user_id) = request
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .and_then(|v| v.as_str())
        {
            if !user_id.is_empty() && !user_id.contains("session-") {
                tracing::debug!("[SessionManager] Using explicit user_id: {}", user_id);
                return user_id.to_string();
            }
        }

        // 2. 基于第一条有效用户消息的 SHA256 哈希
        let mut hasher = Sha256::new();
        let mut content_found = false;

        if let Some(messages) = request.get("messages").and_then(|v| v.as_array()) {
            for msg in messages {
                if msg.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }

                let text = extract_claude_message_text(msg);
                let clean_text = text.trim();

                if is_valid_user_message(clean_text) {
                    hasher.update(clean_text.as_bytes());
                    content_found = true;
                    break; // 始终锚定第一条有效用户消息
                }
            }
        }

        if !content_found {
            // 退化：对最后一条消息进行哈希
            if let Some(last_msg) = request
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
            {
                hasher.update(last_msg.to_string().as_bytes());
            }
        }

        let sid = hash_to_session_id(hasher);
        tracing::debug!(
            "[SessionManager] Generated session_id: {} (content_found: {})",
            sid,
            content_found
        );
        sid
    }

    /// 根据 OpenAI 协议请求生成稳定的会话指纹
    ///
    /// 基于第一条有效用户消息的 SHA256 哈希
    pub fn extract_openai_session_id(request: &Value) -> String {
        let mut hasher = Sha256::new();
        let mut content_found = false;

        if let Some(messages) = request.get("messages").and_then(|v| v.as_array()) {
            for msg in messages {
                if msg.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }

                let text = extract_openai_message_text(msg);
                let clean_text = text.trim();

                if is_valid_user_message(clean_text) {
                    hasher.update(clean_text.as_bytes());
                    content_found = true;
                    break;
                }
            }
        }

        if !content_found {
            if let Some(last_msg) = request
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
            {
                hasher.update(last_msg.to_string().as_bytes());
            }
        }

        let sid = hash_to_session_id(hasher);
        tracing::debug!("[SessionManager-OpenAI] Generated fingerprint: {}", sid);
        sid
    }

    /// 根据 Gemini 协议请求生成稳定的会话指纹
    ///
    /// 基于第一条有效用户消息的 SHA256 哈希
    pub fn extract_gemini_session_id(request: &Value, _model: &str) -> String {
        let mut hasher = Sha256::new();
        let mut content_found = false;

        if let Some(contents) = request.get("contents").and_then(|v| v.as_array()) {
            for content in contents {
                if content.get("role").and_then(|v| v.as_str()) != Some("user") {
                    continue;
                }

                if let Some(parts) = content.get("parts").and_then(|v| v.as_array()) {
                    let text_parts: Vec<&str> = parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
                        .collect();

                    let combined_text = text_parts.join(" ");
                    let clean_text = combined_text.trim();

                    if is_valid_user_message(clean_text) {
                        hasher.update(clean_text.as_bytes());
                        content_found = true;
                        break;
                    }
                }
            }
        }

        if !content_found {
            // 兜底：对整个请求体进行摘要
            hasher.update(request.to_string().as_bytes());
        }

        let sid = hash_to_session_id(hasher);
        tracing::debug!("[SessionManager-Gemini] Generated fingerprint: {}", sid);
        sid
    }
}

/// 从 Claude 消息中提取文本内容
/// 支持 string 和 array（content blocks）两种格式
fn extract_claude_message_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    block.get("text").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// 从 OpenAI 消息中提取文本内容
/// 支持 string 和 array（content parts）两种格式
fn extract_openai_message_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    block.get("text").and_then(|v| v.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use proptest::prelude::*;

    // ========== Property-Based Tests ==========
    // **Feature: kiro-ai-gateway, Property 10: 会话 ID 确定性**
    // **Validates: Requirements 4.16, 4.17, 4.18**

    /// Strategy to generate valid user message content (≥3 chars, no system markers)
    fn valid_user_message_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 ]{3,100}".prop_filter("must be valid user message", |s| {
            let trimmed = s.trim();
            trimmed.len() >= 3
                && !trimmed.contains("<system-reminder>")
                && !trimmed.contains("[System")
        })
    }

    /// Strategy to generate two distinct valid user messages
    fn two_distinct_messages() -> impl Strategy<Value = (String, String)> {
        (valid_user_message_strategy(), valid_user_message_strategy()).prop_filter(
            "messages must differ after trimming",
            |(a, b)| a.trim() != b.trim(),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        /// Property 10a: Claude - same content produces same session_id
        /// **Validates: Requirements 4.16**
        #[test]
        fn prop_claude_same_content_same_sid(content in valid_user_message_strategy()) {
            let req1 = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let req2 = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let sid1 = SessionManager::extract_session_id(&req1);
            let sid2 = SessionManager::extract_session_id(&req2);
            prop_assert_eq!(&sid1, &sid2, "Same Claude content must produce same session_id");
            prop_assert!(sid1.starts_with("sid-"));
            prop_assert_eq!(sid1.len(), 20);
        }

        /// Property 10b: Claude - different content produces different session_id
        /// **Validates: Requirements 4.16**
        #[test]
        fn prop_claude_different_content_different_sid((c1, c2) in two_distinct_messages()) {
            let req1 = json!({
                "messages": [{ "role": "user", "content": c1 }]
            });
            let req2 = json!({
                "messages": [{ "role": "user", "content": c2 }]
            });
            let sid1 = SessionManager::extract_session_id(&req1);
            let sid2 = SessionManager::extract_session_id(&req2);
            prop_assert_ne!(sid1, sid2, "Different Claude content must produce different session_id");
        }

        /// Property 10c: OpenAI - same content produces same session_id
        /// **Validates: Requirements 4.17**
        #[test]
        fn prop_openai_same_content_same_sid(content in valid_user_message_strategy()) {
            let req1 = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let req2 = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let sid1 = SessionManager::extract_openai_session_id(&req1);
            let sid2 = SessionManager::extract_openai_session_id(&req2);
            prop_assert_eq!(&sid1, &sid2, "Same OpenAI content must produce same session_id");
            prop_assert!(sid1.starts_with("sid-"));
            prop_assert_eq!(sid1.len(), 20);
        }

        /// Property 10d: OpenAI - different content produces different session_id
        /// **Validates: Requirements 4.17**
        #[test]
        fn prop_openai_different_content_different_sid((c1, c2) in two_distinct_messages()) {
            let req1 = json!({
                "messages": [{ "role": "user", "content": c1 }]
            });
            let req2 = json!({
                "messages": [{ "role": "user", "content": c2 }]
            });
            let sid1 = SessionManager::extract_openai_session_id(&req1);
            let sid2 = SessionManager::extract_openai_session_id(&req2);
            prop_assert_ne!(sid1, sid2, "Different OpenAI content must produce different session_id");
        }

        /// Property 10e: Gemini - same content produces same session_id
        /// **Validates: Requirements 4.18**
        #[test]
        fn prop_gemini_same_content_same_sid(content in valid_user_message_strategy()) {
            let req1 = json!({
                "contents": [{ "role": "user", "parts": [{ "text": content }] }]
            });
            let req2 = json!({
                "contents": [{ "role": "user", "parts": [{ "text": content }] }]
            });
            let sid1 = SessionManager::extract_gemini_session_id(&req1, "gemini-pro");
            let sid2 = SessionManager::extract_gemini_session_id(&req2, "gemini-pro");
            prop_assert_eq!(&sid1, &sid2, "Same Gemini content must produce same session_id");
            prop_assert!(sid1.starts_with("sid-"));
            prop_assert_eq!(sid1.len(), 20);
        }

        /// Property 10f: Gemini - different content produces different session_id
        /// **Validates: Requirements 4.18**
        #[test]
        fn prop_gemini_different_content_different_sid((c1, c2) in two_distinct_messages()) {
            let req1 = json!({
                "contents": [{ "role": "user", "parts": [{ "text": c1 }] }]
            });
            let req2 = json!({
                "contents": [{ "role": "user", "parts": [{ "text": c2 }] }]
            });
            let sid1 = SessionManager::extract_gemini_session_id(&req1, "gemini-pro");
            let sid2 = SessionManager::extract_gemini_session_id(&req2, "gemini-pro");
            prop_assert_ne!(sid1, sid2, "Different Gemini content must produce different session_id");
        }

        /// Property 10g: Cross-protocol determinism - same content across all protocols produces same session_id
        /// **Validates: Requirements 4.16, 4.17, 4.18**
        #[test]
        fn prop_cross_protocol_same_content_same_sid(content in valid_user_message_strategy()) {
            let claude_req = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let openai_req = json!({
                "messages": [{ "role": "user", "content": content }]
            });
            let gemini_req = json!({
                "contents": [{ "role": "user", "parts": [{ "text": content }] }]
            });

            let claude_sid = SessionManager::extract_session_id(&claude_req);
            let openai_sid = SessionManager::extract_openai_session_id(&openai_req);
            let gemini_sid = SessionManager::extract_gemini_session_id(&gemini_req, "gemini-pro");

            prop_assert_eq!(&claude_sid, &openai_sid, "Claude and OpenAI must match for same content");
            prop_assert_eq!(&openai_sid, &gemini_sid, "OpenAI and Gemini must match for same content");
        }
    }

    // ========== Claude Protocol Tests ==========

    #[test]
    fn test_claude_metadata_user_id_priority() {
        let request = json!({
            "model": "claude-3-opus",
            "metadata": { "user_id": "custom-user-123" },
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert_eq!(sid, "custom-user-123");
    }

    #[test]
    fn test_claude_empty_user_id_falls_back_to_hash() {
        let request = json!({
            "model": "claude-3-opus",
            "metadata": { "user_id": "" },
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
        assert_eq!(sid.len(), 20); // "sid-" + 16 hex chars
    }

    #[test]
    fn test_claude_session_prefix_user_id_falls_back() {
        let request = json!({
            "model": "claude-3-opus",
            "metadata": { "user_id": "session-abc123" },
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_claude_no_metadata_uses_hash() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
        assert_eq!(sid.len(), 20);
    }

    #[test]
    fn test_claude_content_blocks_array() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Hello world" }
                    ]
                }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_claude_deterministic_same_content() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid1 = SessionManager::extract_session_id(&request);
        let sid2 = SessionManager::extract_session_id(&request);
        assert_eq!(sid1, sid2);
    }

    #[test]
    fn test_claude_different_content_different_sid() {
        let req1 = json!({
            "model": "claude-3-opus",
            "messages": [{ "role": "user", "content": "Hello world" }]
        });
        let req2 = json!({
            "model": "claude-3-opus",
            "messages": [{ "role": "user", "content": "Goodbye world" }]
        });
        let sid1 = SessionManager::extract_session_id(&req1);
        let sid2 = SessionManager::extract_session_id(&req2);
        assert_ne!(sid1, sid2);
    }

    #[test]
    fn test_claude_skips_system_reminder() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "<system-reminder>ignore this</system-reminder>" },
                { "role": "user", "content": "Real message here" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));

        // Should match the hash of "Real message here", not the system reminder
        let request2 = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Real message here" }
            ]
        });
        let sid2 = SessionManager::extract_session_id(&request2);
        assert_eq!(sid, sid2);
    }

    #[test]
    fn test_claude_skips_short_messages() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Hi" },
                { "role": "user", "content": "Tell me about Rust" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);

        // Should match "Tell me about Rust" since "Hi" is < 3 chars
        let request2 = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Tell me about Rust" }
            ]
        });
        let sid2 = SessionManager::extract_session_id(&request2);
        assert_eq!(sid, sid2);
    }

    #[test]
    fn test_claude_skips_assistant_messages() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "assistant", "content": "I am an assistant" },
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);

        let request2 = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid2 = SessionManager::extract_session_id(&request2);
        assert_eq!(sid, sid2);
    }

    // ========== OpenAI Protocol Tests ==========

    #[test]
    fn test_openai_basic_session_id() {
        let request = json!({
            "model": "gpt-4",
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_openai_session_id(&request);
        assert!(sid.starts_with("sid-"));
        assert_eq!(sid.len(), 20);
    }

    #[test]
    fn test_openai_content_array() {
        let request = json!({
            "model": "gpt-4",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "Hello world" }
                    ]
                }
            ]
        });
        let sid = SessionManager::extract_openai_session_id(&request);
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_openai_deterministic() {
        let request = json!({
            "model": "gpt-4",
            "messages": [{ "role": "user", "content": "Hello world" }]
        });
        let sid1 = SessionManager::extract_openai_session_id(&request);
        let sid2 = SessionManager::extract_openai_session_id(&request);
        assert_eq!(sid1, sid2);
    }

    #[test]
    fn test_openai_skips_system_messages() {
        let request = json!({
            "model": "gpt-4",
            "messages": [
                { "role": "system", "content": "You are helpful" },
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid = SessionManager::extract_openai_session_id(&request);

        let request2 = json!({
            "model": "gpt-4",
            "messages": [
                { "role": "user", "content": "Hello world" }
            ]
        });
        let sid2 = SessionManager::extract_openai_session_id(&request2);
        assert_eq!(sid, sid2);
    }

    // ========== Gemini Protocol Tests ==========

    #[test]
    fn test_gemini_basic_session_id() {
        let request = json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": "Hello world" }]
                }
            ]
        });
        let sid = SessionManager::extract_gemini_session_id(&request, "gemini-pro");
        assert!(sid.starts_with("sid-"));
        assert_eq!(sid.len(), 20);
    }

    #[test]
    fn test_gemini_multi_part() {
        let request = json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [
                        { "text": "Hello" },
                        { "text": "world" }
                    ]
                }
            ]
        });
        let sid = SessionManager::extract_gemini_session_id(&request, "gemini-pro");
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_gemini_deterministic() {
        let request = json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": "Hello world" }]
                }
            ]
        });
        let sid1 = SessionManager::extract_gemini_session_id(&request, "gemini-pro");
        let sid2 = SessionManager::extract_gemini_session_id(&request, "gemini-pro");
        assert_eq!(sid1, sid2);
    }

    #[test]
    fn test_gemini_skips_model_messages() {
        let request = json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [{ "text": "I am a model" }]
                },
                {
                    "role": "user",
                    "parts": [{ "text": "Hello world" }]
                }
            ]
        });
        let sid = SessionManager::extract_gemini_session_id(&request, "gemini-pro");

        let request2 = json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": "Hello world" }]
                }
            ]
        });
        let sid2 = SessionManager::extract_gemini_session_id(&request2, "gemini-pro");
        assert_eq!(sid, sid2);
    }

    // ========== Cross-Protocol Consistency Tests ==========

    #[test]
    fn test_same_content_same_hash_across_string_formats() {
        // Claude string format
        let claude_req = json!({
            "model": "claude-3-opus",
            "messages": [{ "role": "user", "content": "Hello world" }]
        });
        // OpenAI string format
        let openai_req = json!({
            "model": "gpt-4",
            "messages": [{ "role": "user", "content": "Hello world" }]
        });
        // Gemini format
        let gemini_req = json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": "Hello world" }]
            }]
        });

        let claude_sid = SessionManager::extract_session_id(&claude_req);
        let openai_sid = SessionManager::extract_openai_session_id(&openai_req);
        let gemini_sid = SessionManager::extract_gemini_session_id(&gemini_req, "gemini-pro");

        // All three should produce the same session ID for the same content
        assert_eq!(claude_sid, openai_sid);
        assert_eq!(openai_sid, gemini_sid);
    }

    // ========== Edge Cases ==========

    #[test]
    fn test_empty_messages_fallback() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": []
        });
        let sid = SessionManager::extract_session_id(&request);
        // Should still produce a valid sid (from empty hasher)
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_no_messages_field() {
        let request = json!({ "model": "claude-3-opus" });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
    }

    #[test]
    fn test_exactly_3_chars_is_valid() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [{ "role": "user", "content": "abc" }]
        });
        let sid = SessionManager::extract_session_id(&request);
        assert!(sid.starts_with("sid-"));
        assert_eq!(sid.len(), 20);

        // Verify it's not the fallback hash
        let empty_req = json!({
            "model": "claude-3-opus",
            "messages": []
        });
        let empty_sid = SessionManager::extract_session_id(&empty_req);
        assert_ne!(sid, empty_sid);
    }

    #[test]
    fn test_2_chars_is_invalid() {
        let request = json!({
            "model": "claude-3-opus",
            "messages": [
                { "role": "user", "content": "ab" },
                { "role": "user", "content": "Valid message here" }
            ]
        });
        let sid = SessionManager::extract_session_id(&request);

        // Should match "Valid message here" since "ab" is < 3 chars
        let request2 = json!({
            "model": "claude-3-opus",
            "messages": [{ "role": "user", "content": "Valid message here" }]
        });
        let sid2 = SessionManager::extract_session_id(&request2);
        assert_eq!(sid, sid2);
    }
}
