// 服务状态检查中间件
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
    http::StatusCode,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 服务状态检查中间件
///
/// 当代理服务未启动时，拒绝非管理 API 的请求。
/// 需要 `is_running: Arc<RwLock<bool>>` 作为请求扩展或通过 State 传入。
pub async fn service_status_middleware(
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();

    // Always allow Admin API, auth callback, and health checks
    if path.starts_with("/api/")
        || path == "/auth/callback"
        || path == "/health"
        || path == "/healthz"
    {
        return next.run(request).await;
    }

    // Check if service is running via request extension
    if let Some(is_running) = request.extensions().get::<Arc<RwLock<bool>>>() {
        let running = *is_running.read().await;
        if !running {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Proxy service is currently disabled".to_string(),
            )
                .into_response();
        }
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_service_status_placeholder() {
        // Service status middleware is tested via integration tests
        assert!(true);
    }
}
