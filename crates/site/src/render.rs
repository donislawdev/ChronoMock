//! Composing the pages and writing the site out.
//!
//! Lives in the library rather than in the binary so that the end-to-end guard in
//! `tests/` can build the real site and read what came out. A generator whose output
//! is only ever inspected by eye is a generator whose next change breaks a page
//! nobody opens.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    count_json_files, esc, internal_links, link_target, load_i18n, load_pages, output_path,
    parse_json, resolve_tokens, tokens, url_path, Page, Report, Result, SiteConfig, ROOT_LANGUAGE,
};

pub struct Ctx<'a> {
    pub cfg: &'a SiteConfig,
    pub i18n: &'a BTreeMap<String, BTreeMap<String, String>>,
    pub pages: &'a [Page],
    pub tokens: &'a BTreeMap<String, String>,
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
    let pages = load_pages(&site_dir)?;

    let preset_count = count_json_files(&root.join("presets"))?;
    let calendar_count = count_json_files(&root.join("calendars"))?;
    let tok = tokens(&cfg, preset_count, calendar_count);

    // Every indexable page must exist in every language. Catching it here rather than
    // at review time is the difference between a build error and a published page
    // that silently has no Polish counterpart.
    for page in &pages {
        if !page.meta.indexable {
            continue;
        }
        for lang in &cfg.languages {
            if !page.meta.languages.contains_key(lang) {
                return Err(format!(
                    "page '{}' is indexable but has no '{lang}' version - add it, or mark the page indexable:false",
                    page.meta.id
                ));
            }
        }
    }

    prepare_out(out)?;

    let ctx = Ctx {
        cfg: &cfg,
        i18n: &i18n,
        pages: &pages,
        tokens: &tok,
    };

    let mut report = Report {
        languages: cfg.languages.len(),
        ..Report::default()
    };
    let mut rendered: Vec<(String, String)> = Vec::new();

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
            rendered.push((format!("{}[{lang}]", page.meta.id), html));
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
    for (whence, html) in &rendered {
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

    Ok(report)
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

    let title = resolve_tokens(&meta.title, ctx.tokens, &whence)?;
    let description = resolve_tokens(&meta.description, ctx.tokens, &whence)?;
    let body = resolve_tokens(&page.bodies[lang], ctx.tokens, &whence)?;

    let mut html = String::with_capacity(body.len() + 4096);
    html.push_str("<!doctype html>\n");
    html.push_str(&format!("<html lang=\"{}\">\n", ctx.s(lang, "html_lang")?));
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
    html.push_str(&render_footer(ctx, lang)?);
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
        for l in &cfg.languages {
            if let Some(p) = page.url_path(l) {
                h.push_str(&format!(
                    "<link rel=\"alternate\" hreflang=\"{l}\" href=\"{}{p}\">\n",
                    cfg.host
                ));
            }
        }
        if let Some(p) = page.url_path(ROOT_LANGUAGE) {
            h.push_str(&format!(
                "<link rel=\"alternate\" hreflang=\"x-default\" href=\"{}{p}\">\n",
                cfg.host
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
    for other in cfg.languages.iter().filter(|l| *l != lang) {
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
    h.push_str(&format!(
        "<meta property=\"og:url\" content=\"{}\">\n",
        esc(&url)
    ));
    h.push_str(&format!("<meta property=\"og:image\" content=\"{social}\">\n"));
    h.push_str(&format!(
        "<meta property=\"og:image:width\" content=\"{}\">\n",
        cfg.social_image_width
    ));
    h.push_str(&format!(
        "<meta property=\"og:image:height\" content=\"{}\">\n",
        cfg.social_image_height
    ));
    h.push_str(&format!(
        "<meta property=\"og:image:alt\" content=\"{}\">\n",
        esc(&cfg.social_image_alt)
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
        esc(&cfg.social_image_alt)
    ));

    h.push_str("<link rel=\"icon\" href=\"/assets/icon.png\" type=\"image/png\">\n");
    h.push_str("<link rel=\"apple-touch-icon\" href=\"/assets/icon.png\">\n");
    h.push_str("<link rel=\"stylesheet\" href=\"/assets/style.css\">\n");

    if page.meta.id == "home" {
        h.push_str(&json_ld(ctx, lang, description));
    }

    h.push_str("</head>\n");
    Ok(h)
}

/// Structured data for the home page of each language. Kept to facts that are true
/// and checkable - there is no rating, no review count and no invented audience.
fn json_ld(ctx: &Ctx, lang: &str, description: &str) -> String {
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
        "inLanguage": lang,
        "offers": { "@type": "Offer", "price": "0", "priceCurrency": "USD" },
        "author": { "@type": "Person", "name": "DonislawDev" }
    });
    format!(
        "<script type=\"application/ld+json\">\n{}\n</script>\n",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    )
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

    // The label and the tooltip come from the dictionary of the language being linked
    // TO, so the English page offers "Polski" rather than "Polish" - a reader looking
    // for the Polish version is looking for the Polish word. Reading them from the
    // current language instead reverses the pair, which is exactly what it did before
    // this was written down.
    for other in cfg.languages.iter().filter(|l| *l != lang) {
        let target = page.url_path(other).unwrap_or_else(|| url_path(other, ""));
        h.push_str(&format!(
            "<a class=\"lang\" href=\"{target}\" hreflang=\"{other}\" title=\"{}\">{}</a>\n",
            esc(ctx.s(other, "language_title")?),
            esc(ctx.s(other, "language_name")?)
        ));
    }

    h.push_str("</nav>\n</div>\n</header>\n");
    Ok(h)
}

fn render_footer(ctx: &Ctx, lang: &str) -> Result<String> {
    let repo = &ctx.cfg.repo;
    Ok(format!(
        "<footer>\n<div class=\"shell fgrid\">\n<div>{}</div>\n<div class=\"flinks\">\
         <a href=\"{repo}\">{}</a>\
         <a href=\"{repo}/releases\">{}</a>\
         <a href=\"{repo}/blob/main/LICENSE\">{}</a>\
         </div>\n</div>\n</footer>\n",
        esc(ctx.s(lang, "footer_summary")?),
        esc(ctx.s(lang, "footer_source")?),
        esc(ctx.s(lang, "footer_releases")?),
        esc(ctx.s(lang, "footer_license")?),
    ))
}

// --------------------------------------------------------------- site-wide files --

fn write_sitemap(ctx: &Ctx, out: &Path) -> Result<()> {
    let mut urls: BTreeSet<String> = BTreeSet::new();
    for page in ctx.pages {
        if !page.meta.indexable {
            continue;
        }
        for lang in page.meta.languages.keys() {
            if let Some(p) = page.url_path(lang) {
                urls.insert(format!("{}{p}", ctx.cfg.host));
            }
        }
    }

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for url in &urls {
        xml.push_str(&format!("  <url>\n    <loc>{url}</loc>\n  </url>\n"));
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
    fs::copy(&bean, dst.join("bean.svg")).map_err(|e| format!("{}: {e}", bean.display()))?;
    Ok(())
}
