//! What the page on checking a download shows, taken from the two files the release itself is held to.
//!
//! * `{{verify_commands}}`: the attestation commands of the `<!-- verify-commands -->` block in
//!   `README.md`. `.github/workflows/verify-release.yml` runs that block as written on every published
//!   release, so the page shows commands that were run, not a copy of them that can drift. The block is
//!   read here by the rule the workflow reads it by: the first fenced block between the two markers,
//!   one command a line, with blank lines and `#` lines left out, and at least two commands.
//! * `{{codesign_sha256}}`: the SHA-256 fingerprint of the signing certificate, pinned in
//!   `packaging/codesign.json`, which the same workflow checks every signed file against. It is shown in
//!   capitals, as `GetCertHashString('SHA256')` prints it. The certificate's subject is not in that file
//!   and so it cannot reach the page.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::{esc, parse_json, read_to_string, Result};

const OPEN: &str = "<!-- verify-commands -->";
const CLOSE: &str = "<!-- /verify-commands -->";

/// Fewer than this and the workflow refuses the README, so the page refuses it too.
const MIN_COMMANDS: usize = 2;

#[derive(Debug, Deserialize)]
struct Codesign {
    certificate_sha256: String,
}

pub fn tokens(root: &Path) -> Result<BTreeMap<String, String>> {
    let readme = read_to_string(&root.join("README.md"))?;
    let commands = commands(&readme).map_err(|e| format!("README.md: {e}"))?;
    let codesign: Codesign = parse_json(&root.join("packaging").join("codesign.json"))?;
    let fingerprint =
        fingerprint(&codesign.certificate_sha256).map_err(|e| format!("packaging/codesign.json: {e}"))?;
    let mut t = BTreeMap::new();
    t.insert("verify_commands".into(), terminal(&commands));
    t.insert("codesign_sha256".into(), fingerprint);
    Ok(t)
}

/// The commands of the verify block, as the workflow takes them.
fn commands(readme: &str) -> Result<Vec<String>> {
    let start = readme.find(OPEN).ok_or_else(|| format!("no {OPEN} block"))? + OPEN.len();
    let end = readme[start..]
        .find(CLOSE)
        .ok_or_else(|| format!("{OPEN} is never closed by {CLOSE}"))?
        + start;
    let block = readme[start..end].trim();
    let inner = block
        .strip_prefix("```")
        .and_then(|b| b.strip_suffix("```"))
        .ok_or_else(|| "the verify block holds no fenced code block, which the release workflow needs".to_string())?;
    // The first line of the fence may name a language. The workflow takes it as a command that is
    // empty when there is none, so a name there would be run, and is refused here instead.
    let (info, body) = inner.split_once('\n').unwrap_or((inner, ""));
    if !info.trim().is_empty() {
        return Err(format!(
            "the verify block's fence names '{}', which the release workflow would run as a command",
            info.trim()
        ));
    }
    let commands: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    if commands.len() < MIN_COMMANDS {
        return Err(format!(
            "the verify block has {} command(s), and the release workflow wants at least {MIN_COMMANDS}",
            commands.len()
        ));
    }
    Ok(commands)
}

/// A terminal block in the site's style: the program in its own colour, the rest as typed.
fn terminal(commands: &[String]) -> String {
    let lines: Vec<String> = commands
        .iter()
        .map(|c| match c.split_once(' ') {
            Some((program, rest)) => format!("<span class=\"p\">{}</span> {}", esc(program), esc(rest)),
            None => format!("<span class=\"p\">{}</span>", esc(c)),
        })
        .collect();
    format!("<pre class=\"term\">{}</pre>", lines.join("\n"))
}

fn fingerprint(pin: &str) -> Result<String> {
    if pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(pin.to_ascii_uppercase())
    } else {
        Err("certificate_sha256 is not 64 lower-case hexadecimal digits".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readme(block: &str) -> String {
        format!("intro\n\n{OPEN}\n{block}\n{CLOSE}\n\noutro\n")
    }

    #[test]
    fn the_commands_are_the_lines_the_workflow_runs() {
        let r = readme("```\ngh one --a\n\n# a note\n  gh two --b  \n```");
        assert_eq!(commands(&r).unwrap(), ["gh one --a", "gh two --b"]);
    }

    #[test]
    fn a_block_the_workflow_would_refuse_is_refused() {
        let cases = [
            ("intro only".to_string(), "no <!--"),
            (format!("{OPEN}\n```\ngh a\ngh b\n```\n"), "never closed"),
            (readme("gh a\ngh b"), "no fenced code block"),
            (readme("```\ngh a\n```"), "has 1 command"),
            (readme("```\n# only a note\n```"), "has 0 command"),
            (readme("```powershell\ngh a\ngh b\n```"), "names 'powershell'"),
        ];
        for (text, want) in cases {
            let err = commands(&text).expect_err(&text);
            assert!(err.contains(want), "{text:?} gave {err:?}, wanted {want:?}");
        }
    }

    #[test]
    fn the_terminal_block_escapes_what_it_shows() {
        let html = terminal(&["cmd /c \"gh x > f.jsonl\"".into(), "gh <y>".into()]);
        assert_eq!(
            html,
            "<pre class=\"term\"><span class=\"p\">cmd</span> /c &quot;gh x &gt; f.jsonl&quot;\n\
             <span class=\"p\">gh</span> &lt;y&gt;</pre>"
        );
    }

    #[test]
    fn the_fingerprint_is_shown_as_windows_prints_it_and_nothing_else_passes() {
        let pin = "47b79ad3cfa53ef846cad03a59148f8c981d0b1196891e48b8c8d7982b10c148";
        assert_eq!(fingerprint(pin).unwrap(), pin.to_ascii_uppercase());
        for bad in ["", &pin[1..], &pin.to_ascii_uppercase(), &format!("{}g", &pin[1..]), &format!("{pin}0")] {
            assert!(fingerprint(bad).is_err(), "{bad:?} passed");
        }
    }

    /// The repository's own files, read the way the build reads them: the block the workflow runs
    /// must give the page at least the commands the page explains.
    #[test]
    fn the_repository_gives_the_page_its_commands_and_fingerprint() {
        let t = tokens(&crate::repo_root()).unwrap();
        assert!(t["verify_commands"].contains("attestation verify"), "{}", t["verify_commands"]);
        assert_eq!(t["codesign_sha256"].len(), 64);
    }
}
