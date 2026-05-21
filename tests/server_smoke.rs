use openrelay::config::ensure_files;
use openrelay::server::{build_router, ServerState};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[tokio::test]
async fn rust_server_serves_login_over_http() {
    let dir = tempfile::tempdir().unwrap();
    ensure_files(dir.path()).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            build_router(ServerState::new(dir.path().to_path_buf())),
        )
        .with_graceful_shutdown(async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });

    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{addr}/api/login"))
        .json(&json!({"username": "admin", "password": "admin123"}))
        .send()
        .await
        .unwrap();

    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "status={status}, body={text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["success"], true);
    assert!(body["token"].as_str().unwrap().contains('.'));

    let _ = tx.send(());
    server.await.unwrap();
}
