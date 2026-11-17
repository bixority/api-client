use super::support::{Reply, ok, response, serve};
use crate::retry::{jittered, parse_retry_after};
use crate::{APIClient, APIClientError, RetryPolicy};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unavailable() -> Reply {
    Reply::Raw(response("503 Service Unavailable", &[], ""))
}

#[test]
fn default_policy_matches_the_old_behaviour() {
    let p = RetryPolicy::default();
    assert_eq!(p.max_attempts, 3);
    assert_eq!(p.delay(1, None), Duration::from_millis(100));
    assert_eq!(p.delay(2, None), Duration::from_millis(200));
    assert_eq!(p.retry_statuses, vec![502, 503, 504]);
    assert_eq!(RetryPolicy::none().max_attempts, 1);
}

#[test]
fn delay_doubles_and_is_capped() {
    let p = RetryPolicy {
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(500),
        ..RetryPolicy::default()
    };
    assert_eq!(p.delay(3, None), Duration::from_millis(400));
    assert_eq!(p.delay(4, None), Duration::from_millis(500));
    assert_eq!(p.delay(200, None), Duration::from_millis(500));
}

#[test]
fn jitter_stays_within_half_to_full_delay() {
    let delay = Duration::from_millis(200);
    assert_eq!(jittered(delay, 0), Duration::from_millis(100));
    assert_eq!(
        jittered(delay, u64::MAX),
        delay.min(jittered(delay, u64::MAX))
    );
    for r in [1, 12345, 99_999_999, u64::MAX, u64::MAX / 3] {
        let j = jittered(delay, r);
        assert!(j >= delay / 2 && j <= delay, "{j:?}");
    }
    let p = RetryPolicy {
        jitter: true,
        ..RetryPolicy::default()
    };
    for _ in 0..50 {
        let d = p.delay(2, None);
        assert!(d >= Duration::from_millis(100) && d <= Duration::from_millis(200));
    }
}

#[test]
fn retry_after_is_honoured_and_capped() {
    let p = RetryPolicy {
        respect_retry_after: true,
        ..RetryPolicy::default()
    };
    assert_eq!(
        p.delay(1, Some(Duration::from_secs(1))),
        Duration::from_secs(1)
    );
    assert_eq!(p.delay(1, Some(Duration::from_secs(60))), p.max_delay);
    assert_eq!(p.delay(1, None), Duration::from_millis(100));

    let off = RetryPolicy::default();
    assert_eq!(
        off.delay(1, Some(Duration::from_secs(1))),
        Duration::from_millis(100)
    );
}

#[test]
fn retry_after_parses_delta_seconds_only() {
    assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
    assert_eq!(parse_retry_after(" 0 "), Some(Duration::ZERO));
    assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(parse_retry_after("-1"), None);
}

#[tokio::test]
async fn get_is_retried_to_three_attempts_on_503() -> TestResult {
    let server = serve(|_, _| unavailable()).await;
    let client = APIClient::new(&server.url).build()?;
    let response = client.get("/").send().await?;
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(server.hits(), 3);
    Ok(())
}

#[tokio::test]
async fn get_recovers_when_a_retry_succeeds() -> TestResult {
    let server = serve(|n, _| if n < 1 { unavailable() } else { ok("fine") }).await;
    let client = APIClient::new(&server.url).build()?;
    let response = client.get("/").send().await?;
    assert_eq!(response.text().await?, "fine");
    assert_eq!(server.hits(), 2);
    Ok(())
}

#[tokio::test]
async fn post_is_not_retried() -> TestResult {
    let server = serve(|_, _| unavailable()).await;
    let client = APIClient::new(&server.url).build()?;
    let response = client.post("/").body("x").send().await?;
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(server.hits(), 1);
    Ok(())
}

#[tokio::test]
async fn none_policy_does_not_retry() -> TestResult {
    let server = serve(|_, _| unavailable()).await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy::none())
        .build()?;
    client.get("/").send().await?;
    assert_eq!(server.hits(), 1);
    Ok(())
}

#[tokio::test]
async fn idempotent_methods_retries_put_only_when_enabled() -> TestResult {
    let server = serve(|_, _| unavailable()).await;
    let client = APIClient::new(&server.url).build()?;
    client.put("/").send().await?;
    assert_eq!(server.hits(), 1);

    let server = serve(|_, _| unavailable()).await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy {
            idempotent_methods: true,
            base_delay: Duration::from_millis(1),
            ..RetryPolicy::default()
        })
        .build()?;
    client.put("/").send().await?;
    assert_eq!(server.hits(), 3);
    client.post("/").send().await?;
    assert_eq!(server.hits(), 4);
    Ok(())
}

#[tokio::test]
async fn timeouts_are_retried_unless_disabled() -> TestResult {
    let server = serve(|_, _| Reply::Hang).await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy {
            base_delay: Duration::from_millis(1),
            ..RetryPolicy::default()
        })
        .build()?;
    let err = client
        .get("/")
        .timeout(Duration::from_millis(100))
        .send()
        .await;
    assert!(matches!(&err, Err(APIClientError::Request(e)) if e.is_timeout()));
    assert_eq!(server.hits(), 3);

    let server = serve(|_, _| Reply::Hang).await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy {
            retry_timeouts: false,
            ..RetryPolicy::default()
        })
        .build()?;
    let err = client
        .get("/")
        .timeout(Duration::from_millis(100))
        .send()
        .await;
    assert!(matches!(&err, Err(APIClientError::Request(e)) if e.is_timeout()));
    assert_eq!(server.hits(), 1);
    Ok(())
}

#[tokio::test]
async fn retry_after_is_honoured_and_capped_end_to_end() -> TestResult {
    let server = serve(|_, _| {
        Reply::Raw(response(
            "503 Service Unavailable",
            &[("Retry-After", "30")],
            "",
        ))
    })
    .await;
    let client = APIClient::new(&server.url)
        .retry(RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(300),
            respect_retry_after: true,
            ..RetryPolicy::default()
        })
        .build()?;
    let started = Instant::now();
    client.get("/").send().await?;
    let took = started.elapsed();
    assert_eq!(server.hits(), 2);
    assert!(took >= Duration::from_millis(280), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    Ok(())
}
