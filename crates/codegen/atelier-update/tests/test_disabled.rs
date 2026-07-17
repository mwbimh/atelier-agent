use std::path::Path;

use atelier_update::UpdateConfig;
use atelier_update::auto_update::{
    UpdateRunMode, check_update_background, check_update_status, download_silent,
    ensure_latest_on_disk, run_install_script, run_update, run_update_if_available,
};
use atelier_update::version::{fetch_latest_version, get_latest_version};

const DISABLED: &str = "automatic updates disabled";

fn update_config() -> UpdateConfig {
    UpdateConfig {
        proxy_base_url: String::new(),
        auth_scope: String::new(),
        deployment_key: None,
        alpha_test_key: None,
        channel: "stable".to_string(),
        npm_registry: None,
    }
}

fn assert_disabled(error: anyhow::Error) {
    assert_eq!(error.to_string(), DISABLED);
}

#[tokio::test]
async fn version_probes_never_leave_the_process() {
    let config = update_config();

    assert_disabled(fetch_latest_version("internal", &config).await.unwrap_err());
    assert_disabled(get_latest_version("npm", &config).await.unwrap_err());
}

#[tokio::test]
async fn network_and_install_entry_points_are_disabled() {
    let config = update_config();

    assert_disabled(
        download_silent("unused-url", Path::new("unused"))
            .await
            .unwrap_err(),
    );
    assert_disabled(
        run_install_script("internal", None, &config)
            .await
            .unwrap_err(),
    );

    let mut config = config;
    assert_disabled(
        run_update(false, None, None, &mut config)
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn background_and_status_paths_report_no_update() {
    let config = update_config();

    let check = check_update_background(&config).await;
    assert!(check.update.is_none());
    assert!(check.download.is_none());

    let outcome = ensure_latest_on_disk(&config).await.unwrap();
    assert_eq!(outcome.installed, None);
    assert!(!outcome.relaunch_needed);

    let status = check_update_status(&config).await;
    assert!(!status.update_available);
    assert_eq!(status.latest_version, None);
    assert_eq!(status.error.as_deref(), Some(DISABLED));

    assert_disabled(
        run_update_if_available(UpdateRunMode::Blocking, false, &config)
            .await
            .unwrap_err(),
    );
}
