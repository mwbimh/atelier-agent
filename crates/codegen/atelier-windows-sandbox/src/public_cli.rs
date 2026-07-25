use crate::setup::{SetupState, inspect_status, reset_public, setup_public};
use anyhow::{Result, anyhow};
use std::ffi::OsString;
use std::io::{BufRead, Write};

const USAGE: &str = "Usage:\n  ate sandbox setup\n  ate sandbox status [--json]\n  ate sandbox reset [--yes]\n\nCommands:\n  setup   Create the persistent Atelier sandbox accounts and WFP rules (opens one Windows UAC prompt)\n  status  Inspect account, credential, and WFP readiness without elevation\n  reset   Delete the Atelier sandbox accounts, credentials, and WFP rules\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicSandboxCommand {
    Setup,
    Status { json: bool },
    Reset { yes: bool },
    Help,
}

pub fn parse_public_sandbox_command<I, T>(args: I) -> Result<PublicSandboxCommand>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let command = args
        .next()
        .ok_or_else(|| anyhow!("missing sandbox command\n\n{USAGE}"))?;
    match command.to_string_lossy().as_ref() {
        "setup" => {
            reject_extra(args)?;
            Ok(PublicSandboxCommand::Setup)
        }
        "status" => {
            let mut json = false;
            for arg in args {
                match arg.to_string_lossy().as_ref() {
                    "--json" => json = true,
                    other => return Err(anyhow!("unknown status option: {other}\n\n{USAGE}")),
                }
            }
            Ok(PublicSandboxCommand::Status { json })
        }
        "reset" => {
            let mut yes = false;
            for arg in args {
                match arg.to_string_lossy().as_ref() {
                    "--yes" => yes = true,
                    other => return Err(anyhow!("unknown reset option: {other}\n\n{USAGE}")),
                }
            }
            Ok(PublicSandboxCommand::Reset { yes })
        }
        "help" | "--help" | "-h" => {
            reject_extra(args)?;
            Ok(PublicSandboxCommand::Help)
        }
        other => Err(anyhow!("unknown sandbox command: {other}\n\n{USAGE}")),
    }
}

fn reject_extra(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    if let Some(arg) = args.next() {
        return Err(anyhow!(
            "unexpected sandbox argument: {}\n\n{USAGE}",
            arg.to_string_lossy()
        ));
    }
    Ok(())
}

pub fn run_public_sandbox_command<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    match parse_public_sandbox_command(args)? {
        PublicSandboxCommand::Help => {
            print!("{USAGE}");
            Ok(0)
        }
        PublicSandboxCommand::Status { json } => {
            let status = inspect_status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Windows sandbox: {}", status.state.as_str());
                println!(
                    "  account (network allowed):  {}",
                    if status.network_allowed_account_exists {
                        "present"
                    } else {
                        "missing"
                    }
                );
                println!(
                    "  account (network disabled): {}",
                    if status.network_disabled_account_exists {
                        "present"
                    } else {
                        "missing"
                    }
                );
                println!(
                    "  marker:      {}",
                    if status.marker_valid {
                        "valid"
                    } else {
                        "missing or invalid"
                    }
                );
                println!(
                    "  credentials: {}",
                    if status.credentials_valid {
                        "valid"
                    } else {
                        "missing or invalid"
                    }
                );
                println!(
                    "  WFP filters: {}",
                    if status.wfp_filters_ready {
                        "ready"
                    } else {
                        "missing or invalid"
                    }
                );
                println!("  home:        {}", status.atelier_home.display());
                if status.state != SetupState::Ready {
                    println!("  next:        ate sandbox setup");
                }
            }
            Ok(if status.state == SetupState::Ready {
                0
            } else {
                1
            })
        }
        PublicSandboxCommand::Setup => {
            let executable = crate::materialize::runner_source()?;
            if setup_public(&executable)? {
                println!("Windows sandbox setup complete.");
            } else {
                println!("Windows sandbox is already ready.");
            }
            Ok(0)
        }
        PublicSandboxCommand::Reset { yes } => {
            if !yes && !confirm_reset(&mut std::io::stdin().lock(), &mut std::io::stderr())? {
                eprintln!("Windows sandbox reset cancelled.");
                return Ok(1);
            }
            let executable = crate::materialize::runner_source()?;
            if reset_public(&executable)? {
                println!("Atelier sandbox accounts, credentials, and WFP rules removed.");
            } else {
                println!("Windows sandbox is already reset.");
            }
            Ok(0)
        }
    }
}

fn confirm_reset(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<bool> {
    write!(
        writer,
        "This deletes the Atelier sandbox local accounts, credentials, and WFP rules. Type 'reset' to continue: "
    )?;
    writer.flush()?;
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    Ok(answer.trim() == "reset")
}

#[cfg(test)]
mod tests {
    #[test]
    fn reset_confirmation_requires_the_exact_word() {
        for (input, expected) in [("reset\n", true), ("yes\n", false), ("RESET\n", false)] {
            let mut input = std::io::Cursor::new(input.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                super::confirm_reset(&mut input, &mut output).unwrap(),
                expected
            );
            assert!(String::from_utf8(output).unwrap().contains("Type 'reset'"));
        }
    }
}
