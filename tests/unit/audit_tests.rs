use crate::audit::{pretty_body, shell_quote};

#[test]
fn test_shell_quote() {
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("safe"), "safe");
    assert_eq!(shell_quote("safe_with-123"), "safe_with-123");
    assert_eq!(shell_quote("needs quote"), "'needs quote'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    assert_eq!(shell_quote("complex chars: $?*"), "'complex chars: $?*'");
}

#[test]
fn test_pretty_body_json() {
    let json = r#"{"foo":"bar","baz":123}"#;
    let pretty = pretty_body(json.as_bytes());
    // Should be pretty printed JSON
    assert!(pretty.contains("\"foo\": \"bar\""));
    assert!(pretty.contains("\"baz\": 123"));
}

#[test]
fn test_pretty_body_non_json() {
    let text = "Hello world";
    let pretty = pretty_body(text.as_bytes());
    assert_eq!(pretty, text);
}
