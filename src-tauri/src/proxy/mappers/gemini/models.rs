// Gemini v1internal data models
//
// Requirements covered:
// - 2.3: Gemini native format support

use serde::{Deserialize, Serialize};

/// V1Internal request wrapper used by Google's internal API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V1InternalRequest {
    pub project: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub request: serde_json::Value,
    pub model: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
    #[serde(rename = "requestType")]
    pub request_type: String,
}

/// Gemini model info for model list endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiModelInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(rename = "displayName")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputTokenLimit")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_token_limit: Option<u32>,
    #[serde(rename = "outputTokenLimit")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_token_limit: Option<u32>,
    #[serde(rename = "supportedGenerationMethods")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_generation_methods: Option<Vec<String>>,
}

/// Gemini model list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiModelList {
    pub models: Vec<GeminiModelInfo>,
    #[serde(rename = "nextPageToken")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v1internal_request_serialization() {
        let req = V1InternalRequest {
            project: "test-project".to_string(),
            request_id: "agent-123".to_string(),
            request: serde_json::json!({"contents": []}),
            model: "gemini-2.5-flash".to_string(),
            user_agent: "kiro-ai-gateway".to_string(),
            request_type: "agent".to_string(),
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["project"], "test-project");
        assert_eq!(json["requestId"], "agent-123");
        assert_eq!(json["model"], "gemini-2.5-flash");
        assert_eq!(json["userAgent"], "kiro-ai-gateway");
        assert_eq!(json["requestType"], "agent");
    }

    #[test]
    fn test_gemini_model_info_serialization() {
        let model = GeminiModelInfo {
            name: "models/gemini-2.5-flash".to_string(),
            version: Some("2.5".to_string()),
            display_name: Some("Gemini 2.5 Flash".to_string()),
            description: Some("Fast model".to_string()),
            input_token_limit: Some(1048576),
            output_token_limit: Some(8192),
            supported_generation_methods: Some(vec![
                "generateContent".to_string(),
                "countTokens".to_string(),
            ]),
        };

        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(json["name"], "models/gemini-2.5-flash");
        assert_eq!(json["inputTokenLimit"], 1048576);
    }

    #[test]
    fn test_gemini_model_list_deserialization() {
        let json = serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-2.5-flash",
                    "displayName": "Gemini 2.5 Flash"
                }
            ]
        });

        let list: GeminiModelList = serde_json::from_value(json).unwrap();
        assert_eq!(list.models.len(), 1);
        assert_eq!(list.models[0].name, "models/gemini-2.5-flash");
        assert!(list.next_page_token.is_none());
    }
}
