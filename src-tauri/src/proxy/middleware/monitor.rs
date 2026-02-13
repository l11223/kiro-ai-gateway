// 请求监控中间件
use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use std::time::Instant;

// Re-export ProxyRequestLog from core monitor module
pub use crate::proxy::monitor::ProxyRequestLog;

/// 请求监控中间件
///
/// 记录请求的基本信息（方法、URL、状态码、耗时）。
/// 完整的日志持久化将在 Task 14.4 (ProxyMonitor) 中实现。
pub async fn monitor_middleware(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();

    // 跳过内部和管理 API 的监控
    if uri.contains("event_logging") || uri.contains("/api/") || uri.starts_with("/internal/") {
        return next.run(request).await;
    }

    let start = Instant::now();

    // 提取客户端 IP
    let client_ip = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .or_else(|| {
            request
                .headers()
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    // 从 URL 提取模型名（Gemini 原生路径）
    let model = if uri.contains("/v1beta/models/") {
        uri.split("/v1beta/models/")
            .nth(1)
            .and_then(|s| s.split(':').next())
            .map(|s| s.to_string())
    } else {
        None
    };

    // 确定协议类型
    let protocol = if uri.contains("/v1/messages") {
        Some("anthropic".to_string())
    } else if uri.contains("/v1beta/models") {
        Some("gemini".to_string())
    } else if uri.starts_with("/v1/") {
        Some("openai".to_string())
    } else {
        None
    };

    let response = next.run(request).await;

    let duration = start.elapsed().as_millis() as u64;
    let status = response.status().as_u16();

    tracing::info!(
        "[Monitor] {} {} → {} ({}ms) client_ip={} model={} protocol={}",
        method,
        uri,
        status,
        duration,
        client_ip.as_deref().unwrap_or("-"),
        model.as_deref().unwrap_or("-"),
        protocol.as_deref().unwrap_or("-"),
    );

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_request_log_creation() {
        let log = ProxyRequestLog {
            id: "test-id".to_string(),
            timestamp: 1234567890,
            method: "POST".to_string(),
            url: "/v1/chat/completions".to_string(),
            status: 200,
            duration: 150,
            model: Some("gpt-4".to_string()),
            mapped_model: Some("gemini-2.5-flash".to_string()),
            account_email: Some("test@example.com".to_string()),
            client_ip: Some("127.0.0.1".to_string()),
            error: None,
            request_body: None,
            response_body: None,
            input_tokens: Some(100),
            output_tokens: Some(200),
            protocol: Some("openai".to_string()),
            username: None,
        };
        assert_eq!(log.status, 200);
        assert_eq!(log.duration, 150);
    }
}
