#[test]
fn configured_request_agent_is_used_as_the_http_user_agent() {
    atelier_http::set_request_agent_identity("pi".to_owned(), Some("1.0".to_owned())).unwrap();

    let user_agent = atelier_http::process_user_agent_string();
    assert!(user_agent.starts_with("pi/1.0 ("), "{user_agent}");
    assert!(!user_agent.contains("atelier-shell"), "{user_agent}");

    let discovery = atelier_http::shared_client()
        .get("https://provider.example.test/v1/models")
        .build()
        .unwrap();
    assert_eq!(
        discovery
            .headers()
            .get(reqwest::header::USER_AGENT)
            .unwrap(),
        user_agent.as_str()
    );

    let oauth = atelier_http::shared_blocking_client()
        .post("https://login.example.test/oauth/token")
        .form(&[("refresh_token", "oauth-secret-must-not-enter-user-agent")])
        .build()
        .unwrap();
    let oauth_user_agent = oauth
        .headers()
        .get(reqwest::header::USER_AGENT)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(oauth_user_agent, user_agent);
    assert!(!oauth_user_agent.contains("oauth-secret"));
}
