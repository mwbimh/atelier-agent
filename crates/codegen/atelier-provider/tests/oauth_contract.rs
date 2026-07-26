use atelier_provider::SecretString;
use atelier_provider::auth::{
    AuthorizationCodeConfig, AuthorizationCodeSession, DeviceCodeConfig, DeviceCodePoll,
    DeviceCodeSession, OAuthCredential, OAuthError, OAuthHttpClient, OAuthHttpResponse,
    OAuthSecretStore, ProviderOAuthCredentialStore, ProviderOAuthMethod, RefreshTokenConfig,
    refresh_credential,
};
use atelier_provider::{
    CredentialRef, ProviderAuth, ProviderConfig, ProviderDiscovery, ProviderRegistry,
};
use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::tempdir;
use url::Url;

#[derive(Default)]
struct ScriptedHttpClient {
    responses: Mutex<VecDeque<OAuthHttpResponse>>,
    requests: Mutex<Vec<(Url, BTreeMap<String, String>)>>,
}

impl ScriptedHttpClient {
    fn with_responses(responses: impl IntoIterator<Item = OAuthHttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<(Url, BTreeMap<String, String>)> {
        self.requests.lock().unwrap().clone()
    }
}

impl OAuthHttpClient for ScriptedHttpClient {
    fn post_form(
        &self,
        url: &Url,
        form: &[(String, String)],
    ) -> Result<OAuthHttpResponse, OAuthError> {
        self.requests
            .lock()
            .unwrap()
            .push((url.clone(), form.iter().cloned().collect()));
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| OAuthError::transport("scripted response exhausted"))
    }
}

#[derive(Default)]
struct MemorySecretStore {
    values: Mutex<BTreeMap<(String, String), SecretString>>,
}

impl OAuthSecretStore for MemorySecretStore {
    fn read(&self, service: &str, account: &str) -> Result<Option<SecretString>, OAuthError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn write(&self, service: &str, account: &str, secret: SecretString) -> Result<(), OAuthError> {
        self.values
            .lock()
            .unwrap()
            .insert((service.to_owned(), account.to_owned()), secret);
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), OAuthError> {
        self.values
            .lock()
            .unwrap()
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

fn json_response(status: u16, value: serde_json::Value) -> OAuthHttpResponse {
    OAuthHttpResponse {
        status,
        body: serde_json::to_vec(&value).unwrap(),
    }
}

#[test]
fn oauth_debug_output_never_contains_tokens() {
    let credential = OAuthCredential::new("access-secret", Some("refresh-secret"), 1_000);
    let response = json_response(
        200,
        serde_json::json!({
            "access_token": "response-access-secret",
            "refresh_token": "response-refresh-secret"
        }),
    );

    let credential_debug = format!("{credential:?}");
    let response_debug = format!("{response:?}");
    assert!(!credential_debug.contains("access-secret"));
    assert!(!credential_debug.contains("refresh-secret"));
    assert!(!response_debug.contains("response-access-secret"));
    assert!(!response_debug.contains("response-refresh-secret"));
}

#[test]
fn authorization_code_pkce_uses_a_real_localhost_callback_and_exchanges_the_verifier() {
    let mut config = AuthorizationCodeConfig::new(
        "allm",
        "atelier-client",
        Url::parse("https://login.example.test/oauth/authorize").unwrap(),
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    config.scopes = vec!["openid".into(), "offline_access".into()];
    config
        .authorization_params
        .insert("audience".into(), "models".into());

    let session = AuthorizationCodeSession::begin(config).unwrap();
    let authorization_url = session.authorization_url().clone();
    let query = authorization_url
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query["code_challenge"].contains('='));
    assert_eq!(query["scope"], "openid offline_access");
    assert_eq!(query["audience"], "models");
    assert_eq!(query["redirect_uri"], session.redirect_uri().as_str());

    let callback = format!(
        "{}?code=authorization-code&state={}",
        session.callback_path(),
        session.state()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", session.callback_port())).unwrap();
    write!(
        stream,
        "GET {callback} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();

    let client = ScriptedHttpClient::with_responses([json_response(
        200,
        serde_json::json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "expires_in": 3600,
            "token_type": "Bearer"
        }),
    )]);
    let credential = session.complete(&client, Duration::from_secs(1)).unwrap();
    assert_eq!(credential.access_token.expose_secret(), "access-1");
    assert_eq!(
        credential.refresh_token.as_ref().unwrap().expose_secret(),
        "refresh-1"
    );

    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    let (_, form) = &requests[0];
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "authorization-code");
    assert_eq!(form["client_id"], "atelier-client");
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    assert!(form["code_verifier"].len() >= 43);
}

#[test]
fn authorization_code_rejects_a_callback_with_the_wrong_state_before_token_exchange() {
    let config = AuthorizationCodeConfig::new(
        "allm",
        "atelier-client",
        Url::parse("https://login.example.test/oauth/authorize").unwrap(),
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    let session = AuthorizationCodeSession::begin(config).unwrap();
    let callback = format!(
        "{}?code=authorization-code&state=wrong",
        session.callback_path()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", session.callback_port())).unwrap();
    write!(
        stream,
        "GET {callback} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();

    let client = ScriptedHttpClient::default();
    let error = session
        .complete(&client, Duration::from_secs(1))
        .unwrap_err();
    assert!(matches!(error, OAuthError::StateMismatch));
    assert!(client.requests().is_empty());
}

#[test]
fn device_code_handles_pending_slow_down_and_success() {
    let mut config = DeviceCodeConfig::new(
        "allm",
        "atelier-client",
        Url::parse("https://login.example.test/oauth/device/code").unwrap(),
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    config.scopes = vec!["openid".into(), "offline_access".into()];
    let client = ScriptedHttpClient::with_responses([
        json_response(
            200,
            serde_json::json!({
                "device_code": "device-1",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://login.example.test/device",
                "verification_uri_complete": "https://login.example.test/device?user_code=ABCD-EFGH",
                "expires_in": 900,
                "interval": 2
            }),
        ),
        json_response(400, serde_json::json!({"error": "authorization_pending"})),
        json_response(400, serde_json::json!({"error": "slow_down"})),
        json_response(
            200,
            serde_json::json!({
                "access_token": "device-access",
                "refresh_token": "device-refresh",
                "expires_in": 3600,
                "token_type": "Bearer"
            }),
        ),
    ]);

    let mut session = DeviceCodeSession::begin(&client, config).unwrap();
    assert_eq!(session.user_code(), "ABCD-EFGH");
    assert_eq!(session.poll_interval(), Duration::from_secs(2));
    assert!(matches!(
        session.poll_once(&client).unwrap(),
        DeviceCodePoll::Pending
    ));
    assert!(matches!(
        session.poll_once(&client).unwrap(),
        DeviceCodePoll::SlowDown
    ));
    assert_eq!(session.poll_interval(), Duration::from_secs(7));
    let DeviceCodePoll::Complete(credential) = session.poll_once(&client).unwrap() else {
        panic!("expected completed device authorization");
    };
    assert_eq!(credential.access_token.expose_secret(), "device-access");

    let requests = client.requests();
    assert_eq!(requests[0].1["scope"], "openid offline_access");
    assert_eq!(
        requests[1].1["grant_type"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(requests[1].1["device_code"], "device-1");
}

#[test]
fn refresh_token_rotation_replaces_a_returned_refresh_token_and_preserves_it_when_omitted() {
    let config = RefreshTokenConfig::new(
        "allm",
        "atelier-client",
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    let original = OAuthCredential::new("old-access", Some("old-refresh"), 1_000);
    let rotating = ScriptedHttpClient::with_responses([json_response(
        200,
        serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600
        }),
    )]);
    let rotated = refresh_credential(&rotating, &config, &original).unwrap();
    assert_eq!(rotated.access_token.expose_secret(), "new-access");
    assert_eq!(
        rotated.refresh_token.as_ref().unwrap().expose_secret(),
        "new-refresh"
    );

    let preserving = ScriptedHttpClient::with_responses([json_response(
        200,
        serde_json::json!({
            "access_token": "newer-access",
            "expires_in": 3600
        }),
    )]);
    let preserved = refresh_credential(&preserving, &config, &rotated).unwrap();
    assert_eq!(
        preserved.refresh_token.as_ref().unwrap().expose_secret(),
        "new-refresh"
    );
}

#[test]
fn provider_credentials_use_a_provider_namespace_and_never_write_tokens_to_disk() {
    let home = tempdir().unwrap();
    let store = ProviderOAuthCredentialStore::new(home.path(), MemorySecretStore::default());
    let credential = OAuthCredential::new("access-secret", Some("refresh-secret"), 1_000);

    store.save("allm", &credential).unwrap();
    let namespace = store.namespace("allm").unwrap();
    assert_eq!(
        namespace.directory(),
        home.path()
            .join("credentials")
            .join("oauth")
            .join("providers")
            .join("allm")
    );
    assert!(
        !namespace
            .directory()
            .starts_with(home.path().join("credentials").join("oauth").join("mcp"))
    );
    assert!(namespace.metadata_path().is_file());
    let metadata = std::fs::read_to_string(namespace.metadata_path()).unwrap();
    assert!(metadata.contains("allm"));
    assert!(!metadata.contains("access-secret"));
    assert!(!metadata.contains("refresh-secret"));

    let loaded = store.load("allm").unwrap().unwrap();
    assert_eq!(loaded.access_token.expose_secret(), "access-secret");
    assert_eq!(
        loaded.refresh_token.as_ref().unwrap().expose_secret(),
        "refresh-secret"
    );
    assert!(store.namespace("../other").is_err());
}

#[test]
fn failed_refresh_does_not_replace_the_stored_provider_credential() {
    let home = tempdir().unwrap();
    let store = ProviderOAuthCredentialStore::new(home.path(), MemorySecretStore::default());
    store
        .save(
            "allm",
            &OAuthCredential::new("old-access", Some("old-refresh"), 1_000),
        )
        .unwrap();
    let config = RefreshTokenConfig::new(
        "allm",
        "atelier-client",
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    let client = ScriptedHttpClient::with_responses([json_response(
        400,
        serde_json::json!({"error": "invalid_grant"}),
    )]);

    assert!(store.refresh(&client, &config).is_err());
    let current = store.load("allm").unwrap().unwrap();
    assert_eq!(current.access_token.expose_secret(), "old-access");
    assert_eq!(
        current.refresh_token.as_ref().unwrap().expose_secret(),
        "old-refresh"
    );
}

#[test]
fn provider_oauth_methods_round_trip_without_storage_v2_changes() {
    let home = tempdir().unwrap();
    let path = home.path().join("providers.toml");
    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry
        .upsert_provider(ProviderConfig {
            id: "allm".into(),
            display_name: "AllM".into(),
            auth: ProviderAuth::Bearer,
            base_url: Url::parse("https://api.example.test/v1").unwrap(),
            credential: CredentialRef::OAuth {
                provider_id: "allm".into(),
                methods: vec![
                    ProviderOAuthMethod::authorization_code(
                        "desktop-client",
                        Url::parse("https://login.example.test/oauth/authorize").unwrap(),
                        Url::parse("https://login.example.test/oauth/token").unwrap(),
                    ),
                    ProviderOAuthMethod::device_code(
                        "desktop-client",
                        Url::parse("https://login.example.test/oauth/device/code").unwrap(),
                        Url::parse("https://login.example.test/oauth/token").unwrap(),
                    ),
                ],
            },
            discovery: ProviderDiscovery::Disabled,
            extra_headers: BTreeMap::new(),
            enabled: true,
        })
        .unwrap();
    registry.save().unwrap();

    let reloaded = ProviderRegistry::load_or_create(path).unwrap();
    let CredentialRef::OAuth {
        provider_id,
        methods,
    } = &reloaded.provider("allm").unwrap().credential
    else {
        panic!("expected OAuth credential schema");
    };
    assert_eq!(provider_id, "allm");
    assert_eq!(methods.len(), 2);
    assert_eq!(methods[0].flow_name(), "authorization-code");
    assert_eq!(methods[1].flow_name(), "device-code");
}

#[test]
fn provider_oauth_schema_rejects_missing_client_configuration_and_provider_mismatch() {
    let invalid = ProviderOAuthMethod::authorization_code(
        "",
        Url::parse("https://login.example.test/oauth/authorize").unwrap(),
        Url::parse("https://login.example.test/oauth/token").unwrap(),
    );
    assert!(invalid.validate("allm").is_err());

    let config = ProviderConfig {
        id: "allm".into(),
        display_name: "AllM".into(),
        auth: ProviderAuth::Bearer,
        base_url: Url::parse("https://api.example.test/v1").unwrap(),
        credential: CredentialRef::OAuth {
            provider_id: "other".into(),
            methods: vec![ProviderOAuthMethod::device_code(
                "desktop-client",
                Url::parse("https://login.example.test/oauth/device/code").unwrap(),
                Url::parse("https://login.example.test/oauth/token").unwrap(),
            )],
        },
        discovery: ProviderDiscovery::Disabled,
        extra_headers: BTreeMap::new(),
        enabled: true,
    };
    assert!(config.validate().is_err());
}
