//! Generator for the Chrono Mock website.
//!
//! The site is plain static HTML: no framework, no JavaScript, no webfonts. What it
//! does need is a way to keep three hundred pages consistent, because the parts that
//! rot are never the prose - they are the head elements, the navigation, the language
//! pairs and the numbers. So the content lives in fragments under `site/pages/` and
//! everything repeated is composed here.
//!
//! Three guards matter more than the rendering:
//!
//! * **Tokens.** A number that appears on a page is written as `{{channel_count}}`
//!   and resolved from the code. An unknown token is a build failure, not a literal
//!   printed to a visitor.
//! * **Language parity.** Every indexable page must exist in every language, and the
//!   dictionaries must carry the same keys. A half-translated page otherwise
//!   reaches production looking finished. The one exception is the documentation,
//!   whose pages must exist in every language `site/data/docs.json` names instead.
//! * **Internal links.** Every `/...` link must resolve to something actually
//!   emitted. GitHub Pages cannot redirect, so a dead internal link is permanent
//!   until someone notices it by hand.
//!
//! # Why the path-traversal rule is suppressed in this crate
//!
//! A web-framework rule reads every path built here as reachable by an attacker. This
//! crate is not reachable at all: it ships with nothing, no other crate depends on it,
//! and it is not in either distribution package. It runs twice, from `cargo run -p
//! chrono-site` in CI and on a maintainer's machine, and every path it opens is either
//! a directory named on its own command line or a name read out of `site/` in this
//! repository. There is no request, no upload and no visitor - the output is static
//! files a web server hands out afterwards, and that server is not this program.
//!
//! One place does delete, and it carries a guard the rule would not have asked for:
//! `prepare_out` refuses to empty a directory that does not already hold the
//! `.chrono-site` marker, so pointing `--out` at the wrong place stops rather than
//! wipes. The suppressions are per rule id, so a different rule still reports here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod catalogue;
pub mod docs;
pub mod reference;
pub mod render;

pub type Result<T> = std::result::Result<T, String>;

/// The repository root, derived from where this crate sits (`<root>/crates/site`)
/// rather than searched for. A search walking up for `Cargo.toml` finds the wrong
/// root when the tool runs from inside another checkout.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/site always has two ancestors")
        .to_path_buf()
}

/// Language whose pages own the site root. Everything else lives under `/<lang>/`.
/// This is the first entry of `languages` in `site.json` - kept as a named constant
/// because several places have to agree on it.
pub const ROOT_LANGUAGE: &str = "en";

// ---------------------------------------------------------------- configuration --

#[derive(Debug, Deserialize)]
pub struct SiteConfig {
    pub host: String,
    pub cname: String,
    pub product: String,
    pub repo: String,
    pub languages: Vec<String>,
    pub theme_color: String,
    pub social_image: String,
    pub social_image_width: u32,
    pub social_image_height: u32,
    /// The size of each download in MiB, written with a decimal point, keyed by the token name
    /// after `size_`. A release changes them, and this is the one place that has to follow.
    pub download_sizes: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct PageLang {
    pub slug: String,
    pub link_text: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct PageMeta {
    pub id: String,
    #[serde(default)]
    pub order: u32,
    /// Whether the page appears in the header bar. The bar holds the few addresses a
    /// first-time reader needs - everything else is reached from the footer map, which
    /// is on every page and has no width to run out of.
    #[serde(default = "yes")]
    pub nav: bool,
    /// Which footer column the page belongs to. Empty means the page is not listed
    /// there - only the home page, reached through the wordmark, and pages that are
    /// not indexable may leave it empty, and `build` refuses anything else.
    #[serde(default)]
    pub group: String,
    #[serde(default = "yes")]
    pub indexable: bool,
    pub languages: BTreeMap<String, PageLang>,
}

fn yes() -> bool {
    true
}

/// A page plus the body fragment for each language it declares.
#[derive(Debug)]
pub struct Page {
    pub meta: PageMeta,
    pub bodies: BTreeMap<String, String>,
}

impl Page {
    /// Address of this page in one language, as it appears in `href`, `canonical`
    /// and the sitemap. An empty slug means the language's own root.
    pub fn url_path(&self, lang: &str) -> Option<String> {
        let slug = &self.meta.languages.get(lang)?.slug;
        Some(url_path(lang, slug))
    }
}

/// `/`, `/download/`, `/pl/`, `/pl/pobieranie/`, `/zh-hans/download/` - one rule, written once.
pub fn url_path(lang: &str, slug: &str) -> String {
    match (lang == ROOT_LANGUAGE, slug.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{slug}/"),
        (false, true) => format!("/{lang}/"),
        (false, false) => format!("/{lang}/{slug}/"),
    }
}

/// Where that address is written on disk. GitHub Pages serves `foo/index.html` for
/// `/foo/`, and issues its own redirect from `/foo` - so directories, not `foo.html`.
pub fn output_path(lang: &str, slug: &str) -> PathBuf {
    let mut p = PathBuf::new();
    if lang != ROOT_LANGUAGE {
        p.push(lang);
    }
    if !slug.is_empty() {
        p.push(slug);
    }
    p.push("index.html");
    p
}

// ----------------------------------------------------------------------- tokens --

/// Values a page may interpolate. Everything drift-prone is here rather than typed
/// into the prose, so that changing the program changes the site.
pub fn tokens(cfg: &SiteConfig, preset_count: usize, calendar_count: usize) -> BTreeMap<String, String> {
    let mut t = BTreeMap::new();
    t.insert("host".into(), cfg.host.clone());
    t.insert("product".into(), cfg.product.clone());
    t.insert("repo".into(), cfg.repo.clone());
    t.insert("releases".into(), format!("{}/releases/latest", cfg.repo));
    t.insert("version".into(), env!("CARGO_PKG_VERSION").to_string());
    // The one that justifies this crate depending on chrono-ctl.
    t.insert("channel_count".into(), chrono_ctl::CHANNEL_COUNT.to_string());
    // How those channels split between the session clock and the audit's observed bucket, counted
    // from the same table, so "44, 30 of them shifted" cannot drift either.
    let observed = chrono_ctl::CHANNELS.iter().filter(|c| c.category.observed()).count();
    t.insert("observed_count".into(), observed.to_string());
    t.insert("substituted_count".into(), (chrono_ctl::CHANNEL_COUNT - observed).to_string());
    t.insert("preset_count".into(), preset_count.to_string());
    t.insert("calendar_count".into(), calendar_count.to_string());
    for (name, size) in &cfg.download_sizes {
        t.insert(format!("{SIZE_TOKEN_PREFIX}{name}"), size.clone());
    }
    t
}

/// The prefix of the tokens that carry a download size - the only tokens a language writes
/// differently, because they hold a decimal point.
pub const SIZE_TOKEN_PREFIX: &str = "size_";

/// Refuse a download size that is not a plain number. A size is printed on twenty-odd pages with
/// its unit typed beside it, so anything else in it - a unit, a tilde, a decimal comma - would be
/// printed twice or read wrongly in half the languages.
pub fn check_download_sizes(cfg: &SiteConfig) -> Result<()> {
    for (name, size) in &cfg.download_sizes {
        let mut parts = size.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        if !digits(whole) || fraction.is_some_and(|f| !digits(f)) || parts.next().is_some() {
            return Err(format!(
                "site.json: download size '{name}' is '{size}', and it must be a number such as 70 or 2.4"
            ));
        }
    }
    Ok(())
}

/// The token table one language renders with: the same values, with the decimal point of every
/// download size written the way that language writes it (`decimal_separator` in its dictionary).
pub fn localized_tokens(table: &BTreeMap<String, String>, decimal_separator: &str) -> BTreeMap<String, String> {
    table
        .iter()
        .map(|(name, value)| {
            let value = if name.starts_with(SIZE_TOKEN_PREFIX) {
                value.replace('.', decimal_separator)
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect()
}

/// Replace every `{{name}}`. An unknown name is an error naming the file, because the
/// alternative is a visitor reading two curly braces on a published page.
pub fn resolve_tokens(text: &str, table: &BTreeMap<String, String>, whence: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(format!("{whence}: an opening {{{{ is never closed"));
        };
        let name = after[..end].trim();
        match table.get(name) {
            Some(value) => out.push_str(value),
            None => {
                let known: Vec<&str> = table.keys().map(String::as_str).collect();
                return Err(format!(
                    "{whence}: unknown token {{{{{name}}}}}. Known tokens: {}",
                    known.join(", ")
                ));
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

// ------------------------------------------------------------------------ links --

/// Every internal `href` in a rendered document. External addresses, fragments and
/// `mailto:` are not ours to check.
pub fn internal_links(html: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = &rest[..end];
        rest = &rest[end..];
        if href.starts_with('/') {
            // Drop any fragment - `/faq/#anchor` is a link to `/faq/`.
            let path = href.split('#').next().unwrap_or(href);
            found.insert(path.to_string());
        }
    }
    found
}

/// Map an internal address to the file that has to exist for it to resolve.
pub fn link_target(link: &str) -> PathBuf {
    let trimmed = link.trim_start_matches('/');
    if link.ends_with('/') || trimmed.is_empty() {
        let mut p = PathBuf::from(trimmed);
        p.push("index.html");
        p
    } else {
        PathBuf::from(trimmed)
    }
}

// ------------------------------------------------------------------- html pieces --

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ------------------------------------------------------------------------ report --

#[derive(Debug, Default)]
pub struct Report {
    pub pages_written: usize,
    pub languages: usize,
    /// Internal links that point at something not emitted. Reported always, fatal
    /// only under `--strict`, so that a half-built site can still be looked at while
    /// never being publishable with a dead link in it.
    pub dangling_links: BTreeMap<String, BTreeSet<String>>,
    /// The mirror image: pages that were emitted and indexed, but that no other page
    /// links to. Published and unreachable at the same time - which is how
    /// `/electron-chromium/` once lived, discoverable only from the sitemap.
    pub orphans: BTreeSet<String>,
}

impl Report {
    pub fn dangling_count(&self) -> usize {
        self.dangling_links.values().map(BTreeSet::len).sum()
    }

    /// Everything `--strict` refuses to publish.
    pub fn problem_count(&self) -> usize {
        self.dangling_count() + self.orphans.len()
    }
}

// ------------------------------------------------------------------------- input --

pub fn read_to_string(path: &Path) -> Result<String> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Load the language dictionaries and check that they carry the same keys. A key
/// present in one file and missing from the other is the way a page ends up half
/// translated while looking complete.
pub fn load_i18n(site_dir: &Path, languages: &[String]) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut all: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for lang in languages {
        let path = site_dir.join("i18n").join(format!("{lang}.json"));
        let raw: BTreeMap<String, String> = parse_json(&path)?;
        // Keys starting with `//` are notes to the translator, not strings.
        let cleaned = raw.into_iter().filter(|(k, _)| !k.starts_with("//")).collect();
        all.insert(lang.clone(), cleaned);
    }

    let Some(first) = languages.first() else {
        return Err("site.json lists no languages".into());
    };
    let reference: BTreeSet<&String> = all[first].keys().collect();
    for lang in languages.iter().skip(1) {
        let here: BTreeSet<&String> = all[lang].keys().collect();
        let missing: Vec<&&String> = reference.difference(&here).collect();
        let extra: Vec<&&String> = here.difference(&reference).collect();
        if !missing.is_empty() || !extra.is_empty() {
            return Err(format!(
                "i18n/{lang}.json does not match i18n/{first}.json - missing: {missing:?}, unexpected: {extra:?}"
            ));
        }
    }
    Ok(all)
}

/// Whether `code` is usable as both a directory name and the lower-cased form of a language tag.
fn is_language_code(code: &str) -> bool {
    !code.is_empty()
        && !code.starts_with('-')
        && !code.ends_with('-')
        && code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Facts about each language that more than one place in the output relies on, held to
/// one another so that they cannot drift apart across twenty-odd dictionaries.
///
/// * The code in `site.json` is a directory name AND the thing hreflang is generated from.
///   If `pt-br` were paired with `html_lang` `pt-PT`, every page would announce one language
///   to search engines and live under the address of another, and nothing would fail.
/// * `dir` is the only thing that turns the layout around for Arabic - a typo there would
///   silently publish a left-to-right Arabic site.
/// * An `og_locale` that is not `ll_CC` is dropped by the crawlers that read it, without a
///   message.
/// * `decimal_separator` is written into every download size, so anything but a point or a
///   comma would put a stray character into a number on every page of that language.
pub fn validate_languages(
    languages: &[String],
    i18n: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for lang in languages {
        if !is_language_code(lang) {
            return Err(format!(
                "site.json: '{lang}' is not a usable language code - lower-case letters, digits and hyphens only"
            ));
        }
        if !seen.insert(lang.as_str()) {
            return Err(format!("site.json lists '{lang}' twice"));
        }
        let dict = &i18n[lang];
        let key = |name: &str| {
            dict.get(name)
                .map(String::as_str)
                .ok_or_else(|| format!("i18n/{lang}.json has no key '{name}'"))
        };

        let html_lang = key("html_lang")?;
        if html_lang.to_ascii_lowercase() != *lang {
            return Err(format!(
                "i18n/{lang}.json: html_lang is '{html_lang}', which is not '{lang}' in other letters - the address and the hreflang would name different languages"
            ));
        }
        let dir = key("dir")?;
        if dir != "ltr" && dir != "rtl" {
            return Err(format!("i18n/{lang}.json: dir is '{dir}', and it must be 'ltr' or 'rtl'"));
        }
        let og = key("og_locale")?;
        let shaped = og.len() == 5
            && og.as_bytes()[2] == b'_'
            && og[..2].chars().all(|c| c.is_ascii_lowercase())
            && og[3..].chars().all(|c| c.is_ascii_uppercase());
        if !shaped {
            return Err(format!("i18n/{lang}.json: og_locale '{og}' is not of the form ll_CC"));
        }
        let separator = key("decimal_separator")?;
        if separator != "." && separator != "," {
            return Err(format!(
                "i18n/{lang}.json: decimal_separator is '{separator}', and it must be '.' or ','"
            ));
        }
    }
    Ok(())
}

/// Read every page directory. Order is by `order` then id, so the navigation is a
/// property of the data rather than of the filesystem.
pub fn load_pages(site_dir: &Path) -> Result<Vec<Page>> {
    let dir = site_dir.join("pages");
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut pages = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let meta_path = entry.path().join("page.json");
        let meta: PageMeta = parse_json(&meta_path)?;

        let mut bodies = BTreeMap::new();
        for lang in meta.languages.keys() {
            let body_path = entry.path().join(format!("{lang}.html"));
            bodies.insert(lang.clone(), read_to_string(&body_path)?);
        }
        pages.push(Page { meta, bodies });
    }

    pages.sort_by(|a, b| a.meta.order.cmp(&b.meta.order).then(a.meta.id.cmp(&b.meta.id)));
    Ok(pages)
}

/// Count the catalogue files that ship with the program. Used for the tokens, so a
/// preset added to the repository shows up on the site without anyone editing prose.
pub fn count_json_files(dir: &Path) -> Result<usize> {
    Ok(json_files(dir)?.len())
}

/// The `.json` files of a catalogue folder, ordered by name so every machine builds the same
/// page.
pub fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().is_some_and(|e| e == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> BTreeMap<String, String> {
        let mut t = BTreeMap::new();
        t.insert("repo".to_string(), "https://example.test/repo".to_string());
        t.insert("channel_count".to_string(), "36".to_string());
        t
    }

    #[test]
    fn addresses_follow_one_rule_for_both_languages() {
        assert_eq!(url_path("en", ""), "/");
        assert_eq!(url_path("en", "download"), "/download/");
        assert_eq!(url_path("pl", ""), "/pl/");
        assert_eq!(url_path("pl", "pobieranie"), "/pl/pobieranie/");
    }

    #[test]
    fn addresses_of_hyphenated_languages_keep_the_hyphen() {
        assert_eq!(url_path("zh-hans", ""), "/zh-hans/");
        assert_eq!(url_path("pt-br", "baixar"), "/pt-br/baixar/");
        assert_eq!(
            output_path("zh-hant", "faq"),
            PathBuf::from("zh-hant").join("faq").join("index.html")
        );
    }

    fn dict(html_lang: &str, dir: &str, og: &str) -> BTreeMap<String, String> {
        [("html_lang", html_lang), ("dir", dir), ("og_locale", og), ("decimal_separator", ",")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn one(code: &str, d: BTreeMap<String, String>) -> Result<()> {
        let mut all = BTreeMap::new();
        all.insert(code.to_string(), d);
        validate_languages(&[code.to_string()], &all)
    }

    #[test]
    fn a_language_code_must_be_its_own_html_lang_in_lower_case() {
        assert!(one("pt-br", dict("pt-BR", "ltr", "pt_BR")).is_ok());
        assert!(one("zh-hans", dict("zh-Hans", "ltr", "zh_CN")).is_ok());
        let err = one("pt-br", dict("pt-PT", "ltr", "pt_BR")).unwrap_err();
        assert!(err.contains("html_lang"), "{err}");
    }

    #[test]
    fn direction_and_locale_shapes_are_checked_not_trusted() {
        assert!(one("ar", dict("ar", "rtl", "ar_AR")).is_ok());
        assert!(one("ar", dict("ar", "RTL", "ar_AR")).unwrap_err().contains("dir"));
        assert!(one("ar", dict("ar", "rtl", "ar")).unwrap_err().contains("og_locale"));
        assert!(one("ar", dict("ar", "rtl", "ar_ar")).unwrap_err().contains("og_locale"));
        let mut odd = dict("ar", "rtl", "ar_AR");
        odd.insert("decimal_separator".into(), "\u{66b}".into());
        assert!(one("ar", odd).unwrap_err().contains("decimal_separator"));
    }

    #[test]
    fn a_code_that_cannot_be_a_directory_is_refused() {
        assert!(one("PT", dict("pt", "ltr", "pt_PT")).unwrap_err().contains("language code"));
        assert!(one("../x", dict("x", "ltr", "xx_XX")).unwrap_err().contains("language code"));
    }

    #[test]
    fn addresses_map_to_directory_indexes() {
        assert_eq!(output_path("en", ""), PathBuf::from("index.html"));
        assert_eq!(output_path("en", "faq"), PathBuf::from("faq").join("index.html"));
        assert_eq!(output_path("pl", ""), PathBuf::from("pl").join("index.html"));
        assert_eq!(
            output_path("pl", "faq"),
            PathBuf::from("pl").join("faq").join("index.html")
        );
    }

    #[test]
    fn known_tokens_are_replaced() {
        let out = resolve_tokens("a {{channel_count}} b", &table(), "t").unwrap();
        assert_eq!(out, "a 36 b");
    }

    #[test]
    fn an_unknown_token_stops_the_build_rather_than_reaching_a_page() {
        let err = resolve_tokens("{{nope}}", &table(), "pages/x/en.html").unwrap_err();
        assert!(err.contains("pages/x/en.html"), "the error must name the file: {err}");
        assert!(err.contains("nope"));
    }

    #[test]
    fn an_unclosed_token_is_an_error_not_silent_output() {
        assert!(resolve_tokens("{{oops", &table(), "t").is_err());
    }

    #[test]
    fn text_without_tokens_survives_unchanged() {
        let text = "plain { not a token } text";
        assert_eq!(resolve_tokens(text, &table(), "t").unwrap(), text);
    }

    #[test]
    fn only_internal_links_are_collected() {
        // Two hashes, not one: the fragment link below contains the sequence that
        // would otherwise close a single-hash raw string.
        let html = r##"<a href="/download/">a</a><a href="https://x.test/">b</a>
                       <a href="#section">c</a><a href="/assets/style.css">d</a>"##;
        let links = internal_links(html);
        assert!(links.contains("/download/"));
        assert!(links.contains("/assets/style.css"));
        assert_eq!(links.len(), 2, "external links and fragments are not ours: {links:?}");
    }

    #[test]
    fn a_fragment_does_not_hide_the_page_it_points_at() {
        let links = internal_links(r##"<a href="/faq/#zones">x</a>"##);
        assert!(links.contains("/faq/"), "{links:?}");
    }

    #[test]
    fn links_map_to_the_file_that_must_exist() {
        assert_eq!(link_target("/"), PathBuf::from("index.html"));
        assert_eq!(link_target("/download/"), PathBuf::from("download").join("index.html"));
        assert_eq!(
            link_target("/assets/style.css"),
            PathBuf::from("assets").join("style.css")
        );
    }

    #[test]
    fn attributes_are_escaped() {
        assert_eq!(esc(r#"a "b" & <c>"#), "a &quot;b&quot; &amp; &lt;c&gt;");
    }

    #[test]
    fn the_channel_count_comes_from_the_code_not_from_prose() {
        let cfg_tokens = tokens(&test_config(), 14, 3);
        assert_eq!(
            cfg_tokens["channel_count"],
            chrono_ctl::CHANNEL_COUNT.to_string(),
            "the site must state the number the program actually implements"
        );
    }

    fn test_config() -> SiteConfig {
        SiteConfig {
            host: "https://example.test".into(),
            cname: "example.test".into(),
            product: "Chrono Mock".into(),
            repo: "https://github.com/x/y".into(),
            languages: vec!["en".into(), "pl".into()],
            theme_color: "#000000".into(),
            social_image: "/assets/social-preview.png".into(),
            social_image_width: 1280,
            social_image_height: 640,
            download_sizes: [("cli_zip", "2.4"), ("app_zip", "70")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn the_split_of_the_channels_comes_from_the_same_table() {
        let t = tokens(&test_config(), 14, 3);
        let number = |name: &str| t[name].parse::<usize>().expect("a count");
        let observed = chrono_ctl::CHANNELS.iter().filter(|c| c.category.observed()).count();
        assert_eq!(number("observed_count"), observed);
        assert_eq!(number("substituted_count") + number("observed_count"), chrono_ctl::CHANNEL_COUNT);
        assert!(number("substituted_count") > 0 && observed > 0, "both sides of the split are real");
    }

    #[test]
    fn a_size_takes_the_decimal_mark_of_its_language_and_nothing_else_does() {
        let t = tokens(&test_config(), 14, 3);
        let pl = localized_tokens(&t, ",");
        assert_eq!(pl["size_cli_zip"], "2,4");
        assert_eq!(pl["size_app_zip"], "70");
        assert_eq!(pl["host"], "https://example.test", "only sizes are localized");
        assert_eq!(localized_tokens(&t, ".")["size_cli_zip"], "2.4");
    }

    #[test]
    fn a_download_size_must_be_a_plain_number() {
        let mut cfg = test_config();
        assert!(check_download_sizes(&cfg).is_ok());
        for bad in ["2,4", "~70", "70 MB", "", "2.", ".4", "2.4.1"] {
            cfg.download_sizes.insert("cli_zip".into(), bad.into());
            let err = check_download_sizes(&cfg).expect_err(bad);
            assert!(err.contains("cli_zip"), "{err}");
        }
    }

    #[test]
    fn releases_token_points_at_the_latest_release() {
        let t = tokens(&test_config(), 14, 3);
        assert_eq!(t["releases"], "https://github.com/x/y/releases/latest");
    }
}
