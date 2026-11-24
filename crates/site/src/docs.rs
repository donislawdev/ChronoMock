//! The documentation section of the site.
//!
//! Every other page is a standalone article in twenty-two languages. The documentation is a set of
//! pages read together, written in fewer languages (`site/data/docs.json` says which), so it needs
//! three things the articles do not: a menu of every page, a list of the headings of the page being
//! read, and a way to the previous and the next page. All three are composed here from that one
//! file, so adding a page to the documentation is one line in it and the menu of every other page
//! follows.
//!
//! The headings list is built from the `id` of each `h2` in the page. A heading without one has no
//! address a reader can be sent to, so it stops the build instead of quietly missing from the list.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{esc, url_path, Page, Result, SiteConfig, ROOT_LANGUAGE};

/// The words the layout around a documentation page needs, in one language.
#[derive(Debug, Deserialize)]
pub struct DocsText {
    pub menu: String,
    pub menu_label: String,
    pub on_this_page: String,
    pub pager_label: String,
    pub previous: String,
    pub next: String,
    pub next_release: String,
    pub next_release_title: String,
}

/// A heading of the documentation menu and the pages under it, in reading order.
#[derive(Debug, Deserialize)]
pub struct DocsSection {
    pub id: String,
    pub title: BTreeMap<String, String>,
    pub pages: Vec<String>,
}

/// `site/data/docs.json`.
#[derive(Debug, Deserialize)]
pub struct DocsConfig {
    pub languages: Vec<String>,
    pub text: BTreeMap<String, DocsText>,
    pub sections: Vec<DocsSection>,
}

impl DocsConfig {
    /// Every documentation page, in the order a reader walks them with "next".
    pub fn order(&self) -> Vec<&str> {
        self.sections
            .iter()
            .flat_map(|s| s.pages.iter().map(String::as_str))
            .collect()
    }

    /// Whether the page belongs to the documentation.
    pub fn contains(&self, id: &str) -> bool {
        self.order().contains(&id)
    }

    /// Whether the page is rendered with the documentation layout in this language. A page of the
    /// documentation may also exist in a language the documentation is not written in, and there it
    /// is an ordinary page, because the menu would link to pages that language does not have.
    pub fn renders(&self, id: &str, lang: &str) -> bool {
        self.contains(id) && self.languages.iter().any(|l| l == lang)
    }

    /// The languages a page must exist in: the documentation's for one of its pages, every
    /// language of the site for the rest.
    pub fn required_languages<'a>(&'a self, cfg: &'a SiteConfig, id: &str) -> &'a [String] {
        if self.contains(id) {
            &self.languages
        } else {
            &cfg.languages
        }
    }
}

/// The rules `docs.json` has to keep before anything is written.
pub fn check(docs: &DocsConfig, cfg: &SiteConfig, pages: &[Page]) -> Result<()> {
    check_languages(docs, cfg)?;
    let mut seen = BTreeSet::new();
    for section in &docs.sections {
        if section.pages.is_empty() {
            return Err(format!("docs.json: section '{}' lists no pages", section.id));
        }
        for lang in &docs.languages {
            if !section.title.contains_key(lang) {
                return Err(format!("docs.json: section '{}' has no '{lang}' title", section.id));
            }
        }
        for id in &section.pages {
            if !seen.insert(id.as_str()) {
                return Err(format!("docs.json lists page '{id}' twice"));
            }
            check_page(docs, pages, id)?;
        }
    }
    Ok(())
}

fn check_languages(docs: &DocsConfig, cfg: &SiteConfig) -> Result<()> {
    if docs.languages.first().map(String::as_str) != Some(ROOT_LANGUAGE) {
        return Err(format!(
            "docs.json must list '{ROOT_LANGUAGE}' first - the documentation has to exist at the site root"
        ));
    }
    for lang in &docs.languages {
        if !cfg.languages.contains(lang) {
            return Err(format!("docs.json names language '{lang}', which site.json does not list"));
        }
        if !docs.text.contains_key(lang) {
            return Err(format!("docs.json has no 'text' for '{lang}'"));
        }
    }
    Ok(())
}

fn check_page(docs: &DocsConfig, pages: &[Page], id: &str) -> Result<()> {
    let Some(page) = pages.iter().find(|p| p.meta.id == id) else {
        return Err(format!("docs.json lists page '{id}', and there is no site/pages/{id}/"));
    };
    if !page.meta.indexable {
        return Err(format!("docs.json lists page '{id}', which is not indexable"));
    }
    for lang in &docs.languages {
        if !page.meta.languages.contains_key(lang) {
            return Err(format!(
                "page '{id}' is part of the documentation but has no '{lang}' version"
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- headings --

/// The value of one attribute in the inside of an opening tag.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    let marker = format!(" {name}=\"");
    let start = attrs.find(&marker)? + marker.len();
    let end = attrs[start..].find('"')?;
    Some(attrs[start..start + end].to_string())
}

/// Text with its tags taken out and its white space folded, for a heading shown elsewhere.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An anchor a reader can be sent to: lower-case letters, digits and hyphens, and not empty.
fn is_anchor(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The second-level headings of a documentation page, as (id, text) in document order.
pub fn headings(body: &str, whence: &str) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    let mut ids = BTreeSet::new();
    let mut rest = body;
    while let Some(i) = rest.find("<h2") {
        rest = &rest[i + 3..];
        let Some(close) = rest.find('>') else {
            return Err(format!("{whence}: an <h2 tag is never closed"));
        };
        let attrs = &rest[..close];
        let after = &rest[close + 1..];
        let Some(end) = after.find("</h2>") else {
            return Err(format!("{whence}: an <h2> has no </h2>"));
        };
        let text = strip_tags(&after[..end]);
        let id = attribute(attrs, "id").ok_or_else(|| {
            format!("{whence}: the heading '{text}' has no id, so the list of headings cannot link to it")
        })?;
        if !is_anchor(&id) {
            return Err(format!(
                "{whence}: the heading id '{id}' must be lower-case letters, digits and hyphens"
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(format!("{whence}: two headings have the id '{id}'"));
        }
        found.push((id, text));
        rest = &after[end..];
    }
    Ok(found)
}

// ----------------------------------------------------------------------- layout --

fn page_by_id<'p>(pages: &'p [Page], id: &str) -> Result<&'p Page> {
    pages
        .iter()
        .find(|p| p.meta.id == id)
        .ok_or_else(|| format!("docs.json lists page '{id}', which is not loaded"))
}

/// The address and the menu label of a documentation page in one language.
fn link_of(pages: &[Page], id: &str, lang: &str) -> Result<(String, String)> {
    let page = page_by_id(pages, id)?;
    let entry = page
        .meta
        .languages
        .get(lang)
        .ok_or_else(|| format!("page '{id}' has no '{lang}' version"))?;
    Ok((url_path(lang, &entry.slug), entry.link_text.clone()))
}

/// The menu of every documentation page, grouped under the section headings.
fn menu(docs: &DocsConfig, pages: &[Page], current: &str, lang: &str) -> Result<String> {
    let mut h = String::new();
    for section in &docs.sections {
        h.push_str(&format!(
            "<div class=\"docsec\">\n<p>{}</p>\n<ul>\n",
            esc(&section.title[lang])
        ));
        for id in &section.pages {
            let (href, label) = link_of(pages, id, lang)?;
            let here = if id == current { " aria-current=\"page\"" } else { "" };
            h.push_str(&format!("<li><a href=\"{href}\"{here}>{}</a></li>\n", esc(&label)));
        }
        h.push_str("</ul>\n</div>\n");
    }
    Ok(h)
}

/// The links to the previous and the next page in reading order. The first page has no previous
/// one and the last no next one, and the link is then left out rather than pointing at itself.
fn pager(docs: &DocsConfig, text: &DocsText, pages: &[Page], current: &str, lang: &str) -> Result<String> {
    let order = docs.order();
    let at = order
        .iter()
        .position(|id| *id == current)
        .ok_or_else(|| format!("page '{current}' is not in docs.json"))?;
    let mut h = format!("<nav class=\"pager\" aria-label=\"{}\">\n", esc(&text.pager_label));
    if let Some(prev) = at.checked_sub(1).and_then(|i| order.get(i)) {
        let (href, label) = link_of(pages, prev, lang)?;
        h.push_str(&format!(
            "<a class=\"prev\" href=\"{href}\" rel=\"prev\"><span>{}</span>{}</a>\n",
            esc(&text.previous),
            esc(&label)
        ));
    }
    if let Some(next) = order.get(at + 1) {
        let (href, label) = link_of(pages, next, lang)?;
        h.push_str(&format!(
            "<a class=\"next\" href=\"{href}\" rel=\"next\"><span>{}</span>{}</a>\n",
            esc(&text.next),
            esc(&label)
        ));
    }
    h.push_str("</nav>\n");
    Ok(h)
}

/// The list of the headings of the page being read.
fn toc(text: &DocsText, heads: &[(String, String)]) -> String {
    let mut h = format!(
        "<nav class=\"toc\" aria-label=\"{}\">\n<p>{}</p>\n<ul>\n",
        esc(&text.on_this_page),
        esc(&text.on_this_page)
    );
    for (id, title) in heads {
        // The title is the heading's own text with its tags removed. It was written as HTML, so
        // its entities are already in place and escaping it again would print them.
        h.push_str(&format!("<li><a href=\"#{id}\">{title}</a></li>\n"));
    }
    h.push_str("</ul>\n</nav>\n");
    h
}

/// A documentation page: the menu, the page itself with the way to its neighbours, and the list
/// of its headings. On a narrow screen the menu folds into a `<details>` above the page, and the
/// stylesheet shows one of the two copies at a time.
pub fn layout(docs: &DocsConfig, pages: &[Page], page: &Page, lang: &str, body: &str, whence: &str) -> Result<String> {
    let text = docs
        .text
        .get(lang)
        .ok_or_else(|| format!("docs.json has no 'text' for '{lang}'"))?;
    let id = &page.meta.id;
    let heads = headings(body, whence)?;
    let list = menu(docs, pages, id, lang)?;
    let (_, current) = link_of(pages, id, lang)?;

    // The folded menu names the page being read beside its own name, except on the page that has
    // the menu's name, where it would read the same word twice.
    let here = if current == text.menu {
        String::new()
    } else {
        format!(" <span>{}</span>", esc(&current))
    };
    let mut h = String::from("<div class=\"shell docs\">\n");
    h.push_str(&format!(
        "<details class=\"docmenu\">\n<summary>{}{here}</summary>\n{list}</details>\n",
        esc(&text.menu)
    ));
    h.push_str(&format!(
        "<nav class=\"docnav\" aria-label=\"{}\">\n{list}</nav>\n",
        esc(&text.menu_label)
    ));
    h.push_str("<article class=\"doc\">\n");
    h.push_str(body);
    h.push_str(&pager(docs, text, pages, id, lang)?);
    h.push_str("</article>\n");
    if !heads.is_empty() {
        h.push_str(&toc(text, &heads));
    }
    h.push_str("</div>\n");
    Ok(h)
}

// ----------------------------------------------------------------------- tokens --

/// The tokens a page in a documentation language may use, on top of the site's own.
///
/// * `next_release` marks something the latest release does not have yet. It is a token rather
///   than markup typed into the page, so the release check can find every one that is left.
/// * `docs_contents` is every documentation page with its description, for the first page of the
///   documentation, so that list cannot fall behind the menu.
pub fn tokens(
    docs: &DocsConfig,
    pages: &[Page],
    lang: &str,
    resolve: &dyn Fn(&str) -> Result<String>,
) -> Result<BTreeMap<String, String>> {
    let mut t = BTreeMap::new();
    let Some(text) = docs.text.get(lang) else {
        return Ok(t);
    };
    t.insert(
        "next_release".into(),
        format!(
            "<span class=\"badge\" title=\"{}\">{}</span>",
            esc(&text.next_release_title),
            esc(&text.next_release)
        ),
    );
    let mut contents = String::from("<div class=\"doclist\">\n");
    for section in &docs.sections {
        contents.push_str(&format!("<h3>{}</h3>\n<dl>\n", esc(&section.title[lang])));
        for id in &section.pages {
            let (href, label) = link_of(pages, id, lang)?;
            let description = resolve(&page_by_id(pages, id)?.meta.languages[lang].description)?;
            contents.push_str(&format!(
                "<dt><a href=\"{href}\">{}</a></dt>\n<dd>{}</dd>\n",
                esc(&label),
                esc(&description)
            ));
        }
        contents.push_str("</dl>\n");
    }
    contents.push_str("</div>");
    t.insert("docs_contents".into(), contents);
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_come_out_in_order_with_their_text_and_without_tags() {
        let body = r#"<h1>T</h1><h2 id="first">The <code>run</code> command</h2><p>x</p><h2 class="a" id="second-2">Two</h2>"#;
        let got = headings(body, "page").expect("headings");
        assert_eq!(
            got,
            vec![
                ("first".to_string(), "The run command".to_string()),
                ("second-2".to_string(), "Two".to_string())
            ]
        );
    }

    #[test]
    fn a_heading_without_an_id_stops_the_build_and_names_the_heading() {
        let err = headings("<h2>Exit codes</h2>", "site/pages/x/en.html").expect_err("must refuse");
        assert!(err.contains("site/pages/x/en.html") && err.contains("Exit codes"), "{err}");
    }

    #[test]
    fn two_headings_with_one_id_are_refused() {
        let err = headings(r#"<h2 id="a">A</h2><h2 id="a">B</h2>"#, "p").expect_err("must refuse");
        assert!(err.contains("'a'"), "{err}");
    }

    #[test]
    fn an_id_that_is_not_a_plain_anchor_is_refused() {
        for id in ["Upper", "with space", "under_score", ""] {
            let body = format!(r#"<h2 id="{id}">A</h2>"#);
            assert!(headings(&body, "p").is_err(), "'{id}' must be refused");
        }
    }

    #[test]
    fn an_unclosed_heading_is_an_error_not_a_truncated_list() {
        assert!(headings(r#"<h2 id="a">A"#, "p").is_err());
        assert!(headings(r#"<h2 id="a""#, "p").is_err());
    }

    fn site() -> SiteConfig {
        serde_json::from_str(
            r##"{"host":"https://h","cname":"h","product":"P","repo":"https://r","languages":["en","pl","de"],
                "theme_color":"#000","social_image":"/s.png","social_image_width":1,"social_image_height":1,
                "download_sizes":{}}"##,
        )
        .expect("site")
    }

    fn page(id: &str, langs: &[&str]) -> Page {
        let languages: Vec<String> = langs
            .iter()
            .map(|l| format!(r#""{l}":{{"slug":"{id}","link_text":"{id}","title":"t","description":"d"}}"#))
            .collect();
        let meta = serde_json::from_str(&format!(r#"{{"id":"{id}","languages":{{{}}}}}"#, languages.join(",")))
            .expect("meta");
        Page { meta, bodies: BTreeMap::new() }
    }

    fn docs(languages: &[&str], pages: &[&str]) -> DocsConfig {
        let text: Vec<String> = languages
            .iter()
            .map(|l| {
                format!(
                    r#""{l}":{{"menu":"m","menu_label":"m","on_this_page":"o","pager_label":"p","previous":"p","next":"n","next_release":"r","next_release_title":"r"}}"#
                )
            })
            .collect();
        let titles: Vec<String> = languages.iter().map(|l| format!(r#""{l}":"S""#)).collect();
        let ids: Vec<String> = pages.iter().map(|p| format!(r#""{p}""#)).collect();
        serde_json::from_str(&format!(
            r#"{{"languages":[{}],"text":{{{}}},"sections":[{{"id":"s","title":{{{}}},"pages":[{}]}}]}}"#,
            languages.iter().map(|l| format!(r#""{l}""#)).collect::<Vec<_>>().join(","),
            text.join(","),
            titles.join(","),
            ids.join(",")
        ))
        .expect("docs")
    }

    #[test]
    fn a_documentation_page_in_every_documentation_language_passes() {
        let pages = [page("a", &["en", "pl"]), page("b", &["en", "pl", "de"])];
        check(&docs(&["en", "pl"], &["a", "b"]), &site(), &pages).expect("a valid set");
    }

    #[test]
    fn a_documentation_page_missing_one_of_its_languages_is_refused() {
        let pages = [page("a", &["en"])];
        let err = check(&docs(&["en", "pl"], &["a"]), &site(), &pages).expect_err("must refuse");
        assert!(err.contains("'a'") && err.contains("'pl'"), "{err}");
    }

    #[test]
    fn a_page_listed_twice_or_not_there_is_refused() {
        let pages = [page("a", &["en", "pl"])];
        assert!(check(&docs(&["en", "pl"], &["a", "a"]), &site(), &pages).is_err());
        assert!(check(&docs(&["en", "pl"], &["missing"]), &site(), &pages).is_err());
    }

    #[test]
    fn documentation_languages_must_start_at_the_root_and_exist_on_the_site() {
        let pages = [page("a", &["en", "pl", "fr"])];
        assert!(check(&docs(&["pl", "en"], &["a"]), &site(), &pages).is_err());
        assert!(check(&docs(&["en", "fr"], &["a"]), &site(), &pages).is_err());
    }

    #[test]
    fn only_a_documentation_page_may_skip_a_language_of_the_site() {
        let d = docs(&["en", "pl"], &["a"]);
        let cfg = site();
        assert_eq!(d.required_languages(&cfg, "a"), ["en", "pl"]);
        assert_eq!(d.required_languages(&cfg, "other"), ["en", "pl", "de"]);
        assert!(d.renders("a", "pl") && !d.renders("a", "de") && !d.renders("other", "en"));
    }
}
