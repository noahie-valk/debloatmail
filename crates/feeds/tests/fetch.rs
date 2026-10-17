// tests for fetch module using wiremock
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use debloatmail_feeds::fetch::{fetch_feed, FetchError};

#[tokio::test]
async fn test_fetch_success() {
    // start mock server
    let mock_server = MockServer::start().await;
    let body = b"<rss version=\"2.0\"><channel><title>Test</title></channel></rss>";
    let response = ResponseTemplate::new(200)
        .insert_header("Content-Type", "application/rss+xml")
        .set_body_bytes(body.clone());
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(response)
        .mount(&mock_server)
        .await;

    let url = format!("{}/feed", &mock_server.uri());
    let result = fetch_feed(&url, None, None).expect("fetch should succeed");
    assert_eq!(result.status, 200);
    assert_eq!(result.body, body);
    assert!(result.etag.is_none());
    assert!(result.last_modified.is_none());
}

#[tokio::test]
async fn test_fetch_not_modified() {
    let mock_server = MockServer::start().await;
    let response = ResponseTemplate::new(304);
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(response)
        .mount(&mock_server)
        .await;
    let url = format!("{}/feed", &mock_server.uri());
    let result = fetch_feed(&url, Some("etag123"), Some("Wed, 21 Oct 2015 07:28:00 GMT"))
        .expect("fetch should succeed");
    assert_eq!(result.status, 304);
}

#[tokio::test]
async fn test_fetch_html_content_error() {
    let mock_server = MockServer::start().await;
    let response = ResponseTemplate::new(200)
        .insert_header("Content-Type", "text/html")
        .set_body_string("<html></html>");
    Mock::given(method("GET"))
        .and(path("/feed"))
        .respond_with(response)
        .mount(&mock_server)
        .await;
    let url = format!("{}/feed", &mock_server.uri());
    let err = fetch_feed(&url, None, None).unwrap_err();
    match err {
        FetchError::HtmlContent => {}
        _ => panic!("expected HtmlContent error"),
    }
}
