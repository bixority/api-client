use super::support::{Reply, ok, serve};
use crate::{APIClient, APIClientError, Headers, RetryPolicy};
use futures::StreamExt;
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn query_pairs_are_appended_in_order_with_duplicates() -> TestResult {
    let server = serve(|_, _| ok("ok")).await;
    let client = APIClient::new(&server.url).build()?;
    let response = client
        .get("/p")
        .query([("a", "1"), ("b", "2")])
        .query(vec![("a".to_string(), "3 4".to_string())])
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 200);
    let line = server.request(0).lines().next().unwrap_or("").to_owned();
    assert_eq!(line, "GET /p?a=1&b=2&a=3+4 HTTP/1.1");
    Ok(())
}

#[tokio::test]
async fn invalid_header_surfaces_at_send() -> TestResult {
    let server = serve(|_, _| ok("ok")).await;
    let client = APIClient::new(&server.url).build()?;

    let err = client.get("/").header("bad name", "v").send().await;
    assert!(matches!(err, Err(APIClientError::InvalidHeader(_))));

    let err = client.get("/").header("x-ok", "line\nbreak").send().await;
    assert!(matches!(err, Err(APIClientError::InvalidHeader(_))));

    assert_eq!(server.hits(), 0);
    Ok(())
}

#[tokio::test]
async fn headers_are_merged_and_header_replaces() -> TestResult {
    let server = serve(|_, _| ok("ok")).await;
    let client = APIClient::new(&server.url).build()?;
    client
        .get("/")
        .header("x-one", "old")
        .headers(&Headers::new().insert("x-two", "2"))
        .header("x-one", "new")
        .send()
        .await?;
    let req = server.request(0).to_ascii_lowercase();
    assert!(req.contains("x-one: new\r\n"));
    assert!(!req.contains("x-one: old"));
    assert!(req.contains("x-two: 2\r\n"));
    Ok(())
}

#[tokio::test]
async fn json_sets_body_and_content_type() -> TestResult {
    #[derive(serde::Serialize)]
    struct Data {
        a: u32,
    }
    let server = serve(|_, _| ok("ok")).await;
    let client = APIClient::new(&server.url).build()?;
    client.post("/j").json(&Data { a: 1 }).send().await?;
    let req = server.request(0);
    assert!(req.starts_with("POST /j HTTP/1.1"));
    assert!(
        req.to_ascii_lowercase()
            .contains("content-type: application/json\r\n")
    );
    assert!(req.ends_with("{\"a\":1}"));
    Ok(())
}

#[tokio::test]
async fn json_serialization_error_surfaces_at_send() -> TestResult {
    let client = APIClient::new("http://127.0.0.1:1").build()?;
    let mut map = std::collections::HashMap::new();
    map.insert((1, 2), 3);
    let err = client.post("/").json(&map).send().await;
    assert!(matches!(err, Err(APIClientError::Json(_))));
    Ok(())
}

#[tokio::test]
async fn body_is_sent() -> TestResult {
    let server = serve(|_, _| ok("ok")).await;
    let client = APIClient::new(&server.url).build()?;
    client.put("/b").body("raw body").send().await?;
    assert!(server.request(0).ends_with("raw body"));
    Ok(())
}

#[tokio::test]
async fn per_request_timeout_fires() -> TestResult {
    let server = serve(|_, _| Reply::Hang).await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy::none())
        .build()?;
    let started = Instant::now();
    let err = client
        .get("/")
        .timeout(Duration::from_millis(200))
        .send()
        .await;
    assert!(matches!(&err, Err(APIClientError::Request(e)) if e.is_timeout()));
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[tokio::test]
async fn no_timeout_waits_past_the_client_default() -> TestResult {
    let server = serve(|_, _| {
        Reply::After(
            Duration::from_millis(1500),
            super::support::response("200 OK", &[], "late"),
        )
    })
    .await;
    let client = APIClient::new(&server.url)
        .timeout_secs(1)
        .retry(RetryPolicy::none())
        .build()?;

    let err = client.get("/").send().await;
    assert!(matches!(&err, Err(APIClientError::Request(e)) if e.is_timeout()));

    let response = client.get("/").no_timeout().send().await?;
    assert_eq!(response.text().await?, "late");
    Ok(())
}

#[tokio::test]
async fn queue_deadline_gives_queue_timeout() -> TestResult {
    let server = serve(|_, _| {
        Reply::After(
            Duration::from_millis(800),
            super::support::response("200 OK", &[], "slow"),
        )
    })
    .await;
    let client = APIClient::new(&server.url)
        .max_concurrent(Some(1))
        .queue_deadline(Duration::from_millis(100))
        .build()?;

    let first = {
        let client = client.clone();
        tokio::spawn(async move { client.get("/").send().await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    let err = client.get("/").send().await;
    assert!(matches!(err, Err(APIClientError::QueueTimeout)));
    assert!(started.elapsed() < Duration::from_millis(600));
    assert_eq!(server.hits(), 1);

    assert_eq!(first.await??.status().as_u16(), 200);
    Ok(())
}

/// Server answering every request at once with a 5-byte body.
async fn instant_server() -> super::support::Server {
    serve(|_, _| ok("hello")).await
}

#[tokio::test]
async fn permit_is_released_at_the_head_by_default() -> TestResult {
    let server = instant_server().await;
    let client = APIClient::new(&server.url)
        .max_concurrent(Some(1))
        .build()?;

    let first = client.get("/a").send().await?;
    // The first body is unread, yet the second request goes through.
    let second = client.get("/b").send().await?;
    assert_eq!(server.hits(), 2);
    assert_eq!(second.text().await?, "hello");
    assert_eq!(first.text().await?, "hello");
    Ok(())
}

#[tokio::test]
async fn held_permit_blocks_until_the_body_is_read() -> TestResult {
    let server = instant_server().await;
    let client = APIClient::new(&server.url)
        .max_concurrent(Some(1))
        .build()?;

    let first = client.get("/a").hold_permit_until_body_end().send().await?;
    let second = {
        let client = client.clone();
        tokio::spawn(async move { client.get("/b").send().await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.hits(),
        1,
        "second request started before the body ended"
    );

    assert_eq!(first.text().await?, "hello");
    assert_eq!(second.await??.text().await?, "hello");
    assert_eq!(server.hits(), 2);
    Ok(())
}

#[tokio::test]
async fn client_default_holds_the_permit_and_a_stream_releases_it_at_its_end() -> TestResult {
    let server = instant_server().await;
    let client = APIClient::new(&server.url)
        .max_concurrent(Some(1))
        .hold_permit_until_body_end(true)
        .build()?;

    let first = client.get("/a").send().await?;
    let second = {
        let client = client.clone();
        tokio::spawn(async move { client.get("/b").send().await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.hits(), 1);

    let mut stream = Box::pin(first.bytes_stream()?);
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk?);
    }
    assert_eq!(body, b"hello");
    second.await??;
    assert_eq!(server.hits(), 2);
    Ok(())
}

#[tokio::test]
async fn dropping_a_held_response_releases_the_permit() -> TestResult {
    let server = instant_server().await;
    let client = APIClient::new(&server.url)
        .max_concurrent(Some(1))
        .queue_deadline(Duration::from_secs(5))
        .build()?;

    let first = client.get("/a").hold_permit_until_body_end().send().await?;
    drop(first);
    client.get("/b").send().await?;
    assert_eq!(server.hits(), 2);
    Ok(())
}
