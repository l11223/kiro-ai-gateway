use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::quota::QuotaData;
use super::token::TokenData;

/// 账号数据结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub token: TokenData,
    /// 可选的设备指纹，用于切换账号时固定机器信息
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<DeviceProfile>,
    /// 设备指纹历史（生成/采集时记录），不含基线
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub device_history: Vec<DeviceProfileVersion>,
    pub quota: Option<QuotaData>,
    /// Disabled accounts are ignored by the proxy token pool
    #[serde(default)]
    pub disabled: bool,
    /// Optional human-readable reason for disabling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Unix timestamp when the account was disabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<i64>,
    /// User manually disabled proxy feature (does not affect app usage)
    #[serde(default)]
    pub proxy_disabled: bool,
    /// Optional human-readable reason for proxy disabling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_disabled_reason: Option<String>,
    /// Unix timestamp when the proxy was disabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_disabled_at: Option<i64>,
    /// 受配额保护禁用的模型列表
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub protected_models: HashSet<String>,
    /// 403 验证阻止状态 (VALIDATION_REQUIRED)
    #[serde(default)]
    pub validation_blocked: bool,
    /// 验证阻止截止时间戳
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_blocked_until: Option<i64>,
    /// 验证阻止原因
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_blocked_reason: Option<String>,
    pub created_at: i64,
    pub last_used: i64,
    /// 绑定的代理 ID (None = 使用全局代理池)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_id: Option<String>,
    /// 代理绑定时间
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_bound_at: Option<i64>,
    /// 用户自定义标签
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_label: Option<String>,
}

impl Account {
    pub fn new(id: String, email: String, token: TokenData) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            email,
            name: None,
            token,
            device_profile: None,
            device_history: Vec::new(),
            quota: None,
            disabled: false,
            disabled_reason: None,
            disabled_at: None,
            proxy_disabled: false,
            proxy_disabled_reason: None,
            proxy_disabled_at: None,
            protected_models: HashSet::new(),
            validation_blocked: false,
            validation_blocked_until: None,
            validation_blocked_reason: None,
            created_at: now,
            last_used: now,
            proxy_id: None,
            proxy_bound_at: None,
            custom_label: None,
        }
    }

    pub fn update_last_used(&mut self) {
        self.last_used = chrono::Utc::now().timestamp();
    }

    pub fn update_quota(&mut self, quota: QuotaData) {
        self.quota = Some(quota);
    }
}

/// 账号索引数据（accounts.json）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountIndex {
    pub version: String,
    pub accounts: Vec<AccountSummary>,
    pub current_account_id: Option<String>,
}

/// 账号摘要信息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountSummary {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub proxy_disabled: bool,
    /// 受保护的模型列表，供 UI 显示锁定图标
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub protected_models: HashSet<String>,
    pub created_at: i64,
    pub last_used: i64,
}

impl AccountIndex {
    pub fn new() -> Self {
        Self {
            version: "2.0".to_string(),
            accounts: Vec::new(),
            current_account_id: None,
        }
    }
}

impl Default for AccountIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// 设备指纹（storage.json 中 telemetry 相关字段）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceProfile {
    pub machine_id: String,
    pub mac_machine_id: String,
    pub dev_device_id: String,
    pub sqm_id: String,
}

/// 指纹历史版本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceProfileVersion {
    pub id: String,
    pub created_at: i64,
    pub label: String,
    pub profile: DeviceProfile,
    #[serde(default)]
    pub is_current: bool,
}

/// 导出账号项（用于备份/迁移）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountExportItem {
    pub email: String,
    pub refresh_token: String,
}

/// 导出账号响应
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountExportResponse {
    pub accounts: Vec<AccountExportItem>,
}

// ============================================================================
// Property-Based Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::quota::{ModelQuota, QuotaData};
    use crate::models::token::TokenData;
    use proptest::collection::{hash_set, vec};
    use proptest::prelude::*;

    // ── Arbitrary strategies ───────────────────────────────────────────

    fn arb_device_profile() -> impl Strategy<Value = DeviceProfile> {
        ("[a-f0-9]{32}", "[a-f0-9]{32}", "[a-f0-9-]{36}", "[a-f0-9-]{36}").prop_map(
            |(machine_id, mac_machine_id, dev_device_id, sqm_id)| DeviceProfile {
                machine_id,
                mac_machine_id,
                dev_device_id,
                sqm_id,
            },
        )
    }

    fn arb_device_profile_version() -> impl Strategy<Value = DeviceProfileVersion> {
        (
            "[a-f0-9-]{36}",
            0i64..=2_000_000_000i64,
            "[a-zA-Z0-9 ]{1,20}",
            arb_device_profile(),
            any::<bool>(),
        )
            .prop_map(|(id, created_at, label, profile, is_current)| {
                DeviceProfileVersion { id, created_at, label, profile, is_current }
            })
    }

    fn arb_model_quota() -> impl Strategy<Value = ModelQuota> {
        (
            "[a-zA-Z0-9_-]{3,30}",
            0i32..=100i32,
            "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z",
        )
            .prop_map(|(name, percentage, reset_time)| ModelQuota { name, percentage, reset_time })
    }

    fn arb_quota_data() -> impl Strategy<Value = QuotaData> {
        (
            vec(arb_model_quota(), 0..5),
            0i64..=2_000_000_000i64,
            any::<bool>(),
            proptest::option::of(prop_oneof!["FREE", "PRO", "ULTRA"].boxed()),
        )
            .prop_map(|(models, last_updated, is_forbidden, subscription_tier)| QuotaData {
                models,
                last_updated,
                is_forbidden,
                subscription_tier,
            })
    }

    fn arb_token_data() -> impl Strategy<Value = TokenData> {
        (
            "[a-zA-Z0-9]{20,40}",
            "[a-zA-Z0-9]{20,40}",
            300i64..=7200i64,
            0i64..=2_000_000_000i64,
            proptest::option::of("[a-zA-Z0-9.@]{5,30}"),
            proptest::option::of("[a-zA-Z0-9-]{5,20}"),
            proptest::option::of("[a-zA-Z0-9-]{5,20}"),
        )
            .prop_map(
                |(access_token, refresh_token, expires_in, expiry_timestamp, email, project_id, session_id)| {
                    TokenData {
                        access_token,
                        refresh_token,
                        expires_in,
                        expiry_timestamp,
                        token_type: "Bearer".to_string(),
                        email,
                        project_id,
                        session_id,
                    }
                },
            )
    }

    /// Build an Account strategy by composing smaller tuple groups
    /// (proptest Strategy is only implemented for tuples up to 12 elements).
    fn arb_account() -> impl Strategy<Value = Account> {
        // Group 1: identity + core data (7 elements)
        let group1 = (
            "[a-f0-9-]{36}",
            "[a-zA-Z0-9.]+@[a-zA-Z0-9]+\\.[a-z]{2,4}",
            proptest::option::of("[a-zA-Z ]{1,30}"),
            arb_token_data(),
            proptest::option::of(arb_device_profile()),
            vec(arb_device_profile_version(), 0..3),
            proptest::option::of(arb_quota_data()),
        );

        // Group 2: disabled flags + protected models (7 elements)
        let group2 = (
            any::<bool>(),
            proptest::option::of("[a-zA-Z0-9 ]{1,50}"),
            proptest::option::of(0i64..=2_000_000_000i64),
            any::<bool>(),
            proptest::option::of("[a-zA-Z0-9 ]{1,50}"),
            proptest::option::of(0i64..=2_000_000_000i64),
            hash_set("[a-zA-Z0-9_-]{3,20}", 0..5),
        );

        // Group 3: validation + timestamps + extras (8 elements)
        let group3 = (
            any::<bool>(),
            proptest::option::of(0i64..=2_000_000_000i64),
            proptest::option::of("[a-zA-Z0-9 ]{1,50}"),
            0i64..=2_000_000_000i64,
            0i64..=2_000_000_000i64,
            proptest::option::of("[a-f0-9-]{36}"),
            proptest::option::of(0i64..=2_000_000_000i64),
            proptest::option::of("[a-zA-Z0-9 ]{1,30}"),
        );

        (group1, group2, group3).prop_map(|(g1, g2, g3)| Account {
            id: g1.0,
            email: g1.1,
            name: g1.2,
            token: g1.3,
            device_profile: g1.4,
            device_history: g1.5,
            quota: g1.6,
            disabled: g2.0,
            disabled_reason: g2.1,
            disabled_at: g2.2,
            proxy_disabled: g2.3,
            proxy_disabled_reason: g2.4,
            proxy_disabled_at: g2.5,
            protected_models: g2.6,
            validation_blocked: g3.0,
            validation_blocked_until: g3.1,
            validation_blocked_reason: g3.2,
            created_at: g3.3,
            last_used: g3.4,
            proxy_id: g3.5,
            proxy_bound_at: g3.6,
            custom_label: g3.7,
        })
    }

    // ── Property 1: Account 序列化往返一致性 ───────────────────────────
    // **Feature: kiro-ai-gateway, Property 1: Account 序列化往返一致性**
    // **Validates: Requirements 12.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn account_serialization_roundtrip(account in arb_account()) {
            let json = serde_json::to_string(&account)
                .expect("Account serialization should not fail");
            let deserialized: Account = serde_json::from_str(&json)
                .expect("Account deserialization should not fail");
            prop_assert_eq!(&account, &deserialized);
        }
    }
}
