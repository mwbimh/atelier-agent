use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn quote_windows_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '"'));
    if !needs_quotes {
        return arg.to_owned();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub fn argv_to_command_line(program: &Path, args: &[OsString]) -> String {
    std::iter::once(program.as_os_str().to_string_lossy().into_owned())
        .chain(args.iter().map(|arg| arg.to_string_lossy().into_owned()))
        .map(|arg| quote_windows_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn win_error(context: &str) -> anyhow::Error {
    anyhow::anyhow!("{context}: {}", std::io::Error::last_os_error())
}

pub fn win32_error(context: &str, code: u32) -> anyhow::Error {
    let message = std::io::Error::from_raw_os_error(code as i32);
    anyhow::anyhow!("{context}: {code} ({message})")
}

pub fn path_to_wide(path: &Path) -> Vec<u16> {
    to_wide(path.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::argv_to_command_line;
    use super::quote_windows_arg;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn quotes_windows_arguments_without_merging_them() {
        let args = vec![OsString::from("Write-Output \"hello world\"")];
        assert_eq!(
            argv_to_command_line(Path::new("pwsh.exe"), &args),
            "pwsh.exe \"Write-Output \\\"hello world\\\"\""
        );
    }

    #[test]
    fn doubles_trailing_backslashes_inside_quotes() {
        assert_eq!(
            quote_windows_arg(r"C:\Program Files\"),
            r#""C:\Program Files\\""#
        );
    }
}
