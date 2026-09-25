//! Starting a batch script as a target.
//!
//! `CreateProcessW` given a `.bat` or a `.cmd` starts the command interpreter itself, as
//! `cmd.exe /c <command line>`, with no quotes around the line (measured through `%CMDCMDLINE%`).
//! The interpreter keeps the quotes of that line only when it holds exactly one pair, with no special
//! character inside it, and otherwise strips the first quote and the last one (Microsoft Learn,
//! `cmd`). The line always holds the pair around the script, so any other quote broke it: an
//! argument with a space, an empty one, or a script in a folder with `&` in its name kept the script
//! from starting, with the session reporting a target that vanished, an empty argument vanished and
//! shifted the rest, and `a&b` ran its second half as a command of its own.
//!
//! Microsoft Learn says the caller must start the interpreter for a batch file, and this does:
//! `cmd.exe /e:ON /v:OFF /c ""<script>" <arguments>"`, whose outer pair is the one the interpreter
//! strips. Each argument is quoted the way Rust's standard library quotes one for a batch script
//! (`append_bat_arg` in `library/std/src/sys/args/windows.rs`, reworked after CVE-2024-24576),
//! including its handling of `%`. The switches are the two the arguments need and no more (ADR-15):
//! `/e:ON` because that `%` handling works through a command extension, and `/v:OFF` so a `!` in an
//! argument is not expanded. Rust also passes `/d`, which skips the AutoRun commands in the registry.
//! It is left out on purpose, so the script runs in the setting it has when started any other way.

use std::path::Path;

use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

/// Whether `CreateProcessW` would hand this file to the command interpreter: a `.bat` or a `.cmd`,
/// in any case.
pub fn is_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"))
}

/// Why these arguments cannot be given to a batch script, or `None` when they can. A line break
/// ends the interpreter's command line where it stands, and a zero character ends the string
/// `CreateProcessW` reads, so either would hand the script less than it was given.
pub fn batch_arguments_problem(args: &[String]) -> Option<String> {
    let broken = args.iter().find(|a| a.contains(['\r', '\n', '\0']))?;
    Some(format!(
        "the argument {broken:?} holds a line break or a zero character, which would cut the command \
         line of a batch script short"
    ))
}

/// The application and the command line that start `script` through the command interpreter.
///
/// The script path is made absolute first. `CreateProcessW` resolved a relative one against this
/// process's folder, while the interpreter would resolve it against the target's working folder,
/// which `--cwd` can move.
pub(crate) fn interpreter_line(script: &str, args: &[String]) -> Result<(String, String), String> {
    if let Some(problem) = batch_arguments_problem(args) {
        return Err(problem);
    }
    let script = user_path(script)?;
    let mut line = format!("cmd.exe /e:ON /v:OFF /c \"\"{script}\"");
    for arg in args {
        line.push(' ');
        push_batch_arg(&mut line, arg);
    }
    line.push('"');
    Ok((command_interpreter()?, line))
}

/// The interpreter in the system folder, never one found by a search: a `cmd.exe` planted in the
/// working folder would otherwise run in its place.
fn command_interpreter() -> Result<String, String> {
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer is ours and its length goes with it. The call writes at most that much and
    // returns how much it wrote, or the size it needs when that is more.
    let written = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    let folder = buffer
        .get(..written)
        .filter(|_| written > 0 && written < buffer.len())
        .ok_or("GetSystemDirectoryW did not name the system folder")?;
    Ok(format!(r"{}\cmd.exe", String::from_utf16_lossy(folder)))
}

/// The script path, absolute and without a verbatim prefix, which the interpreter cannot run a script
/// by. `\\?\C:\x` names `C:\x` and `\\?\UNC\host\share` names `\\host\share`. Any other verbatim form
/// is left as it is, and the interpreter refuses it as it did before.
fn user_path(script: &str) -> Result<String, String> {
    let absolute = std::path::absolute(script)
        .map_err(|e| format!("the script path '{script}' cannot be made absolute: {e}"))?;
    let text = absolute.to_string_lossy().into_owned();
    // A quote would close the pair around the script. Windows allows none in a file name, so such a
    // path names no file, and it is refused here rather than handed to the interpreter half-quoted.
    if text.contains('"') {
        return Err(format!("the script path '{script}' holds a quote, which no Windows file name can"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Ok(format!(r"\\{rest}"));
    }
    let drive = text
        .strip_prefix(r"\\?\")
        .filter(|rest| matches!(rest.as_bytes(), [letter, b':', b'\\', ..] if letter.is_ascii_alphabetic()))
        .map(str::to_string);
    Ok(drive.unwrap_or(text))
}

/// One argument as the batch script reads it back: bare when it is only letters, digits and a few
/// safe marks, quoted otherwise, with a quote inside it doubled and the backslashes before a quote
/// doubled. A `%` is followed by an empty substring of `%cd%`, which the interpreter expands to
/// nothing, so that `%OS%` reaches the script as those four characters and not as the variable. A
/// trailing backslash forces the quotes, so a script that quotes `%~1` again does not escape its own
/// closing quote.
fn push_batch_arg(line: &mut String, arg: &str) {
    const UNQUOTED: &str = r"#$*+-./:?@\_";
    let quote = arg.is_empty()
        || arg.ends_with('\\')
        || arg
            .chars()
            .any(|c| (c.is_ascii() && !(c.is_ascii_alphanumeric() || UNQUOTED.contains(c))) || c.is_control());
    if quote {
        line.push('"');
    }
    let mut backslashes = 0usize;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                line.extend(std::iter::repeat_n('\\', backslashes));
                line.push('"');
            } else if c == '%' {
                line.push_str("%%cd:~,");
            }
            backslashes = 0;
        }
        line.push(c);
    }
    if quote {
        line.extend(std::iter::repeat_n('\\', backslashes));
        line.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_script_is_known_by_its_extension_in_any_case() {
        for yes in ["run.bat", "RUN.BAT", r"C:\a b\x.Cmd", "x.y.bat"] {
            assert!(is_batch_script(Path::new(yes)), "{yes}");
        }
        for no in ["app.exe", "bat", "run.bat.exe", "run.batch", "run.cm"] {
            assert!(!is_batch_script(Path::new(no)), "{no}");
        }
    }

    /// Each row measured with a script that writes back `%~1`, through the interpreter started this
    /// way (tools/probes/pe-edge, 2026-09-25): what the right-hand side gives is what the script read.
    #[test]
    fn each_argument_is_quoted_so_the_script_reads_it_back() {
        for (arg, quoted) in [
            ("two", "two"),
            (r"C:\path\file", r"C:\path\file"),
            ("one a", "\"one a\""),
            ("a&b", "\"a&b\""),
            ("50%", "\"50%%cd:~,%\""),
            ("%OS%", "\"%%cd:~,%OS%%cd:~,%\""),
            ("", "\"\""),
            (r"C:\dir\", r#""C:\dir\\""#),
            ("say \"hi\"", "\"say \"\"hi\"\"\""),
            ("a!b", "\"a!b\""),
        ] {
            let mut line = String::new();
            push_batch_arg(&mut line, arg);
            assert_eq!(line, quoted, "{arg:?}");
        }
    }

    /// The whole line: the interpreter from the system folder, the two switches, and the outer pair of
    /// quotes around the script and its arguments, which is the pair the interpreter strips.
    #[test]
    fn the_script_runs_through_the_interpreter_with_the_line_in_an_outer_pair_of_quotes() {
        let (app, line) = interpreter_line(r"C:\R&D (x86)\run it.bat", &["one a".to_string(), "x".to_string()])
            .expect("a line");
        assert!(app.to_ascii_lowercase().ends_with(r"\system32\cmd.exe"), "{app}");
        assert_eq!(line, r#"cmd.exe /e:ON /v:OFF /c ""C:\R&D (x86)\run it.bat" "one a" x""#);
        let (_, bare) = interpreter_line(r"C:\x\run.bat", &[]).expect("a line");
        assert_eq!(bare, r#"cmd.exe /e:ON /v:OFF /c ""C:\x\run.bat"""#);
        // AutoRun stays (ADR-15): the switch that would skip it is not on the line.
        assert!(!line.contains("/d"), "{line}");
    }

    #[test]
    fn the_script_path_is_absolute_and_never_verbatim() {
        let here = std::env::current_dir().expect("a current folder");
        let (_, relative) = interpreter_line("run.bat", &[]).expect("a line");
        assert!(relative.contains(&format!("\"{}\"", here.join("run.bat").display())), "{relative}");
        assert_eq!(user_path(r"\\?\C:\x\run.bat").unwrap(), r"C:\x\run.bat");
        assert_eq!(user_path(r"\\?\UNC\host\share\run.bat").unwrap(), r"\\host\share\run.bat");
        assert!(user_path(r#"C:\x"y\run.bat"#).is_err(), "a quote would close the pair around the script");
    }

    #[test]
    fn an_argument_that_would_cut_the_line_short_is_refused() {
        for bad in ["a\nb", "a\rb", "a\0b"] {
            let args = vec!["fine".to_string(), bad.to_string()];
            assert!(batch_arguments_problem(&args).is_some(), "{bad:?}");
            assert!(interpreter_line(r"C:\x\run.bat", &args).is_err(), "{bad:?}");
        }
        assert!(batch_arguments_problem(&["one a".to_string(), String::new()]).is_none());
    }
}
