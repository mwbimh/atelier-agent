use anyhow::{Context, Result, anyhow};
use atelier_config::runtime_defaults::{SandboxPreference, update_sandbox_preference_at};
use atelier_shell::agent::config::SandboxSettingsConfig;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SandboxChoice {
    Enable,
    Disable,
}

fn should_offer_onboarding(
    interactive_startup: bool,
    terminal_attached: bool,
    native_backend: bool,
    sandbox_enabled: bool,
    setup_ready: bool,
) -> bool {
    interactive_startup && terminal_attached && native_backend && sandbox_enabled && !setup_ready
}

fn prompt_sandbox_choice(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<SandboxChoice> {
    writeln!(
        writer,
        "Atelier can isolate commands and workspace access with its Windows sandbox."
    )?;
    writeln!(
        writer,
        "Enabling it creates dedicated local accounts and WFP rules and requires one Windows administrator approval."
    )?;
    loop {
        write!(writer, "Enable the Windows sandbox now? [Y/n]: ")?;
        writer.flush()?;
        let mut answer = String::new();
        if reader.read_line(&mut answer)? == 0 {
            return Err(anyhow!("Windows sandbox selection was cancelled"));
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(SandboxChoice::Enable),
            "n" | "no" => return Ok(SandboxChoice::Disable),
            _ => writeln!(writer, "Please answer yes or no.")?,
        }
    }
}

fn setup_ready() -> Result<bool> {
    let status = atelier_windows_sandbox::setup_status()?;
    Ok(status.network_allowed_account_exists
        && status.network_disabled_account_exists
        && status.marker_valid
        && status.credentials_valid
        && status.wfp_filters_ready)
}

pub fn configure_if_needed(
    home: &Path,
    interactive_startup: bool,
    cli_profile: Option<&str>,
) -> Result<()> {
    let config = SandboxSettingsConfig::from_effective_config();
    let backend = config
        .resolve_backend()
        .context("resolve Windows sandbox backend before onboarding")?;
    let profile = config.resolve_profile(cli_profile, None).value;
    let sandbox_enabled = profile
        .parse::<atelier_sandbox::ProfileName>()
        .map(|profile| profile != atelier_sandbox::ProfileName::Off)
        .map_err(|error| anyhow!("resolve Windows sandbox profile before onboarding: {error}"))?;
    let terminal_attached = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !should_offer_onboarding(
        interactive_startup,
        terminal_attached,
        !backend.is_unsafe(),
        sandbox_enabled,
        setup_ready()?,
    ) {
        return Ok(());
    }

    let choice =
        prompt_sandbox_choice(&mut std::io::stdin().lock(), &mut std::io::stderr().lock())?;
    match choice {
        SandboxChoice::Enable => {
            let code = atelier_windows_sandbox::run_public_sandbox_command(["setup"])?;
            if code != 0 {
                return Err(anyhow!("Windows sandbox setup exited with status {code}"));
            }
        }
        SandboxChoice::Disable => {
            update_sandbox_preference_at(home, SandboxPreference::Disabled).with_context(|| {
                format!(
                    "save disabled Windows sandbox preference in {}",
                    home.join("config.toml").display()
                )
            })?;
            writeln!(
                std::io::stderr(),
                "Windows sandbox disabled. Commands will run without OS isolation. Run `ate sandbox setup` to enable it later."
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_is_only_offered_for_interactive_native_sandbox_startup() {
        assert!(should_offer_onboarding(true, true, true, true, false));
        assert!(!should_offer_onboarding(false, true, true, true, false));
        assert!(!should_offer_onboarding(true, false, true, true, false));
        assert!(!should_offer_onboarding(true, true, false, true, false));
        assert!(!should_offer_onboarding(true, true, true, false, false));
        assert!(!should_offer_onboarding(true, true, true, true, true));
    }

    #[test]
    fn prompt_defaults_to_enabling_the_sandbox() {
        for input in ["\n", "y\n", "Y\n", "yes\n", "YES\n"] {
            let mut reader = std::io::Cursor::new(input.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                prompt_sandbox_choice(&mut reader, &mut output).unwrap(),
                SandboxChoice::Enable,
                "input={input:?}"
            );
            assert!(String::from_utf8(output).unwrap().contains("[Y/n]"));
        }
    }

    #[test]
    fn prompt_accepts_no_and_retries_invalid_input() {
        let mut reader = std::io::Cursor::new(b"maybe\nn\n");
        let mut output = Vec::new();
        assert_eq!(
            prompt_sandbox_choice(&mut reader, &mut output).unwrap(),
            SandboxChoice::Disable
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Please answer yes or no."));
        assert!(output.matches("[Y/n]").count() >= 2);
    }

    #[test]
    fn prompt_fails_closed_when_input_ends_without_a_choice() {
        let mut reader = std::io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let error = prompt_sandbox_choice(&mut reader, &mut output)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cancelled"), "{error}");
    }
}
