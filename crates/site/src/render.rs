//! Composing the pages and writing the site out.
//!
//! Lives in the library rather than in the binary so that the end-to-end guard in
//! `tests/` can build the real site and read what came out. A generator whose output
//! is only ever inspected by eye is a generator whose next change breaks a page
//! nobody opens.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::catalogue::{self, Catalogue, CALENDARS_PAGE};
use crate::docs::{self, DocsConfig};
use crate::reference::{self, CliData};
use crate::verify;
use crate::{
    check_download_sizes, count_json_files, esc, internal_links, link_target, load_i18n, load_pages,
    localized_tokens, output_path, parse_json, resolve_tokens, tokens, url_path, validate_languages,
    Page, Report, Result, SiteConfig, ROOT_LANGUAGE,
};

pub struct Ctx<'a> {
    pub cfg: &'a SiteConfig,
    pub i18n: &'a BTreeMap<String, BTreeMap<String, String>>,
    pub pages: &'a [Page],
    pub tokens: &'a BTreeMap<String, String>,
    pub docs: &'a DocsConfig,
    pub cli: &'a CliData,
    pub catalogue: &'a Catalogue,
}

impl Ctx<'_> {
    fn s(&self, lang: &str, key: &str) -> Result<&str> {
        self.i18n
            .get(lang)
            .and_then(|d| d.get(key))
            .map(String::as_str)
            .ok_or_else(|| format!("i18n/{lang}.json has no key '{key}'"))
    }
}

pub fn build(root: &Path, out: &Path) -> Result<Report> {
    let site_dir = root.join("site");
    let cfg: SiteConfig = parse_json(&site_dir.join("site.json"))?;

    if cfg.languages.first().map(String::as_str) != Some(ROOT_LANGUAGE) {
        return Err(format!(
            "site.json must list '{ROOT_LANGUAGE}' first - it owns the site root and x-default points at it"
        ));
    }

    let i18n = load_i18n(&site_dir, &cfg.languages)?;
    validate_languages(&cfg.languages, &i18n)?;
    check_download_sizes(&cfg)?;
    let pages = load_pages(&site_dir)?;

    let catalogue = crate::catalogue_root(root);
    let preset_count = count_json_files(&catalogue.join("presets"))?;
    let calendar_count = count_json_files(&catalogue.join("calendars"))?;
    let mut tok = tokens(&cfg, preset_count, calendar_count);
    // The one token made of markup: the 404 lists every language's home page, and which
    // languages exist is a fact of site.json, not of that page's text.
    tok.insert("language_homes".into(), language_homes(&cfg, &i18n)?);
    // The commands and the certificate fingerprint of the page on checking a download, from the files
    // the release workflow runs and checks against, so the page cannot show something it did not.
    tok.extend(verify::tokens(root)?);

    let docs_cfg: DocsConfig = parse_json(&site_dir.join("data").join("docs.json"))?;
    docs::check(&docs_cfg, &cfg, &pages)?;
    let cli: CliData = parse_json(&site_dir.join("data").join("cli.json"))?;
    reference::check(&cli, &docs_cfg.languages)?;
    let catalogue = catalogue::load(root)?;
    check_pages(&cfg, &docs_cfg, &pages)?;

    prepare_out(out)?;

    let ctx = Ctx {
        cfg: &cfg,
        i18n: &i18n,
        pages: &pages,
        tokens: &tok,
        docs: &docs_cfg,
        cli: &cli,
        catalogue: &catalogue,
    };

    let mut report = Report {
        languages: cfg.languages.len(),
        ..Report::default()
    };
    // (label for humans, page id, html). The id is carried separately because the
    // orphan check has to know which page a link came FROM, and parsing it back out
    // of the label would be one silent bug away from being wrong.
    let mut rendered: Vec<(String, String, String)> = Vec::new();

    for page in &pages {
        for lang in page.meta.languages.keys() {
            let html = render_page(&ctx, page, lang)?;

            // The 404 is the one address GitHub Pages looks for by name.
            let rel = if page.meta.id == "404" {
                PathBuf::from("404.html")
            } else {
                output_path(lang, &page.meta.languages[lang].slug)
            };

            write_file(&out.join(&rel), &html)?;
            rendered.push((
                format!("{}[{lang}]", page.meta.id),
                page.meta.id.clone(),
                html,
            ));
            report.pages_written += 1;
        }
    }

    write_sitemap(&ctx, out)?;
    write_file(&out.join("robots.txt"), &robots_txt(&cfg))?;
    // Without this file GitHub Pages drops the custom domain on the next deploy and
    // starts serving from the github.io address instead.
    write_file(&out.join("CNAME"), &format!("{}\n", cfg.cname))?;
    copy_assets(root, &site_dir, out)?;

    // Link check last, once everything that could satisfy a link exists on disk.
    for (whence, _, html) in &rendered {
        for link in internal_links(html) {
            if !out.join(link_target(&link)).exists() {
                report
                    .dangling_links
                    .entry(whence.clone())
                    .or_default()
                    .insert(link);
            }
        }
    }

    // ...and the same question asked backwards: which emitted page does nothing link
    // to? A dead link is loud - the reader lands on the 404 - while an orphan is
    // silent, indexed and unreachable.
    //
    // The 404 is excluded as a source on purpose. It deliberately links to every page,
    // so counting it would make the whole site look reachable no matter what the
    // navigation actually holds. That is not hypothetical: an audit of this very site
    // once came back green for exactly that reason.
    let mut linked: BTreeMap<PathBuf, BTreeSet<&str>> = BTreeMap::new();
    for (_, id, html) in &rendered {
        if id == "404" {
            continue;
        }
        for link in internal_links(html) {
            linked
                .entry(link_target(&link))
                .or_default()
                .insert(id.as_str());
        }
    }
    for page in &pages {
        if !page.meta.indexable {
            continue;
        }
        for (lang, entry) in &page.meta.languages {
            if !reached_from_elsewhere(&linked, &output_path(lang, &entry.slug), &page.meta.id) {
                report.orphans.insert(format!("{}[{lang}]", page.meta.id));
            }
        }
    }

    Ok(report)
}

/// The rules about pages that have to hold before anything is written.
///
/// Each of them is a way to publish something wrong without an error: a page missing in one
/// language, a footer column that does not exist, two pages claiming the same address, a
/// language directory shadowing a page.
fn check_pages(cfg: &SiteConfig, docs: &DocsConfig, pages: &[Page]) -> Result<()> {
    // A page that declares `pt-BR` where site.json says `pt-br` would otherwise be rendered
    // under a directory nobody asked for, or die later on a missing dictionary with an error
    // that names the wrong file.
    for page in pages {
        for lang in page.meta.languages.keys() {
            if !cfg.languages.contains(lang) {
                return Err(format!(
                    "page '{}' declares language '{lang}', which site.json does not list ({})",
                    page.meta.id,
                    cfg.languages.join(", ")
                ));
            }
        }
    }

    // Every indexable page must exist in every language. Catching it here rather than
    // at review time is the difference between a build error and a published page
    // that silently has no counterpart in one of the languages. A documentation page is
    // held to the languages of the documentation instead.
    for page in pages {
        if !page.meta.indexable {
            continue;
        }
        for lang in docs.required_languages(cfg, &page.meta.id) {
            if !page.meta.languages.contains_key(lang) {
                return Err(format!(
                    "page '{}' is indexable but has no '{lang}' version - add it, or mark the page indexable:false",
                    page.meta.id
                ));
            }
        }
    }

    // The header bar holds only a few addresses, so the footer map is what stands
    // between a page and being published with nothing linking to it. A page that names
    // no column, or misspells one, would drop out of that map without any error - the
    // way `/electron-chromium/` was once reachable from the sitemap alone. Both cases
    // stop the build instead.
    for page in pages {
        if !page.meta.indexable || page.meta.group.is_empty() {
            continue;
        }
        if !FOOTER_GROUPS.contains(&page.meta.group.as_str()) {
            return Err(format!(
                "page '{}' names footer group '{}', which does not exist - use one of: {}",
                page.meta.id,
                page.meta.group,
                FOOTER_GROUPS.join(", ")
            ));
        }
    }

    // Two pages with one slug in one language are one address, and the second silently
    // overwrites the first. Translators pick slugs by hand, twenty languages at a time -
    // 'download' and 'releases' both rendered as 'herunterladen' is a plausible slip.
    let mut addresses: BTreeMap<String, &str> = BTreeMap::new();
    for page in pages {
        for lang in page.meta.languages.keys() {
            let Some(address) = page.url_path(lang) else { continue };
            if page.meta.id == "404" {
                continue;
            }
            if let Some(other) = addresses.insert(address.clone(), &page.meta.id) {
                return Err(format!(
                    "pages '{other}' and '{}' both claim {address}",
                    page.meta.id
                ));
            }
        }
    }

    // `/de/` is a language directory. A page of the root language whose slug is `de` would
    // be the same address, and which one wins is the order the files were written in.
    for page in pages {
        let Some(entry) = page.meta.languages.get(ROOT_LANGUAGE) else { continue };
        if cfg.languages.iter().any(|l| l != ROOT_LANGUAGE && *l == entry.slug) {
            return Err(format!(
                "page '{}' has the slug '{}', which is also a language directory",
                page.meta.id, entry.slug
            ));
        }
    }
    Ok(())
}

/// A list item per language, each linking to that language's home page, in the language's own
/// name. For the 404, which is one document for every address on the site and so cannot know
/// which language the reader was in.
fn language_homes(
    cfg: &SiteConfig,
    i18n: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<String> {
    let mut out = String::new();
    for lang in &cfg.languages {
        let dict = &i18n[lang];
        let name = dict.get("language_name").ok_or_else(|| format!("i18n/{lang}.json has no key 'language_name'"))?;
        let tag = dict.get("html_lang").ok_or_else(|| format!("i18n/{lang}.json has no key 'html_lang'"))?;
        out.push_str(&format!(
            "<li><a href=\"{}\" hreflang=\"{}\" lang=\"{}\">{}</a></li>\n",
            url_path(lang, ""),
            esc(tag),
            esc(tag),
            esc(name)
        ));
    }
    Ok(out)
}

/// Whether some page OTHER than `whose` links to `address`.
///
/// The "other" is the whole subtlety, and it is worth a named function because getting
/// it wrong is silent. Every page carries a link to its own counterpart in the other
/// language, so a check that accepts a page as its own source makes each pair vouch for
/// itself and can never report anything. Measured: the first version of this did
/// exactly that and passed while `/electron-chromium/` was reachable from nothing else.
fn reached_from_elsewhere(
    linked: &BTreeMap<PathBuf, BTreeSet<&str>>,
    address: &Path,
    whose: &str,
) -> bool {
    linked
        .get(address)
        .is_some_and(|sources| sources.iter().any(|src| *src != whose))
}

/// Empty the output directory, but only one this tool made. A stray `--out` pointing
/// somewhere real would otherwise delete it.
///
/// The contents are removed rather than the directory itself, and the marker is left
/// in place throughout. Removing the whole directory deletes the marker first, so a
/// wipe that fails part way - a preview server holding it open on Windows is enough -
/// leaves a half-empty directory that no longer proves it is ours, and the next run
/// refuses to touch it. Measured the hard way.
fn prepare_out(out: &Path) -> Result<()> {
    let marker = out.join(".chrono-site");

    if out.exists() {
        if !marker.exists() {
            return Err(format!(
                "{} already exists and does not carry the .chrono-site marker - refusing to delete a directory this tool did not create",
                out.display()
            ));
        }
        // The one path in this crate that deletes, which is why the marker above stands where it
        // does. See the note at the top of lib.rs for why the rule does not reach this crate.
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let entries = fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", out.display()))?;
            let path = entry.path();
            if path == marker {
                continue;
            }
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            removed.map_err(|e| format!("{}: {e}", path.display()))?;
        }
    } else {
        fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    }

    fs::write(&marker, "Generated by crates/site. Safe to delete.\n")
        .map_err(|e| format!("{}: {e}", marker.display()))
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

// -------------------------------------------------------------------- rendering --

pub fn render_page(ctx: &Ctx, page: &Page, lang: &str) -> Result<String> {
    let meta = &page.meta.languages[lang];
    let whence = format!("site/pages/{}/{lang}.html", page.meta.id);

    // A size reads 2.4 on one page and 2,4 on the next, so each language gets its own table.
    let mut tokens = localized_tokens(ctx.tokens, ctx.s(lang, "decimal_separator")?);
    let base = tokens.clone();
    let resolve = |text: &str| resolve_tokens(text, &base, &whence);
    let docs_tokens = docs::tokens(ctx.docs, ctx.pages, lang, &resolve)?;
    if let Some(badge) = docs_tokens.get("next_release") {
        tokens.extend(reference::tokens(ctx.cli, lang, badge, &resolve)?);
    }
    let calendars_page = ctx
        .pages
        .iter()
        .find(|p| p.meta.id == CALENDARS_PAGE)
        .and_then(|p| p.url_path(lang));
    tokens.extend(catalogue::tokens(ctx.catalogue, lang, calendars_page.as_deref())?);
    tokens.extend(docs_tokens);
    let title = resolve_tokens(&meta.title, &tokens, &whence)?;
    let description = resolve_tokens(&meta.description, &tokens, &whence)?;
    let mut body = resolve_tokens(&page.bodies[lang], &tokens, &whence)?;
    if ctx.docs.renders(&page.meta.id, lang) {
        body = docs::layout(ctx.docs, ctx.pages, page, lang, &body, &whence)?;
    }

    let mut html = String::with_capacity(body.len() + 4096);
    html.push_str("<!doctype html>\n");
    html.push_str(&format!(
        "<html lang=\"{}\" dir=\"{}\">\n",
        ctx.s(lang, "html_lang")?,
        ctx.s(lang, "dir")?
    ));
    html.push_str(&render_head(ctx, page, lang, &title, &description)?);
    html.push_str("<body>\n");
    html.push_str(&format!(
        "<a class=\"skip\" href=\"#content\">{}</a>\n",
        esc(ctx.s(lang, "skip_to_content")?)
    ));
    html.push_str(&render_header(ctx, page, lang)?);
    html.push_str("<main id=\"content\">\n");
    html.push_str(&body);
    html.push_str("\n</main>\n");
    html.push_str(&render_footer(ctx, page, lang)?);
    html.push_str("</body>\n</html>\n");
    Ok(html)
}

fn render_head(
    ctx: &Ctx,
    page: &Page,
    lang: &str,
    title: &str,
    description: &str,
) -> Result<String> {
    let cfg = ctx.cfg;
    let mut h = String::from("<head>\n<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    h.push_str("<meta name=\"color-scheme\" content=\"dark\">\n");
    h.push_str(&format!(
        "<meta name=\"theme-color\" content=\"{}\">\n",
        esc(&cfg.theme_color)
    ));
    h.push_str(&format!("<title>{}</title>\n", esc(title)));
    h.push_str(&format!(
        "<meta name=\"description\" content=\"{}\">\n",
        esc(description)
    ));

    let url = page
        .url_path(lang)
        .map(|p| format!("{}{p}", cfg.host))
        .unwrap_or_else(|| cfg.host.clone());

    if page.meta.indexable {
        h.push_str(&format!("<link rel=\"canonical\" href=\"{}\">\n", esc(&url)));
        // Escaped like every other attribute in this function. These three interpolations were the
        // only ones that were not, sitting between neighbours that were - and a rule with three
        // exceptions is not a rule anyone can apply. Nothing in the config reaches them today, which
        // is exactly why the inconsistency would have survived until the first value that did.
        for l in &cfg.languages {
            if let Some(p) = page.url_path(l) {
                h.push_str(&format!(
                    "<link rel=\"alternate\" hreflang=\"{}\" href=\"{}\">\n",
                    esc(ctx.s(l, "html_lang")?),
                    esc(&format!("{}{p}", cfg.host))
                ));
            }
        }
        if let Some(p) = page.url_path(ROOT_LANGUAGE) {
            h.push_str(&format!(
                "<link rel=\"alternate\" hreflang=\"x-default\" href=\"{}\">\n",
                esc(&format!("{}{p}", cfg.host))
            ));
        }
    } else {
        // No canonical and no hreflang on a page that must not be indexed - both
        // would invite exactly the indexing this line refuses.
        h.push_str("<meta name=\"robots\" content=\"noindex\">\n");
    }

    let social = format!("{}{}", cfg.host, cfg.social_image);
    h.push_str("<meta property=\"og:type\" content=\"website\">\n");
    h.push_str(&format!(
        "<meta property=\"og:site_name\" content=\"{}\">\n",
        esc(&cfg.product)
    ));
    h.push_str(&format!(
        "<meta property=\"og:locale\" content=\"{}\">\n",
        ctx.s(lang, "og_locale")?
    ));
    // Only the languages this page exists in - a documentation page is not offered in the rest.
    for other in cfg.languages.iter().filter(|l| *l != lang && page.meta.languages.contains_key(*l)) {
        h.push_str(&format!(
            "<meta property=\"og:locale:alternate\" content=\"{}\">\n",
            ctx.s(other, "og_locale")?
        ));
    }
    h.push_str(&format!(
        "<meta property=\"og:title\" content=\"{}\">\n",
        esc(title)
    ));
    h.push_str(&format!(
        "<meta property=\"og:description\" content=\"{}\">\n",
        esc(description)
    ));
    // og:url is the page's own address, and the 404 has none: it is served for every address
    // that does not exist, so naming /404/ would tell a sharing site that this is where the
    // reader is, which is exactly what a canonical-less page must not claim.
    if page.meta.indexable {
        h.push_str(&format!(
            "<meta property=\"og:url\" content=\"{}\">\n",
            esc(&url)
        ));
    }
    h.push_str(&format!("<meta property=\"og:image\" content=\"{social}\">\n"));
    h.push_str(&format!(
        "<meta property=\"og:image:width\" content=\"{}\">\n",
        cfg.social_image_width
    ));
    h.push_str(&format!(
        "<meta property=\"og:image:height\" content=\"{}\">\n",
        cfg.social_image_height
    ));
    let social_alt = ctx.s(lang, "social_image_alt")?;
    h.push_str(&format!(
        "<meta property=\"og:image:alt\" content=\"{}\">\n",
        esc(social_alt)
    ));

    // summary_large_image, not summary: the preview is a wide picture, and the small
    // card crops it to a square nobody can read.
    h.push_str("<meta name=\"twitter:card\" content=\"summary_large_image\">\n");
    h.push_str(&format!(
        "<meta name=\"twitter:title\" content=\"{}\">\n",
        esc(title)
    ));
    h.push_str(&format!(
        "<meta name=\"twitter:description\" content=\"{}\">\n",
        esc(description)
    ));
    h.push_str(&format!(
        "<meta name=\"twitter:image\" content=\"{social}\">\n"
    ));
    h.push_str(&format!(
        "<meta name=\"twitter:image:alt\" content=\"{}\">\n",
        esc(social_alt)
    ));

    h.push_str("<link rel=\"icon\" href=\"/assets/icon.png\" type=\"image/png\">\n");
    h.push_str("<link rel=\"apple-touch-icon\" href=\"/assets/icon.png\">\n");
    h.push_str("<link rel=\"stylesheet\" href=\"/assets/style.css\">\n");

    if page.meta.id == "home" {
        h.push_str(&json_ld(ctx, lang, description)?);
    }

    h.push_str("</head>\n");
    Ok(h)
}

/// Structured data for the home page of each language. Kept to facts that are true
/// and checkable - there is no rating, no review count and no invented audience.
fn json_ld(ctx: &Ctx, lang: &str, description: &str) -> Result<String> {
    let cfg = ctx.cfg;
    let value = serde_json::json!({
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "name": cfg.product,
        "description": description,
        "url": format!("{}{}", cfg.host, url_path(lang, "")),
        "applicationCategory": "DeveloperApplication",
        "operatingSystem": "Windows 10, Windows 11",
        "softwareVersion": ctx.tokens.get("version"),
        "downloadUrl": ctx.tokens.get("releases"),
        "softwareHelp": cfg.repo,
        "license": "https://www.gnu.org/licenses/gpl-3.0.html",
        "isAccessibleForFree": true,
        "inLanguage": ctx.s(lang, "html_lang")?,
        "offers": { "@type": "Offer", "price": "0", "priceCurrency": "USD" },
        "author": { "@type": "Person", "name": "DonislawDev" }
    });
    Ok(format!(
        "<script type=\"application/ld+json\">\n{}\n</script>\n",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    ))
}

fn render_header(ctx: &Ctx, page: &Page, lang: &str) -> Result<String> {
    let cfg = ctx.cfg;
    let mut h = String::from("<header>\n<div class=\"shell bar\">\n");
    h.push_str(&format!(
        "<a class=\"brand\" href=\"{}\"><img class=\"bean\" src=\"/assets/bean.svg\" alt=\"\" width=\"20\" height=\"20\">{}</a>\n",
        url_path(lang, ""),
        esc(&cfg.product)
    ));
    h.push_str(&format!(
        "<nav aria-label=\"{}\">\n",
        esc(ctx.s(lang, "nav_label")?)
    ));

    for other in ctx.pages {
        if !other.meta.nav || !other.meta.indexable {
            continue;
        }
        let Some(entry) = other.meta.languages.get(lang) else {
            continue;
        };
        let current = if other.meta.id == page.meta.id {
            " aria-current=\"page\""
        } else {
            ""
        };
        h.push_str(&format!(
            "<a href=\"{}\"{current}>{}</a>\n",
            url_path(lang, &entry.slug),
            esc(&entry.link_text)
        ));
    }

    h.push_str("</nav>\n");
    h.push_str(&render_language_menu(ctx, page, lang)?);
    h.push_str("</div>\n</header>\n");
    Ok(h)
}

/// One link per language, to the SAME page in that language - or to that language's home when
/// the page has no counterpart there (the 404). The label and the tooltip come from the
/// dictionary of the language being linked TO, so the English page offers "Deutsch" rather than
/// "German" - a reader looking for the German version is looking for the German word. Reading
/// them from the current language instead reverses the pair, which is exactly what it did before
/// this was written down.
///
/// `lang` is set on each link as well as `hreflang`: the first tells a browser which font and
/// which glyph variant to draw the name with (the Han characters of 日本語 and 简体中文 are not
/// the same shapes), the second tells a crawler what the link leads to.
fn language_links(ctx: &Ctx, page: &Page, current: &str) -> Result<Vec<String>> {
    let mut links = Vec::with_capacity(ctx.cfg.languages.len());
    for other in &ctx.cfg.languages {
        // A page that is not indexed has no counterparts to offer - the 404 declares itself
        // in English alone, and its "English version" is /404/, which does not exist.
        let counterpart = if page.meta.indexable { page.url_path(other) } else { None };
        let target = counterpart.unwrap_or_else(|| url_path(other, ""));
        let tag = ctx.s(other, "html_lang")?;
        let here = if other == current { " aria-current=\"true\"" } else { "" };
        links.push(format!(
            "<a href=\"{target}\" hreflang=\"{}\" lang=\"{}\" title=\"{}\"{here}>{}</a>",
            esc(tag),
            esc(tag),
            esc(ctx.s(other, "language_title")?),
            esc(ctx.s(other, "language_name")?)
        ));
    }
    Ok(links)
}

/// The language picker in the header: a `<details>`, so it opens and closes without a line of
/// script, and its links are in the document for a crawler whether or not anybody opened it.
/// Twenty-two names in a row of links does not fit a header bar at any width.
fn render_language_menu(ctx: &Ctx, page: &Page, lang: &str) -> Result<String> {
    let label = esc(ctx.s(lang, "language_menu_label")?);
    let mut h = format!(
        "<details class=\"langmenu\">\n<summary title=\"{label}\"><span class=\"sr\">{label}</span>{}</summary>\n<ul>\n",
        esc(ctx.s(lang, "language_name")?)
    );
    for link in language_links(ctx, page, lang)? {
        h.push_str(&format!("<li>{link}</li>\n"));
    }
    h.push_str("</ul>\n</details>\n");
    Ok(h)
}

/// The footer columns, in the order they are shown. The membership of a page lives in
/// its own `page.json`, but the set of columns lives here so that a typo in one page
/// cannot invent a column that silently holds a single orphan: `build` rejects a group
/// that is not on this list.
///
/// Each id needs a `footer_group_<id>` heading in every dictionary, which the language
/// parity check then holds to both languages.
pub const FOOTER_GROUPS: [&str; 3] = ["start", "tools", "features"];

fn render_footer(ctx: &Ctx, page: &Page, lang: &str) -> Result<String> {
    let repo = &ctx.cfg.repo;
    let mut h = String::from("<footer>\n<div class=\"shell\">\n");

    // The site map. The header bar can only carry a handful of addresses before it
    // wraps, so every content page is listed here instead - this is what keeps a page
    // from being published and orphaned at the same time, and it is on every page.
    h.push_str(&format!(
        "<nav class=\"fmap\" aria-label=\"{}\">\n",
        esc(ctx.s(lang, "footer_nav_label")?)
    ));
    for group in FOOTER_GROUPS {
        let mut links = String::new();
        for page in ctx.pages {
            if page.meta.group != group || !page.meta.indexable {
                continue;
            }
            let Some(entry) = page.meta.languages.get(lang) else {
                continue;
            };
            links.push_str(&format!(
                "<a href=\"{}\">{}</a>\n",
                url_path(lang, &entry.slug),
                esc(&entry.link_text)
            ));
        }
        if links.is_empty() {
            continue;
        }
        h.push_str(&format!(
            "<div class=\"fcol\">\n<h2>{}</h2>\n{links}</div>\n",
            esc(ctx.s(lang, &format!("footer_group_{group}"))?)
        ));
    }
    h.push_str("</nav>\n");

    // Every language, visibly, on every page. The header menu is closed until somebody opens
    // it, and a visitor who scrolled to the end of a page is exactly the one looking for
    // another language - it is also a plain list of links to a crawler that renders nothing.
    h.push_str(&format!(
        "<nav class=\"flangs\" aria-label=\"{}\">\n<h2>{}</h2>\n",
        esc(ctx.s(lang, "language_menu_label")?),
        esc(ctx.s(lang, "language_menu_label")?)
    ));
    for link in language_links(ctx, page, lang)? {
        h.push_str(&link);
        h.push('\n');
    }
    h.push_str("</nav>\n");

    h.push_str(&format!(
        "<div class=\"fgrid\">\n<div>{}</div>\n<div class=\"flinks\">\
         <a href=\"{repo}\">{}</a>\
         <a href=\"{repo}/releases\">{}</a>\
         <a href=\"{repo}/blob/main/LICENSE\">{}</a>\
         </div>\n</div>\n</div>\n</footer>\n",
        esc(ctx.s(lang, "footer_summary")?),
        esc(ctx.s(lang, "footer_source")?),
        esc(ctx.s(lang, "footer_releases")?),
        esc(ctx.s(lang, "footer_license")?),
    ));
    Ok(h)
}

// --------------------------------------------------------------- site-wide files --

/// One `<url>` per page per language, and inside each the full set of its language versions.
///
/// The hreflang links in the page `<head>` already say this. Repeating it in the sitemap is
/// not redundant, though: it is the form a crawler can read for every address from one file
/// without fetching all of them, and the two are expected to agree - which they do, because
/// both come from `Page::url_path`. A page with twenty-two versions is twenty-two `<url>`
/// entries of twenty-three links each - the protocol's limit is fifty thousand addresses.
fn write_sitemap(ctx: &Ctx, out: &Path) -> Result<()> {
    let host = &ctx.cfg.host;
    let mut entries: BTreeMap<String, String> = BTreeMap::new();

    for page in ctx.pages {
        if !page.meta.indexable {
            continue;
        }
        let mut alternates = String::new();
        for lang in &ctx.cfg.languages {
            if let Some(p) = page.url_path(lang) {
                alternates.push_str(&format!(
                    "    <xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>\n",
                    esc(ctx.s(lang, "html_lang")?),
                    esc(&format!("{host}{p}"))
                ));
            }
        }
        if let Some(p) = page.url_path(ROOT_LANGUAGE) {
            alternates.push_str(&format!(
                "    <xhtml:link rel=\"alternate\" hreflang=\"x-default\" href=\"{}\"/>\n",
                esc(&format!("{host}{p}"))
            ));
        }
        for lang in page.meta.languages.keys() {
            if let Some(p) = page.url_path(lang) {
                entries.insert(format!("{host}{p}"), alternates.clone());
            }
        }
    }

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(
        "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    for (url, alternates) in &entries {
        xml.push_str(&format!(
            "  <url>\n    <loc>{}</loc>\n{alternates}  </url>\n",
            esc(url)
        ));
    }
    xml.push_str("</urlset>\n");
    write_file(&out.join("sitemap.xml"), &xml)
}

fn robots_txt(cfg: &SiteConfig) -> String {
    format!("User-agent: *\nAllow: /\n\nSitemap: {}/sitemap.xml\n", cfg.host)
}

fn copy_assets(root: &Path, site_dir: &Path, out: &Path) -> Result<()> {
    let src = site_dir.join("assets");
    let dst = out.join("assets");
    fs::create_dir_all(&dst).map_err(|e| format!("{}: {e}", dst.display()))?;

    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let entries = fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", src.display()))?;
        let path = entry.path();
        if path.is_file() {
            let target = dst.join(entry.file_name());
            fs::copy(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }

    // The wordmark uses the product's own icon rather than a second drawing of it, so
    // that changing the program's icon changes the site.
    let bean = root.join("assets").join("chrono-bean.svg");
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    fs::copy(&bean, dst.join("bean.svg")).map_err(|e| format!("{}: {e}", bean.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(pairs: &[(&'static str, &'static str)]) -> BTreeMap<PathBuf, BTreeSet<&'static str>> {
        let mut m: BTreeMap<PathBuf, BTreeSet<&str>> = BTreeMap::new();
        for (address, from) in pairs {
            m.entry(PathBuf::from(address)).or_default().insert(from);
        }
        m
    }

    #[test]
    fn a_page_linked_from_another_page_is_reachable() {
        let linked = sources(&[("faq/index.html", "home")]);
        assert!(reached_from_elsewhere(
            &linked,
            &PathBuf::from("faq/index.html"),
            "faq"
        ));
    }

    #[test]
    fn the_language_switch_does_not_make_a_page_reach_itself() {
        // Every page links to its own counterpart in the other language, so the Polish
        // FAQ is always linked "from faq". If that counted, no page could ever be
        // reported - which is what the first version of this check did.
        let linked = sources(&[("pl/faq/index.html", "faq")]);
        assert!(!reached_from_elsewhere(
            &linked,
            &PathBuf::from("pl/faq/index.html"),
            "faq"
        ));
    }

    #[test]
    fn a_page_nothing_points_at_is_not_reachable() {
        let linked = sources(&[("faq/index.html", "home")]);
        assert!(!reached_from_elsewhere(
            &linked,
            &PathBuf::from("electron-chromium/index.html"),
            "electron-chromium"
        ));
    }

    #[test]
    fn one_foreign_source_is_enough_even_among_self_links() {
        let linked = sources(&[
            ("pl/faq/index.html", "faq"),
            ("pl/faq/index.html", "download"),
        ]);
        assert!(reached_from_elsewhere(
            &linked,
            &PathBuf::from("pl/faq/index.html"),
            "faq"
        ));
    }
}
