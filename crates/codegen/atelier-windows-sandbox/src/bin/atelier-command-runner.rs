use atelier_windows_sandbox::CommandRequest;
use atelier_windows_sandbox::SandboxMode;
use atelier_windows_sandbox::run_command;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("atelier-command-runner: {err:#}");
            std::process::exit(2);
        }
    }
}

fn run() -> anyhow::Result<i32> {
    let mut args = std::env::args_os().skip(1).peekable();
    let mut mode = SandboxMode::ReadOnly;
    let mut roots = Vec::new();
    let mut cwd = None;
    let mut atelier_home = None;
    let mut timeout = None;
    let mut command: Vec<OsString> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--" => {
                command.extend(args);
                break;
            }
            "--help" | "-h" => {
                print_help();
                return Ok(0);
            }
            "--mode" => {
                mode = parse_mode(
                    &args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("missing --mode value"))?,
                )?;
            }
            "--root" => roots.push(PathBuf::from(
                args.next()
                    .ok_or_else(|| anyhow::anyhow!("missing --root value"))?,
            )),
            "--cwd" => {
                cwd = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("missing --cwd value"))?,
                ))
            }
            "--atelier-home" => {
                atelier_home =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("missing --atelier-home value")
                    })?));
            }
            "--timeout-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing --timeout-ms value"))?;
                timeout = Some(std::time::Duration::from_millis(
                    value.to_string_lossy().parse()?,
                ));
            }
            other => return Err(anyhow::anyhow!("unknown option: {other}")),
        }
    }

    let cwd = cwd.unwrap_or(std::env::current_dir()?);
    let program = command
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing command after --"))?;
    let args = command.into_iter().skip(1).collect();
    let mut request = CommandRequest::new(mode, roots, cwd, PathBuf::from(program), args);
    request.atelier_home = atelier_home;
    request.timeout = timeout;
    let output = run_command(request)?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(if output.timed_out {
        124
    } else {
        output.exit_code
    })
}

fn parse_mode(value: &OsString) -> anyhow::Result<SandboxMode> {
    match value.to_string_lossy().as_ref() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        other => Err(anyhow::anyhow!("unsupported sandbox mode: {other}")),
    }
}

fn print_help() {
    println!(
        "atelier-command-runner --root PATH [--root PATH] [--cwd PATH] \\
         [--mode read-only|workspace-write] [--timeout-ms N] -- PROGRAM [ARGS...]"
    );
}
