// integration test for poll_feed using wiremock
use debloatmail_feeds::poll::poll_feed;
use debloatmail_store::{open_in_memory, upsert_feed_item, UpsertFeedItem, StoreError};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn test_poll_feed_inserts_items() {
    // start mock HTTP server serving an RSS feed
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mock_server = MockServer::start().await;
        let rss = include_bytes!("../tests/fixtures/rss_sample.xml");
        let response = ResponseTemplate::new(200)
            .insert_header("Content-Type", "application/rss+xml")
            .set_body_bytes(rss.clone());
        Mock::given(method("GET"))
            .and(path("/feed"))
            .respond_with(response)
            .mount(&mock_server)
            .await;

        // prepare in‑memory SQLite store
        let conn = open_in_memory().expect("open in‑memory DB");
        // insert a feed account and a feed row
        conn.execute(
            "INSERT INTO accounts (public_id, kind, name, created_at, updated_at) VALUES ('acct-feed', 'feed', 'Feeds', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feeds (public_id, account_id, url, title) VALUES ('feed-1', 1, ?, 'Test Feed')",
            [&format!("{}/feed", mock_server.uri())],
        )
        .unwrap();

        // run poll_feed (feed_id = 1)
        poll_feed(&conn, 1).expect("poll_feed should succeed");

        // verify feed_items were inserted (2 items from fixture)
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM feed_items WHERE feed_id = 1").unwrap();
        let count: i64 = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
    });
}
