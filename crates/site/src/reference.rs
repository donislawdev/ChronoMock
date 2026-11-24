//! The command line reference, rendered from `site/data/cli.json`.
//!
//! The tables of commands, options and exit codes come from one data file rather than from prose
//! typed into each page, because the page they replaced drifted from the program within a release:
//! it never learned of `chrono calendars`. The options in that file are held to the program by
//! `crates/cli/tests/site_reference.rs`, which reads the usage lines of the program's own help.
//!
//! Text in the file is plain, with a pair of backticks around code. It is escaped here, so nothing
//! in the file can open a tag.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{esc, Result};

#[derive(Debug, Deserialize)]
pub struct Flag {
    pub flag: String,
    #[serde(default)]
    pub value: Option<String>,
    /// Not in the latest release yet: the page marks it, and the release check refuses to tag while
    /// one is left.
    #[serde(default)]
    pub unreleased: bool,
    #[serde(flatten)]
    pub text: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct FlagGroup {
    pub flags: Vec<Flag>,
    #[serde(flatten)]
    pub title: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CliCommand {
    pub name: String,
    pub synopsis: String,
    pub groups: Vec<FlagGroup>,
    #[serde(flatten)]
    pub text: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ExitCode {
    pub code: i32,
    #[serde(flatten)]
    pub text: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ExitTable {
    pub id: String,
    pub codes: Vec<ExitCode>,
}

/// `site/data/cli.json`.
#[derive(Debug, Deserialize)]
pub struct CliData {
    pub words: BTreeMap<String, Words>,
    pub commands: Vec<CliCommand>,
    pub exit_codes: Vec<ExitTable>,
}

/// The column headings and the link label, in one language.
#[derive(Debug, Deserialize)]
pub struct Words {
    pub command: String,
    pub option: String,
    pub does: String,
    pub code: String,
    pub meaning: String,
    pub link: String,
}

/// Every text of the file must exist in every language the documentation is written in. A
/// missing one would otherwise leave a blank cell on a published page.
pub fn check(data: &CliData, languages: &[String]) -> Result<()> {
    for lang in languages {
        let has = |text: &BTreeMap<String, String>, what: &str| {
            if text.contains_key(lang) {
                Ok(())
            } else {
                Err(format!("cli.json: {what} has no '{lang}' text"))
            }
        };
        if !data.words.contains_key(lang) {
            return Err(format!("cli.json: 'words' has no '{lang}'"));
        }
        for command in &data.commands {
            has(&command.text, &format!("command '{}'", command.name))?;
            for group in &command.groups {
                has(&group.title, &format!("a group of '{}'", command.name))?;
                for flag in &group.flags {
                    has(&flag.text, &format!("{} {}", command.name, flag.flag))?;
                }
            }
        }
        for table in &data.exit_codes {
            for code in &table.codes {
                has(&code.text, &format!("exit code {} of '{}'", code.code, table.id))?;
            }
        }
    }
    Ok(())
}

/// Escape a text and turn each pair of backticks into a `code` element. An odd number of
/// backticks is an error: half a pair would otherwise turn the rest of the cell into code.
pub fn inline(text: &str, whence: &str) -> Result<String> {
    let parts: Vec<&str> = text.split('`').collect();
    if parts.len().is_multiple_of(2) {
        return Err(format!("{whence}: a backtick is never closed in \"{text}\""));
    }
    let mut out = String::with_capacity(text.len() + 16);
    for (i, part) in parts.iter().enumerate() {
        if i % 2 == 1 {
            out.push_str(&format!("<code>{}</code>", esc(part)));
        } else {
            out.push_str(&esc(part));
        }
    }
    Ok(out)
}

/// The anchor of an option: the command and the option without its dashes, `run-at`.
pub fn anchor(command: &str, flag: &str) -> String {
    format!("{command}-{}", flag.trim_start_matches('-'))
}

fn text_in<'t>(text: &'t BTreeMap<String, String>, lang: &str, what: &str) -> Result<&'t str> {
    text.get(lang)
        .map(String::as_str)
        .ok_or_else(|| format!("cli.json: {what} has no '{lang}' text"))
}

fn options_table(
    command: &CliCommand,
    lang: &str,
    words: &Words,
    badge: &str,
    resolve: &dyn Fn(&str) -> Result<String>,
) -> Result<String> {
    let mut h = String::new();
    for group in &command.groups {
        let title = text_in(&group.title, lang, &format!("a group of '{}'", command.name))?;
        h.push_str(&format!(
            "<h3>{}</h3>\n<div class=\"wrap\"><table>\n<thead><tr><th>{}</th><th>{}</th></tr></thead>\n<tbody>\n",
            esc(title),
            esc(&words.option),
            esc(&words.does)
        ));
        for flag in &group.flags {
            let what = format!("{} {}", command.name, flag.flag);
            let id = anchor(&command.name, &flag.flag);
            let shown = match &flag.value {
                Some(value) => format!("{} {value}", flag.flag),
                None => flag.flag.clone(),
            };
            let mark = if flag.unreleased { badge } else { "" };
            let body = inline(&resolve(text_in(&flag.text, lang, &what)?)?, &what)?;
            h.push_str(&format!(
                "<tr id=\"{id}\"><td><code>{}</code>{mark}<a class=\"anchor\" href=\"#{id}\" aria-label=\"{} {}\">#</a></td><td>{body}</td></tr>\n",
                esc(&shown),
                esc(&words.link),
                esc(&flag.flag)
            ));
        }
        h.push_str("</tbody>\n</table></div>\n");
    }
    Ok(h)
}

fn commands_table(data: &CliData, lang: &str, words: &Words, resolve: &dyn Fn(&str) -> Result<String>) -> Result<String> {
    let mut h = format!(
        "<div class=\"wrap\"><table>\n<thead><tr><th>{}</th><th>{}</th></tr></thead>\n<tbody>\n",
        esc(&words.command),
        esc(&words.does)
    );
    for command in &data.commands {
        let what = format!("command '{}'", command.name);
        let body = inline(&resolve(text_in(&command.text, lang, &what)?)?, &what)?;
        h.push_str(&format!(
            "<tr id=\"command-{}\"><td><code>{}</code></td><td>{body}</td></tr>\n",
            command.name,
            esc(&command.synopsis)
        ));
    }
    h.push_str("</tbody>\n</table></div>");
    Ok(h)
}

fn exit_table(table: &ExitTable, lang: &str, words: &Words, resolve: &dyn Fn(&str) -> Result<String>) -> Result<String> {
    let mut h = format!(
        "<div class=\"wrap\"><table>\n<thead><tr><th>{}</th><th>{}</th></tr></thead>\n<tbody>\n",
        esc(&words.code),
        esc(&words.meaning)
    );
    for code in &table.codes {
        let what = format!("exit code {} of '{}'", code.code, table.id);
        let body = inline(&resolve(text_in(&code.text, lang, &what)?)?, &what)?;
        h.push_str(&format!(
            "<tr id=\"exit-{}-{}\"><td>{}</td><td>{body}</td></tr>\n",
            table.id, code.code, code.code
        ));
    }
    h.push_str("</tbody>\n</table></div>");
    Ok(h)
}

/// The tokens of the reference in one language: `cli_commands`, `cli_options_<command>` for every
/// command with options, and `exit_codes_<id>` for every table of exit codes. A language without
/// words in the file gets none, so a page in it that uses one fails on an unknown token.
pub fn tokens(
    data: &CliData,
    lang: &str,
    badge: &str,
    resolve: &dyn Fn(&str) -> Result<String>,
) -> Result<BTreeMap<String, String>> {
    let mut t = BTreeMap::new();
    let Some(words) = data.words.get(lang) else {
        return Ok(t);
    };
    t.insert("cli_commands".into(), commands_table(data, lang, words, resolve)?);
    for command in &data.commands {
        if !command.groups.is_empty() {
            t.insert(
                format!("cli_options_{}", command.name),
                options_table(command, lang, words, badge, resolve)?,
            );
        }
    }
    for table in &data.exit_codes {
        t.insert(format!("exit_codes_{}", table.id), exit_table(table, lang, words, resolve)?);
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_between_backticks_is_marked_and_everything_is_escaped() {
        let got = inline("Use `--at <moment>` & <b>", "t").expect("inline");
        assert_eq!(got, "Use <code>--at &lt;moment&gt;</code> &amp; &lt;b&gt;");
    }

    #[test]
    fn an_unclosed_backtick_is_an_error_naming_where() {
        let err = inline("the `--at flag", "run --at").expect_err("must refuse");
        assert!(err.contains("run --at"), "{err}");
    }

    #[test]
    fn an_anchor_is_the_command_and_the_option_without_dashes() {
        assert_eq!(anchor("run", "--no-gecko-sandbox"), "run-no-gecko-sandbox");
    }

    fn data(pl_text: bool) -> CliData {
        let pl = if pl_text { r#","pl":"b""# } else { "" };
        let words = r#"{"command":"c","option":"o","does":"d","code":"k","meaning":"m","link":"l"}"#;
        serde_json::from_str(&format!(
            r#"{{"words":{{"en":{words},"pl":{words}}},
                "commands":[{{"name":"run","synopsis":"s","en":"a","pl":"b","groups":[
                  {{"en":"g","pl":"g","flags":[{{"flag":"--at","value":"<m>","unreleased":true,"en":"a"{pl}}}]}}]}}],
                "exit_codes":[{{"id":"run","codes":[{{"code":0,"en":"a","pl":"b"}}]}}]}}"#
        ))
        .expect("data")
    }

    #[test]
    fn an_option_without_text_in_a_documentation_language_is_refused() {
        let langs = ["en".to_string(), "pl".to_string()];
        check(&data(true), &langs).expect("complete");
        let err = check(&data(false), &langs).expect_err("must refuse");
        assert!(err.contains("run --at") && err.contains("'pl'"), "{err}");
    }

    #[test]
    fn an_unreleased_option_carries_the_mark_and_a_released_one_does_not() {
        let resolve = |s: &str| Ok(s.to_string());
        let t = tokens(&data(true), "en", "<MARK>", &resolve).expect("tokens");
        let options = &t["cli_options_run"];
        assert!(options.contains(r#"<tr id="run-at"><td><code>--at &lt;m&gt;</code><MARK>"#), "{options}");
        let mut released = data(true);
        released.commands[0].groups[0].flags[0].unreleased = false;
        let t = tokens(&released, "en", "<MARK>", &resolve).expect("tokens");
        assert!(!t["cli_options_run"].contains("<MARK>"));
        assert!(t.contains_key("cli_commands") && t.contains_key("exit_codes_run"));
    }
}
