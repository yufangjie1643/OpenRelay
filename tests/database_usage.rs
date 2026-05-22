use openrelay::database::{Database, UsageLog};

fn log_entry(model: &str, key_name: &str, input: u64, cached: u64, output: u64) -> UsageLog {
    UsageLog {
        timestamp: "2026-05-21T12:00:00Z".to_string(),
        request_id: "req-test".to_string(),
        model: model.to_string(),
        key_name: key_name.to_string(),
        input_tokens: input,
        cached_tokens: cached,
        cached_write_tokens: 0,
        output_tokens: output,
        cost: 0.42,
        status: 200,
        duration_ms: 123,
        stream: false,
        user_agent: "test-client".to_string(),
        error: String::new(),
        path: "/v1/chat/completions".to_string(),
    }
}

#[test]
fn sqlite_usage_database_records_stats_and_pages_entries() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path()).unwrap();

    db.record_usage(&log_entry("gpt-a", "master", 100, 25, 50))
        .unwrap();
    db.record_usage(&log_entry("gpt-a", "dev-key", 80, 0, 20))
        .unwrap();

    let page = db.usage_page(1, 1).unwrap();

    assert!(dir.path().join("openrelay.db").exists());
    assert_eq!(page.stats.total_requests, 2);
    assert_eq!(page.stats.total_input_tokens, 180);
    assert_eq!(page.stats.total_cached_tokens, 25);
    assert_eq!(page.stats.total_output_tokens, 70);
    assert_eq!(page.stats.cache_hit_rate, 13.89);
    assert_eq!(page.pagination.page_size, 1);
    assert_eq!(page.pagination.total, 2);
    assert_eq!(page.pagination.total_pages, 2);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.stats.by_model["gpt-a"].requests, 2);
    assert_eq!(page.stats.by_key["master"].requests, 1);
}

#[test]
fn sqlite_usage_database_exports_and_clears_logs() {
    let db = Database::memory().unwrap();
    db.record_usage(&log_entry("gpt-a", "master", 10, 0, 5))
        .unwrap();

    let csv = db.export_usage_csv().unwrap();
    assert!(csv.starts_with("timestamp,model,key_name,input_tokens"));
    assert!(csv.contains("gpt-a,master,10,0,0,5"));

    db.clear_usage().unwrap();
    let page = db.usage_page(1, 20).unwrap();
    assert_eq!(page.stats.total_requests, 0);
    assert!(page.entries.is_empty());
}

#[tokio::test]
async fn sqlite_usage_database_exposes_async_wrappers_for_request_path() {
    let db = Database::memory().unwrap();
    db.record_usage_async(log_entry("gpt-a", "master", 10, 0, 5))
        .await
        .unwrap();

    let count = db
        .request_count_since_async(
            "2026-05-21T00:00:00Z".to_string(),
            Some("master".to_string()),
            None,
        )
        .await
        .unwrap();
    let cost = db
        .total_cost_async(Some("master".to_string()), None)
        .await
        .unwrap();
    let page = db.usage_page_async(1, 20).await.unwrap();

    assert_eq!(count, 1);
    assert_eq!(cost, 0.42);
    assert_eq!(page.stats.total_requests, 1);
}

#[test]
fn sqlite_usage_database_lists_distinct_user_agent_candidates() {
    let db = Database::memory().unwrap();
    let mut first = log_entry("gpt-a", "master", 10, 0, 5);
    first.user_agent = "claude-cli/2.0.0 (external, cli)".to_string();
    db.record_usage(&first).unwrap();

    let mut duplicate = log_entry("gpt-a", "master", 10, 0, 5);
    duplicate.user_agent = "claude-cli/2.0.0 (external, cli)".to_string();
    db.record_usage(&duplicate).unwrap();

    let mut second = log_entry("gpt-a", "master", 10, 0, 5);
    second.user_agent = "cursor-agent/1.0.0".to_string();
    db.record_usage(&second).unwrap();

    let candidates = db.user_agent_candidates(10).unwrap();

    assert_eq!(
        candidates,
        vec![
            "cursor-agent/1.0.0".to_string(),
            "claude-cli/2.0.0 (external, cli)".to_string()
        ]
    );
}
