use crate::cookies::CookieJar;
use reqwest::cookie::CookieStore;
use reqwest::header::HeaderValue;
use url::Url;

#[test]
fn test_cookie_jar_clear() -> Result<(), Box<dyn std::error::Error>> {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com")?;

    // Set a cookie
    let headers = [HeaderValue::from_static("session=123; Domain=example.com")];
    jar.set_cookies(&mut headers.iter(), &url);

    assert!(jar.cookies(&url).is_some());

    // Clear cookies
    jar.clear();
    assert!(jar.cookies(&url).is_none());

    Ok(())
}

#[test]
fn test_cookie_header() -> Result<(), Box<dyn std::error::Error>> {
    let jar = CookieJar::new();
    let url = Url::parse("https://example.com")?;

    let headers = [
        HeaderValue::from_static("a=1; Domain=example.com"),
        HeaderValue::from_static("b=2; Domain=example.com"),
    ];
    jar.set_cookies(&mut headers.iter(), &url);

    let cookie = jar.cookie_header(&url).ok_or("missing cookie header")?;
    let cookie_str = cookie.to_str()?;
    assert!(cookie_str.contains("a=1"));
    assert!(cookie_str.contains("b=2"));

    Ok(())
}
