fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.to_string_lossy().as_ref(), "--help" | "-h"))
    {
        println!(
            "atelier-command-runner --root PATH [--root PATH] [--cwd PATH] \\
             [--mode read-only|workspace-write] [--timeout-ms N] -- PROGRAM [ARGS...]"
        );
        return;
    }
    match atelier_windows_sandbox::run_command_runner(args) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("atelier-command-runner: {err:#}");
            std::process::exit(2);
        }
    }
}
