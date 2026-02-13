use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// TTL for all cache entries: 2 hours (Requirement 9.4)
const SIGNATURE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// Minimum signature length to be considered valid
const MIN_SIGNATURE_LENGTH: usize = 50;

/// Layer 1 capacity: tool-specific signatures
const TOOL_CACHE_LIMIT: usize = 500;
/// Layer 2 capacity: model family mappings
const FAMILY_CACHE_LIMIT: usize = 200;
/// Layer 3 capacity: session-based signatures (largest)
const SESSION_CACHE_LIMIT: usize = 1000;

/// Cache entry with timestamp for TTL management
#[derive(Clone, Debug)]
struct CacheEntry<T> {
    data: T,
    timestamp: SystemTime,
}

/// Session signature entry tracking message count for rewind detection
#[derive(Clone, Debug)]
struct SessionSignatureEntry {
    signature: String,
    message_count: usize,
}

impl<T> CacheEntry<T> {
    fn new(data: T) -> Self {
        Self {
            data,
            timestamp: SystemTime::now(),
        }
    }

    fn is_expired(&self) -> bool {
        self.timestamp.elapsed().unwrap_or(Duration::ZERO) > SIGNATURE_TTL
    }
}

/// Triple-layer signature cache system (Requirements 9.1, 9.2, 9.3, 9.4).
///
/// - L1 (`tool_signatures`): tool_use_id → thinking signature
/// - L2 (`thinking_families`): signature → model_family
/// - L3 (`session_signatures`): session_id → latest signature + message count
pub struct SignatureCache {
    /// L1: Tool Use ID → Thinking Signature
    tool_signatures: Mutex<HashMap<String, CacheEntry<String>>>,
    /// L2: Signature → Model Family (e.g. "claude-3-5-sonnet", "gemini-2.0-flash")
    thinking_families: Mutex<HashMap<String, CacheEntry<String>>>,
    /// L3: Session ID → Latest Thinking Signature
    session_signatures: Mutex<HashMap<String, CacheEntry<SessionSignatureEntry>>>,
}

impl SignatureCache {
    fn new() -> Self {
        Self {
            tool_signatures: Mutex::new(HashMap::new()),
            thinking_families: Mutex::new(HashMap::new()),
            session_signatures: Mutex::new(HashMap::new()),
        }
    }

    /// Global singleton instance
    pub fn global() -> &'static SignatureCache {
        static INSTANCE: OnceLock<SignatureCache> = OnceLock::new();
        INSTANCE.get_or_init(SignatureCache::new)
    }

    // ===== Layer 1: Tool Signature Cache =====

    /// Store a tool call signature (Requirement 9.1).
    /// Signatures shorter than `MIN_SIGNATURE_LENGTH` are silently ignored.
    pub fn cache_tool_signature(&self, tool_use_id: &str, signature: String) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.tool_signatures.lock() {
            tracing::debug!(
                "[SignatureCache] Caching tool signature for id: {}",
                tool_use_id
            );
            cache.insert(tool_use_id.to_string(), CacheEntry::new(signature));

            if cache.len() > TOOL_CACHE_LIMIT {
                let before = cache.len();
                cache.retain(|_, v| !v.is_expired());
                let after = cache.len();
                if before != after {
                    tracing::debug!(
                        "[SignatureCache] Tool cache cleanup: {} -> {} entries",
                        before,
                        after
                    );
                }
            }
        }
    }

    /// Retrieve a signature for a tool_use_id (Requirement 9.2).
    /// Returns `None` if not found or expired.
    pub fn get_tool_signature(&self, tool_use_id: &str) -> Option<String> {
        if let Ok(cache) = self.tool_signatures.lock() {
            if let Some(entry) = cache.get(tool_use_id) {
                if !entry.is_expired() {
                    tracing::debug!(
                        "[SignatureCache] Hit tool signature for id: {}",
                        tool_use_id
                    );
                    return Some(entry.data.clone());
                }
            }
        }
        None
    }

    // ===== Layer 2: Thinking Family Cache =====

    /// Store model family for a signature (Requirement 9.3).
    pub fn cache_thinking_family(&self, signature: String, family: String) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.thinking_families.lock() {
            tracing::debug!(
                "[SignatureCache] Caching thinking family for sig (len={}): {}",
                signature.len(),
                family
            );
            cache.insert(signature, CacheEntry::new(family));

            if cache.len() > FAMILY_CACHE_LIMIT {
                let before = cache.len();
                cache.retain(|_, v| !v.is_expired());
                let after = cache.len();
                if before != after {
                    tracing::debug!(
                        "[SignatureCache] Family cache cleanup: {} -> {} entries",
                        before,
                        after
                    );
                }
            }
        }
    }

    /// Get model family for a signature.
    pub fn get_signature_family(&self, signature: &str) -> Option<String> {
        if let Ok(cache) = self.thinking_families.lock() {
            if let Some(entry) = cache.get(signature) {
                if !entry.is_expired() {
                    return Some(entry.data.clone());
                } else {
                    tracing::debug!("[SignatureCache] Signature family entry expired");
                }
            }
        }
        None
    }

    /// Check cross-model compatibility (Requirement 9.3).
    ///
    /// Returns `true` if the signature is compatible with `target_family`.
    /// Returns `false` (and logs a warning) when a signature from one model
    /// family would be injected into a request for an incompatible family
    /// (e.g. a Claude signature used with a Gemini model).
    ///
    /// If the signature has no recorded family the check passes (optimistic).
    pub fn is_signature_compatible(&self, signature: &str, target_family: &str) -> bool {
        if let Some(source_family) = self.get_signature_family(signature) {
            let compatible = Self::families_compatible(&source_family, target_family);
            if !compatible {
                tracing::warn!(
                    "[SignatureCache] Cross-model signature incompatibility: \
                     source_family={}, target_family={}. Blocking injection.",
                    source_family,
                    target_family
                );
            }
            compatible
        } else {
            // No family recorded – allow optimistically
            true
        }
    }

    /// Determine whether two model families are compatible.
    /// Models within the same family prefix are compatible.
    fn families_compatible(source: &str, target: &str) -> bool {
        let src = Self::normalize_family(source);
        let tgt = Self::normalize_family(target);
        src == tgt
    }

    /// Normalize a model family string to its canonical prefix.
    /// e.g. "claude-3-5-sonnet-20241022" → "claude",
    ///      "gemini-2.0-flash-thinking" → "gemini"
    fn normalize_family(family: &str) -> &str {
        let lower = family.as_bytes();
        if lower.len() >= 6 && family[..6].eq_ignore_ascii_case("claude") {
            "claude"
        } else if lower.len() >= 6 && family[..6].eq_ignore_ascii_case("gemini") {
            "gemini"
        } else if lower.len() >= 3 && family[..3].eq_ignore_ascii_case("gpt") {
            "openai"
        } else if lower.len() >= 2 && family[..2].eq_ignore_ascii_case("o1") {
            "openai"
        } else {
            family
        }
    }

    // ===== Layer 3: Session Signature Cache =====

    /// Store the latest thinking signature for a session.
    ///
    /// Handles rewind detection: if `message_count` is lower than the cached
    /// value the signature is forcefully replaced (the user deleted messages).
    pub fn cache_session_signature(
        &self,
        session_id: &str,
        signature: String,
        message_count: usize,
    ) {
        if signature.len() < MIN_SIGNATURE_LENGTH {
            return;
        }

        if let Ok(mut cache) = self.session_signatures.lock() {
            let should_store = match cache.get(session_id) {
                None => true,
                Some(existing) => {
                    if existing.is_expired() {
                        true
                    } else if message_count < existing.data.message_count {
                        // Rewind detected – force update
                        tracing::info!(
                            "[SignatureCache] Rewind detected for {}: {} -> {} messages. \
                             Forcing signature update.",
                            session_id,
                            existing.data.message_count,
                            message_count
                        );
                        true
                    } else if message_count == existing.data.message_count {
                        // Same count – only replace with a longer (more complete) signature
                        signature.len() > existing.data.signature.len()
                    } else {
                        // Normal forward progression
                        true
                    }
                }
            };

            if should_store {
                tracing::debug!(
                    "[SignatureCache] Session {} (msg_count={}) -> storing signature (len={})",
                    session_id,
                    message_count,
                    signature.len()
                );
                cache.insert(
                    session_id.to_string(),
                    CacheEntry::new(SessionSignatureEntry {
                        signature,
                        message_count,
                    }),
                );
            }

            if cache.len() > SESSION_CACHE_LIMIT {
                let before = cache.len();
                cache.retain(|_, v| !v.is_expired());
                let after = cache.len();
                if before != after {
                    tracing::info!(
                        "[SignatureCache] Session cache cleanup: {} -> {} entries (limit: {})",
                        before,
                        after,
                        SESSION_CACHE_LIMIT
                    );
                }
            }
        }
    }

    /// Retrieve the latest thinking signature for a session.
    pub fn get_session_signature(&self, session_id: &str) -> Option<String> {
        if let Ok(cache) = self.session_signatures.lock() {
            if let Some(entry) = cache.get(session_id) {
                if !entry.is_expired() {
                    tracing::debug!(
                        "[SignatureCache] Session {} -> HIT (len={})",
                        session_id,
                        entry.data.signature.len()
                    );
                    return Some(entry.data.signature.clone());
                } else {
                    tracing::debug!("[SignatureCache] Session {} -> EXPIRED", session_id);
                }
            }
        }
        None
    }

    /// Delete a specific session's cached signature.
    #[allow(dead_code)]
    pub fn delete_session_signature(&self, session_id: &str) {
        if let Ok(mut cache) = self.session_signatures.lock() {
            if cache.remove(session_id).is_some() {
                tracing::debug!(
                    "[SignatureCache] Deleted session signature for: {}",
                    session_id
                );
            }
        }
    }

    /// Clear all three cache layers.
    #[allow(dead_code)]
    pub fn clear(&self) {
        if let Ok(mut cache) = self.tool_signatures.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.thinking_families.lock() {
            cache.clear();
        }
        if let Ok(mut cache) = self.session_signatures.lock() {
            cache.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== L1: Tool Signature Tests =====

    #[test]
    fn test_tool_signature_cache_hit() {
        let cache = SignatureCache::new();
        let sig = "x".repeat(60);

        cache.cache_tool_signature("tool_1", sig.clone());
        assert_eq!(cache.get_tool_signature("tool_1"), Some(sig));
    }

    #[test]
    fn test_tool_signature_cache_miss() {
        let cache = SignatureCache::new();
        assert_eq!(cache.get_tool_signature("nonexistent"), None);
    }

    #[test]
    fn test_tool_signature_min_length_rejected() {
        let cache = SignatureCache::new();
        cache.cache_tool_signature("tool_short", "short".to_string());
        assert_eq!(cache.get_tool_signature("tool_short"), None);
    }

    #[test]
    fn test_tool_signature_exact_min_length_rejected() {
        let cache = SignatureCache::new();
        // 49 chars – just below threshold
        let sig = "a".repeat(49);
        cache.cache_tool_signature("tool_49", sig);
        assert_eq!(cache.get_tool_signature("tool_49"), None);
    }

    #[test]
    fn test_tool_signature_at_min_length_accepted() {
        let cache = SignatureCache::new();
        let sig = "a".repeat(50);
        cache.cache_tool_signature("tool_50", sig.clone());
        assert_eq!(cache.get_tool_signature("tool_50"), Some(sig));
    }

    #[test]
    fn test_tool_signature_overwrite() {
        let cache = SignatureCache::new();
        let sig1 = "a".repeat(60);
        let sig2 = "b".repeat(60);

        cache.cache_tool_signature("tool_1", sig1);
        cache.cache_tool_signature("tool_1", sig2.clone());
        assert_eq!(cache.get_tool_signature("tool_1"), Some(sig2));
    }

    // ===== L2: Thinking Family Tests =====

    #[test]
    fn test_thinking_family_cache() {
        let cache = SignatureCache::new();
        let sig = "y".repeat(60);

        cache.cache_thinking_family(sig.clone(), "claude".to_string());
        assert_eq!(
            cache.get_signature_family(&sig),
            Some("claude".to_string())
        );
    }

    #[test]
    fn test_thinking_family_min_length() {
        let cache = SignatureCache::new();
        let sig = "y".repeat(30);
        cache.cache_thinking_family(sig.clone(), "claude".to_string());
        assert_eq!(cache.get_signature_family(&sig), None);
    }

    #[test]
    fn test_thinking_family_miss() {
        let cache = SignatureCache::new();
        assert_eq!(cache.get_signature_family("unknown_sig"), None);
    }

    // ===== Cross-model Compatibility (Requirement 9.3) =====

    #[test]
    fn test_compatible_same_family_claude() {
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);
        cache.cache_thinking_family(sig.clone(), "claude-3-5-sonnet".to_string());
        assert!(cache.is_signature_compatible(&sig, "claude-3-opus"));
    }

    #[test]
    fn test_compatible_same_family_gemini() {
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);
        cache.cache_thinking_family(sig.clone(), "gemini-2.0-flash".to_string());
        assert!(cache.is_signature_compatible(&sig, "gemini-1.5-pro"));
    }

    #[test]
    fn test_incompatible_cross_family() {
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);
        cache.cache_thinking_family(sig.clone(), "claude-3-5-sonnet".to_string());
        assert!(!cache.is_signature_compatible(&sig, "gemini-2.0-flash"));
    }

    #[test]
    fn test_incompatible_claude_to_openai() {
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);
        cache.cache_thinking_family(sig.clone(), "claude-3-opus".to_string());
        assert!(!cache.is_signature_compatible(&sig, "gpt-4o"));
    }

    #[test]
    fn test_compatible_no_family_recorded() {
        // When no family is recorded, the check should pass optimistically
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);
        assert!(cache.is_signature_compatible(&sig, "gemini-2.0-flash"));
    }

    #[test]
    fn test_normalize_family_variants() {
        assert_eq!(SignatureCache::normalize_family("claude-3-5-sonnet-20241022"), "claude");
        assert_eq!(SignatureCache::normalize_family("Claude-3-Opus"), "claude");
        assert_eq!(SignatureCache::normalize_family("gemini-2.0-flash-thinking"), "gemini");
        assert_eq!(SignatureCache::normalize_family("Gemini-1.5-Pro"), "gemini");
        assert_eq!(SignatureCache::normalize_family("gpt-4o"), "openai");
        assert_eq!(SignatureCache::normalize_family("GPT-4-turbo"), "openai");
        assert_eq!(SignatureCache::normalize_family("o1-preview"), "openai");
        assert_eq!(SignatureCache::normalize_family("unknown-model"), "unknown-model");
    }

    // ===== L3: Session Signature Tests =====

    #[test]
    fn test_session_signature_basic() {
        let cache = SignatureCache::new();
        let sig = "a".repeat(60);

        assert!(cache.get_session_signature("sid-test").is_none());
        cache.cache_session_signature("sid-test", sig.clone(), 5);
        assert_eq!(cache.get_session_signature("sid-test"), Some(sig));
    }

    #[test]
    fn test_session_signature_longer_replaces_same_count() {
        let cache = SignatureCache::new();
        let sig1 = "a".repeat(60);
        let sig2 = "b".repeat(80);

        cache.cache_session_signature("sid-1", sig1, 5);
        cache.cache_session_signature("sid-1", sig2.clone(), 5);
        assert_eq!(cache.get_session_signature("sid-1"), Some(sig2));
    }

    #[test]
    fn test_session_signature_shorter_does_not_replace_same_count() {
        let cache = SignatureCache::new();
        let sig_long = "b".repeat(80);
        let sig_short = "a".repeat(60);

        cache.cache_session_signature("sid-1", sig_long.clone(), 5);
        cache.cache_session_signature("sid-1", sig_short, 5);
        assert_eq!(cache.get_session_signature("sid-1"), Some(sig_long));
    }

    #[test]
    fn test_session_signature_rewind_forces_update() {
        let cache = SignatureCache::new();
        let sig1 = "b".repeat(80);
        let sig2 = "a".repeat(60);

        cache.cache_session_signature("sid-1", sig1, 10);
        // Rewind: message_count goes from 10 to 3
        cache.cache_session_signature("sid-1", sig2.clone(), 3);
        assert_eq!(cache.get_session_signature("sid-1"), Some(sig2));
    }

    #[test]
    fn test_session_signature_too_short_ignored() {
        let cache = SignatureCache::new();
        let valid = "a".repeat(60);
        let too_short = "c".repeat(40);

        cache.cache_session_signature("sid-1", valid.clone(), 5);
        cache.cache_session_signature("sid-1", too_short, 1);
        // Still the valid one
        assert_eq!(cache.get_session_signature("sid-1"), Some(valid));
    }

    #[test]
    fn test_session_signature_isolation() {
        let cache = SignatureCache::new();
        let sig = "a".repeat(60);

        cache.cache_session_signature("sid-a", sig.clone(), 1);
        assert_eq!(cache.get_session_signature("sid-a"), Some(sig));
        assert!(cache.get_session_signature("sid-b").is_none());
    }

    #[test]
    fn test_session_forward_progression_replaces() {
        let cache = SignatureCache::new();
        let sig1 = "a".repeat(60);
        let sig2 = "b".repeat(55); // shorter but higher msg count

        cache.cache_session_signature("sid-1", sig1, 5);
        cache.cache_session_signature("sid-1", sig2.clone(), 10);
        assert_eq!(cache.get_session_signature("sid-1"), Some(sig2));
    }

    #[test]
    fn test_delete_session_signature() {
        let cache = SignatureCache::new();
        let sig = "a".repeat(60);

        cache.cache_session_signature("sid-del", sig, 1);
        assert!(cache.get_session_signature("sid-del").is_some());

        cache.delete_session_signature("sid-del");
        assert!(cache.get_session_signature("sid-del").is_none());
    }

    #[test]
    fn test_delete_nonexistent_session_no_panic() {
        let cache = SignatureCache::new();
        cache.delete_session_signature("does-not-exist");
    }

    // ===== Clear =====

    #[test]
    fn test_clear_all_caches() {
        let cache = SignatureCache::new();
        let sig = "x".repeat(60);

        cache.cache_tool_signature("tool_1", sig.clone());
        cache.cache_thinking_family(sig.clone(), "model".to_string());
        cache.cache_session_signature("sid-1", sig.clone(), 1);

        assert!(cache.get_tool_signature("tool_1").is_some());
        assert!(cache.get_signature_family(&sig).is_some());
        assert!(cache.get_session_signature("sid-1").is_some());

        cache.clear();

        assert!(cache.get_tool_signature("tool_1").is_none());
        assert!(cache.get_signature_family(&sig).is_none());
        assert!(cache.get_session_signature("sid-1").is_none());
    }

    // ===== TTL Tests =====

    #[test]
    fn test_cache_entry_not_expired_immediately() {
        let entry = CacheEntry::new("data".to_string());
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_cache_entry_expired_after_ttl() {
        let entry = CacheEntry {
            data: "data".to_string(),
            timestamp: SystemTime::now() - Duration::from_secs(2 * 60 * 60 + 1),
        };
        assert!(entry.is_expired());
    }

    #[test]
    fn test_cache_entry_not_expired_just_before_ttl() {
        let entry = CacheEntry {
            data: "data".to_string(),
            timestamp: SystemTime::now() - Duration::from_secs(2 * 60 * 60 - 10),
        };
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_tool_signature_expired_returns_none() {
        let cache = SignatureCache::new();
        let sig = "x".repeat(60);

        // Manually insert an expired entry
        if let Ok(mut c) = cache.tool_signatures.lock() {
            c.insert(
                "expired_tool".to_string(),
                CacheEntry {
                    data: sig,
                    timestamp: SystemTime::now() - Duration::from_secs(3 * 60 * 60),
                },
            );
        }
        assert_eq!(cache.get_tool_signature("expired_tool"), None);
    }

    #[test]
    fn test_thinking_family_expired_returns_none() {
        let cache = SignatureCache::new();
        let sig = "y".repeat(60);

        if let Ok(mut c) = cache.thinking_families.lock() {
            c.insert(
                sig.clone(),
                CacheEntry {
                    data: "claude".to_string(),
                    timestamp: SystemTime::now() - Duration::from_secs(3 * 60 * 60),
                },
            );
        }
        assert_eq!(cache.get_signature_family(&sig), None);
    }

    #[test]
    fn test_session_signature_expired_returns_none() {
        let cache = SignatureCache::new();

        if let Ok(mut c) = cache.session_signatures.lock() {
            c.insert(
                "sid-expired".to_string(),
                CacheEntry {
                    data: SessionSignatureEntry {
                        signature: "a".repeat(60),
                        message_count: 1,
                    },
                    timestamp: SystemTime::now() - Duration::from_secs(3 * 60 * 60),
                },
            );
        }
        assert_eq!(cache.get_session_signature("sid-expired"), None);
    }

    #[test]
    fn test_expired_session_entry_gets_replaced() {
        let cache = SignatureCache::new();
        let new_sig = "b".repeat(60);

        // Insert an expired entry
        if let Ok(mut c) = cache.session_signatures.lock() {
            c.insert(
                "sid-exp".to_string(),
                CacheEntry {
                    data: SessionSignatureEntry {
                        signature: "a".repeat(60),
                        message_count: 100,
                    },
                    timestamp: SystemTime::now() - Duration::from_secs(3 * 60 * 60),
                },
            );
        }

        // New entry should replace the expired one regardless of message count
        cache.cache_session_signature("sid-exp", new_sig.clone(), 1);
        assert_eq!(cache.get_session_signature("sid-exp"), Some(new_sig));
    }

    // ===== Cross-model compatibility with expired family =====

    #[test]
    fn test_compatibility_with_expired_family_passes() {
        let cache = SignatureCache::new();
        let sig = "z".repeat(60);

        // Insert an expired family entry
        if let Ok(mut c) = cache.thinking_families.lock() {
            c.insert(
                sig.clone(),
                CacheEntry {
                    data: "claude".to_string(),
                    timestamp: SystemTime::now() - Duration::from_secs(3 * 60 * 60),
                },
            );
        }

        // Expired family → no family found → optimistic pass
        assert!(cache.is_signature_compatible(&sig, "gemini-2.0-flash"));
    }
}
