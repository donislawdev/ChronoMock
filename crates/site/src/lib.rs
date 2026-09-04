//! Generator for the Chrono Mock website.
//!
//! The site is plain static HTML: no framework, no JavaScript, no webfonts. What it
//! does need is a way to keep twenty-four pages consistent, because the parts that
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
//!   two dictionaries must carry the same keys. A half-translated page otherwise
//!   reaches production looking finished.
//! * **Internal links.** Every `/...` link must resolve to something actually
//!   emitted. GitHub Pages cannot redirect, so a dead internal link is permanent
//!   until someone notices it by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    pub social_image_alt: String,
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
    #[serde(default = "yes")]
    pub nav: bool,
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

/// `/`, `/download/`, `/pl/`, `/pl/pobieranie/` - one rule, written once.
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
    t.insert("preset_count".into(), preset_count.to_string());
    t.insert("calendar_count".into(), calendar_count.to_string());
    t
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
}

impl Report {
    pub fn dangling_count(&self) -> usize {
        self.dangling_links.values().map(BTreeSet::len).sum()
    }
}

// ------------------------------------------------------------------------- input --

pub fn read_to_string(path: &Path) -> Result<String> {
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

/// Read every page directory. Order is by `order` then id, so the navigation is a
/// property of the data rather than of the filesystem.
pub fn load_pages(site_dir: &Path) -> Result<Vec<Page>> {
    let dir = site_dir.join("pages");
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
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut n = 0;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if entry.path().extension().is_some_and(|e| e == "json") {
            n += 1;
        }
    }
    Ok(n)
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
            social_image_alt: "alt".into(),
        }
    }

    #[test]
    fn releases_token_points_at_the_latest_release() {
        let t = tokens(&test_config(), 14, 3);
        assert_eq!(t["releases"], "https://github.com/x/y/releases/latest");
    }
}
