//! End-to-end guard: build the real site and read what came out.
//!
//! The unit tests in the library cover the rules in isolation. This file covers the
//! thing those cannot - that the rules, the content and the configuration in this
//! repository still compose into pages that are correct. Every assertion here failed
//! at least once while the generator was being written.

use std::fs;
use std::path::{Path, PathBuf};

use std::collections::{BTreeMap, BTreeSet};

use chrono_site::docs::{headings, DocsConfig};
use chrono_site::{load_i18n, load_pages, output_path, parse_json, render, repo_root, url_path, Page, PageMeta, SiteConfig};

/// Build into a directory of this test's own, so tests running in parallel do not
/// wipe each other's output.
///
/// The directory is removed first rather than reused. A test that fails part way can
/// leave one without the marker the generator looks for, and every later run would
/// then fail on that residue instead of on the thing under test - which is a slow way
/// to debug the wrong problem.
fn built(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("chrono-site-test-{name}"));
    let _ = fs::remove_dir_all(&out);
    render::build(&repo_root(), &out).expect("the site in this repository must build");
    out
}

fn read(out: &Path, rel: &str) -> String {
    fs::read_to_string(out.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Walk every emitted .html file.
fn html_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("output directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            html_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "html") {
            into.push(path);
        }
    }
}

#[test]
fn the_english_page_offers_polish_and_the_polish_page_offers_english() {
    // The label and tooltip must come from the language being linked TO. Reading them
    // from the current language reverses the pair - the English page then offers
    // "English", which is both useless and how this shipped before the guard existed.
    let out = built("langlink");

    let en = read(&out, "index.html");
    assert!(
        en.contains(r#"<a href="/pl/" hreflang="pl" lang="pl" title="Ta strona po polsku">Polski</a>"#),
        "the English page must offer Polski, in Polish"
    );

    let pl = read(&out, "pl/index.html");
    assert!(
        pl.contains(r#"<a href="/" hreflang="en" lang="en" title="This page in English">English</a>"#),
        "the Polish page must offer English, in English"
    );
}

/// What the build was configured with, read from the same files it reads.
fn configured() -> (SiteConfig, BTreeMap<String, BTreeMap<String, String>>) {
    let site = repo_root().join("site");
    let cfg: SiteConfig = parse_json(&site.join("site.json")).expect("site.json");
    let i18n = load_i18n(&site, &cfg.languages).expect("dictionaries");
    (cfg, i18n)
}

/// Where a language's pages live on disk, relative to the output directory.
fn lang_dir(lang: &str) -> String {
    if lang == "en" {
        String::new()
    } else {
        format!("{lang}/")
    }
}

/// Every `<link rel="alternate" hreflang=".." href="..">` of a document, as (hreflang, href).
fn alternates(html: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let marker = r#"<link rel="alternate" hreflang=""#;
    let mut rest = html;
    while let Some(i) = rest.find(marker) {
        rest = &rest[i + marker.len()..];
        let Some(end) = rest.find('"') else { break };
        let tag = rest[..end].to_string();
        let Some(h) = rest.find(r#"href=""#) else { break };
        let after = &rest[h + 6..];
        let Some(close) = after.find('"') else { break };
        found.push((tag, after[..close].to_string()));
    }
    found
}

/// The `href` of the canonical link.
fn canonical(html: &str) -> Option<String> {
    let marker = r#"<link rel="canonical" href=""#;
    let i = html.find(marker)? + marker.len();
    let end = html[i..].find('"')?;
    Some(html[i..i + end].to_string())
}

/// The file an absolute address of this site is written to.
fn file_of(cfg: &SiteConfig, url: &str) -> String {
    let path = url.strip_prefix(&cfg.host).unwrap_or_else(|| panic!("{url} is not on {}", cfg.host));
    format!("{}index.html", path.trim_start_matches('/'))
}

#[test]
fn every_indexable_page_declares_both_languages_and_a_default() {
    let out = built("hreflang");
    let en = read(&out, "index.html");

    assert!(en.contains(r#"<link rel="canonical" href="https://chronomock.donislawdev.com/">"#));
    assert!(en.contains(r#"hreflang="en" href="https://chronomock.donislawdev.com/">"#));
    assert!(en.contains(r#"hreflang="pl" href="https://chronomock.donislawdev.com/pl/">"#));
    assert!(en.contains(r#"hreflang="x-default" href="https://chronomock.donislawdev.com/">"#));

    // The Polish page must point back at the same pair, not at itself alone. A
    // one-directional hreflang is ignored by search engines.
    let pl = read(&out, "pl/index.html");
    assert!(pl.contains(r#"<link rel="canonical" href="https://chronomock.donislawdev.com/pl/">"#));
    assert!(pl.contains(r#"hreflang="en" href="https://chronomock.donislawdev.com/">"#));
    assert!(pl.contains(r#"hreflang="pl" href="https://chronomock.donislawdev.com/pl/">"#));
}

#[test]
fn the_404_refuses_indexing_and_claims_no_canonical_address() {
    let out = built("notfound");
    let page = read(&out, "404.html");

    assert!(page.contains(r#"<meta name="robots" content="noindex">"#));
    assert!(
        !page.contains("rel=\"canonical\""),
        "a noindex page that also claims a canonical address invites the indexing it refuses"
    );
    assert!(!page.contains("hreflang=\"x-default\""));
}

#[test]
fn the_sitemap_lists_the_real_pages_and_leaves_out_the_404() {
    let out = built("sitemap");
    let xml = read(&out, "sitemap.xml");

    assert!(xml.contains("<loc>https://chronomock.donislawdev.com/</loc>"));
    assert!(xml.contains("<loc>https://chronomock.donislawdev.com/pl/</loc>"));
    assert!(
        !xml.contains("404"),
        "a page marked noindex must not be advertised in the sitemap"
    );
}

#[test]
fn the_custom_domain_file_matches_the_address_the_pages_claim() {
    // Losing this file, or letting it disagree with the canonical host, makes GitHub
    // Pages serve from the github.io address while every page still points here.
    let out = built("cname");
    let cname = read(&out, "CNAME");
    let home = read(&out, "index.html");

    let host = cname.trim();
    assert!(
        home.contains(&format!(r#"<link rel="canonical" href="https://{host}/">"#)),
        "CNAME says {host}, but the page canonicalises somewhere else"
    );
}

#[test]
fn no_page_ships_an_unresolved_token() {
    let out = built("tokens");
    let mut files = Vec::new();
    html_files(&out, &mut files);
    assert!(!files.is_empty(), "the build produced no pages at all");

    for file in files {
        let text = fs::read_to_string(&file).expect("read");
        assert!(
            !text.contains("{{"),
            "{} still contains an unresolved token - a visitor would read the braces",
            file.display()
        );
    }
}

#[test]
fn the_channel_count_on_the_page_is_the_one_the_program_implements() {
    // The whole reason this crate depends on chrono-ctl. If the code grows a channel
    // and the site keeps saying the old number, this fails instead of misinforming.
    let out = built("channels");
    let home = read(&out, "index.html");
    let expected = chrono_ctl::CHANNEL_COUNT.to_string();

    assert!(
        home.contains(&format!("{expected} time channels")),
        "the home page must state {expected} channels"
    );
}

#[test]
fn a_download_size_is_written_with_the_decimal_mark_of_its_language() {
    // site.json holds 2.4 and the Polish page must print 2,4 - read in half of these languages, a point
    // there is a thousands separator. The rule is a unit test in the library, and this is the wiring:
    // a render that used the shared table instead of the language's own printed the point everywhere.
    let out = built("sizes");
    let (cfg, i18n) = configured();
    let meta: PageMeta =
        parse_json(&repo_root().join("site").join("pages").join("download").join("page.json")).expect("download page");
    assert!(!cfg.download_sizes.is_empty(), "site.json lists no download sizes");
    for lang in ["en", "pl"] {
        let file = output_path(lang, &meta.languages[lang].slug);
        let page = fs::read_to_string(out.join(&file)).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for (name, size) in &cfg.download_sizes {
            let written = size.replace('.', &i18n[lang]["decimal_separator"]);
            assert!(
                page.contains(&format!("~{written} MB")),
                "{} must state {name} as ~{written} MB",
                file.display()
            );
        }
    }
}

#[test]
fn the_link_preview_picture_states_the_channel_count_the_program_implements() {
    // The one number no token reaches: the picture is drawn in SVG, turned into the PNG by hand and
    // uploaded to the repository settings by hand, so it kept saying 36 long after the program had
    // grown past it. This reads the SVG, not the PNG - the PNG stays a manual step, and the failure
    // message is where that step is written down.
    let svg = fs::read_to_string(repo_root().join("site").join("assets").join("social-preview.svg"))
        .expect("site/assets/social-preview.svg");
    let expected = format!(">{} time channels", chrono_ctl::CHANNEL_COUNT);
    assert!(
        svg.contains(&expected),
        "site/assets/social-preview.svg must say '{}' - redraw it, rebuild the PNG with the magick line at \
         its top, and upload the PNG in the repository settings (General, Social preview)",
        &expected[1..]
    );
}

#[test]
fn every_page_has_exactly_one_title_and_one_description() {
    let out = built("headuniq");
    let mut files = Vec::new();
    html_files(&out, &mut files);

    for file in files {
        let text = fs::read_to_string(&file).expect("read");
        assert_eq!(
            text.matches("<title>").count(),
            1,
            "{} must carry exactly one title",
            file.display()
        );
        assert_eq!(
            text.matches(r#"<meta name="description""#).count(),
            1,
            "{} must carry exactly one description",
            file.display()
        );
    }
}

#[test]
fn a_second_build_clears_what_the_first_one_left() {
    // Deliberately a weak property - it passes whether the directory is emptied or
    // replaced wholesale. The invariant that actually matters is the next test.
    let out = built("rebuild");
    let stale = out.join("gone-in-the-next-build.html");
    fs::write(&stale, "old").expect("write");

    render::build(&repo_root(), &out).expect("a second build must be allowed");

    assert!(!stale.exists(), "the previous build's leftovers must be cleared");
    assert!(out.join("index.html").exists(), "and it must be rebuilt");
}

// Windows-only, because the lever that reliably blocks a deletion is. The generator
// itself builds and runs on Linux, where this failure mode would need a different one.
#[cfg(windows)]
#[test]
fn a_clean_that_cannot_finish_leaves_the_directory_still_claimable() {
    // Removing the whole output directory deletes the marker first, so a clean that
    // fails part way - a preview server holding it open was enough - leaves a
    // half-empty directory that no longer proves it is ours, and every later run then
    // refuses to touch it. Emptying the directory while keeping the marker is what
    // makes the failure recoverable.
    //
    // Marking the file read-only is NOT enough - measured: std's remove_file deletes it
    // anyway. What does block a deletion on Windows is an open handle that shares
    // nothing, which is the shape of the real case (a preview server holding the
    // directory it serves).
    use std::os::windows::fs::OpenOptionsExt;

    let out = built("failedclean");
    let stubborn = out.join("locked.bin");
    fs::write(&stubborn, b"x").expect("write");

    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&stubborn)
        .expect("open with no sharing");

    let err = render::build(&repo_root(), &out).expect_err("the clean cannot finish");
    assert!(err.contains("locked.bin"), "the error must name the file: {err}");
    assert!(
        out.join(".chrono-site").exists(),
        "the marker must survive a failed clean, or the next run locks itself out"
    );

    drop(held);
    fs::remove_file(&stubborn).expect("remove");
}

#[test]
fn the_output_directory_is_not_deleted_unless_this_tool_made_it() {
    // --out is a path from the command line. Pointed at something real, an
    // unconditional wipe would delete it.
    let out = std::env::temp_dir().join("chrono-site-test-guard");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).expect("create");
    fs::write(out.join("precious.txt"), "not ours").expect("write");

    let err = render::build(&repo_root(), &out).expect_err("must refuse");
    assert!(err.contains("refusing to delete"), "{err}");
    assert!(
        out.join("precious.txt").exists(),
        "the refusal must leave the directory untouched"
    );

    let _ = fs::remove_dir_all(&out);
}

// ------------------------------------------------------------- every language --
//
// The tests below are written against whatever site.json lists, not against English and
// Polish, so a language added there is held to the same rules the day it is added.

#[test]
fn every_language_page_announces_the_whole_set_and_every_member_announces_it_back() {
    // hreflang is honoured only when it is reciprocal: a page that names its siblings, whose
    // siblings do not name it back, is ignored. With twenty-two languages the number of
    // pairs is what makes a one-way link easy to ship - so check every page against every
    // page it names, rather than a sample.
    let out = built("hreflang-all");
    let (cfg, i18n) = configured();
    let mut checked = 0;
    // Which languages each address is built in, from the same page files the build read. A
    // documentation page exists in fewer languages than the rest, and must name exactly those.
    let mut built_in: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for page in load_pages(&repo_root().join("site")).expect("pages") {
        for (lang, entry) in &page.meta.languages {
            let tags = page.meta.languages.keys().map(|l| i18n[l]["html_lang"].clone()).collect();
            built_in.insert(format!("{}{}", cfg.host, url_path(lang, &entry.slug)), tags);
        }
    }

    for lang in &cfg.languages {
        let mut files = Vec::new();
        html_files(&out.join(lang_dir(lang)), &mut files);
        for file in files {
            let rel = file.strip_prefix(&out).expect("under out").to_string_lossy().replace('\\', "/");
            // Other languages' directories live under the root language's - only look at this
            // language's own pages.
            if lang == "en" && cfg.languages.iter().any(|l| l != "en" && rel.starts_with(&format!("{l}/"))) {
                continue;
            }
            let html = fs::read_to_string(&file).expect("read");
            let Some(own) = canonical(&html) else { continue }; // the 404 claims none
            assert_eq!(file_of(&cfg, &own), rel, "{rel}: the canonical address is not this file");

            let set = alternates(&html);
            let tags: Vec<&str> = set.iter().map(|(t, _)| t.as_str()).collect();
            let expected = &built_in[&own];
            for l in &cfg.languages {
                let tag = &i18n[l]["html_lang"];
                let want = usize::from(expected.contains(tag));
                assert_eq!(
                    tags.iter().filter(|t| *t == tag).count(),
                    want,
                    "{rel}: hreflang '{tag}' must appear {want} time(s), got {tags:?}"
                );
            }
            assert_eq!(
                tags.iter().filter(|t| **t == "x-default").count(),
                1,
                "{rel}: exactly one x-default"
            );
            assert!(
                set.iter().any(|(t, u)| *t == i18n[lang]["html_lang"] && *u == own),
                "{rel}: the page must list itself under its own language"
            );

            for (tag, url) in &set {
                let sibling = fs::read_to_string(out.join(file_of(&cfg, url)))
                    .unwrap_or_else(|e| panic!("{rel} names {url} ({tag}), which was not built: {e}"));
                assert_eq!(
                    alternates(&sibling),
                    set,
                    "{rel} and {url} must announce the same set of alternates"
                );
            }
            checked += 1;
        }
    }
    assert!(checked >= cfg.languages.len(), "only {checked} pages were looked at");
}

#[test]
fn the_sitemap_carries_every_page_in_every_language_with_its_alternates() {
    let out = built("sitemap-all");
    let (cfg, i18n) = configured();
    let xml = read(&out, "sitemap.xml");

    assert!(xml.contains(r#"xmlns:xhtml="http://www.w3.org/1999/xhtml""#));

    // Every canonical address the pages claim is a <loc>, and nothing else is.
    let mut canon = BTreeSet::new();
    let mut files = Vec::new();
    html_files(&out, &mut files);
    for file in files {
        if let Some(c) = canonical(&fs::read_to_string(&file).expect("read")) {
            canon.insert(c);
        }
    }
    let locs: BTreeSet<String> = xml
        .split("<loc>")
        .skip(1)
        .map(|chunk| chunk.split("</loc>").next().expect("closed").to_string())
        .collect();
    assert_eq!(locs, canon, "the sitemap and the pages must agree on which addresses exist");

    // ...and each entry lists the whole set, the same one the page's own head lists.
    let home = format!("{}/", cfg.host);
    let entry = xml.split("<url>").find(|e| e.contains(&format!("<loc>{home}</loc>"))).expect("home entry");
    for l in &cfg.languages {
        let tag = &i18n[l]["html_lang"];
        let url = format!("{}{}", cfg.host, if l == "en" { "/".to_string() } else { format!("/{l}/") });
        assert!(
            entry.contains(&format!(r#"<xhtml:link rel="alternate" hreflang="{tag}" href="{url}"/>"#)),
            "the home entry must list {tag} -> {url}"
        );
    }
    assert!(entry.contains(&format!(r#"hreflang="x-default" href="{home}""#)));
    assert!(!xml.contains("404"), "a noindex page must not be advertised");
}

#[test]
fn every_page_says_which_way_it_reads() {
    let out = built("dir");
    let (cfg, i18n) = configured();
    for lang in &cfg.languages {
        let home = read(&out, &format!("{}index.html", lang_dir(lang)));
        let expected = format!(
            r#"<html lang="{}" dir="{}">"#,
            i18n[lang]["html_lang"], i18n[lang]["dir"]
        );
        assert!(home.contains(&expected), "{lang}: expected {expected}");
    }
}

#[test]
fn the_language_menu_and_the_footer_each_list_every_language_once() {
    let out = built("menu");
    let (cfg, i18n) = configured();
    for lang in &cfg.languages {
        let home = read(&out, &format!("{}index.html", lang_dir(lang)));
        let menu = home.split(r#"<details class="langmenu">"#).nth(1).expect("a menu").split("</details>").next().unwrap();
        let foot = home.split(r#"<nav class="flangs""#).nth(1).expect("a footer list").split("</nav>").next().unwrap();
        for region in [menu, foot] {
            assert_eq!(region.matches("<a href=").count(), cfg.languages.len(), "{lang}: one link per language");
            for l in &cfg.languages {
                // Each language is named in its own words, whichever page it is offered from.
                let name = &i18n[l]["language_name"];
                assert!(region.contains(&format!(">{name}</a>")), "{lang}: the list must name {l} as '{name}'");
            }
            assert_eq!(region.matches("aria-current").count(), 1, "{lang}: exactly one current language");
        }
    }
}

/// The language an internal address belongs to: the first path segment when it is a language of
/// the site, the root language otherwise.
fn language_of<'l>(link: &str, languages: &'l [String]) -> &'l str {
    let first = link.trim_start_matches('/').split('/').next().unwrap_or_default();
    languages
        .iter()
        .find(|l| l.as_str() != "en" && l.as_str() == first)
        .or_else(|| languages.iter().find(|l| l.as_str() == "en"))
        .map(String::as_str)
        .expect("en is a language of the site")
}

/// The document with every link taken out that says, with `hreflang`, which language it leads to,
/// when that is the language of the address. Such a link leaves the language on purpose - the
/// short pages that lead to the English documentation - and says so to the reader's browser. A
/// link whose hreflang names another language than its address is left in, so it still fails.
fn without_declared_language(html: &str, languages: &[String]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("<a ") {
        out.push_str(&rest[..i]);
        let tag_end = rest[i..].find('>').map_or(rest.len(), |e| i + e + 1);
        let tag = &rest[i..tag_end];
        let declared = tag.split("hreflang=\"").nth(1).and_then(|s| s.split('"').next());
        let href = tag.split("href=\"").nth(1).and_then(|s| s.split('"').next());
        match (declared, href) {
            // hreflang is the tag (pt-BR), the address uses the code (pt-br).
            (Some(lang), Some(href)) if href.starts_with('/') && language_of(href, languages).eq_ignore_ascii_case(lang) => {}
            _ => out.push_str(tag),
        }
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn a_page_links_only_within_its_own_language() {
    // A German page that links to the English FAQ strands the reader in another language
    // after one click, and nothing else notices: the link resolves. Hand-written addresses in
    // fragments are exactly where this slips in - the Polish ones and the German ones are
    // different strings that look alike.
    //
    // The language pickers are the one place that leaves a language on purpose, and the 404 is
    // one document for every language, so both are left out.
    let out = built("ownlang");
    let (cfg, _) = configured();
    for lang in &cfg.languages {
        let mut files = Vec::new();
        html_files(&out.join(lang_dir(lang)), &mut files);
        for file in files {
            let rel = file.strip_prefix(&out).expect("under out").to_string_lossy().replace('\\', "/");
            let other_lang_dir = cfg.languages.iter().any(|l| l != "en" && l != lang && rel.starts_with(&format!("{l}/")));
            if rel == "404.html" || other_lang_dir {
                continue;
            }
            let html = fs::read_to_string(&file).expect("read");
            let mut body = html.clone();
            for (open, close) in [(r#"<details class="langmenu">"#, "</details>"), (r#"<nav class="flangs""#, "</nav>")] {
                if let Some(i) = body.find(open) {
                    let j = body[i..].find(close).expect("closed") + i + close.len();
                    body.replace_range(i..j, "");
                }
            }
            for link in chrono_site::internal_links(&without_declared_language(&body, &cfg.languages)) {
                let fine = link.starts_with("/assets/")
                    || if lang == "en" {
                        !cfg.languages.iter().any(|l| l != "en" && link.starts_with(&format!("/{l}/")))
                    } else {
                        link == format!("/{lang}/") || link.starts_with(&format!("/{lang}/"))
                    };
                assert!(fine, "{rel} links to {link}, which is not a page in {lang}");
            }
        }
    }
}

#[test]
fn the_link_preview_description_is_in_the_language_of_the_page() {
    let out = built("socialalt");
    let (cfg, i18n) = configured();
    for lang in &cfg.languages {
        let home = read(&out, &format!("{}index.html", lang_dir(lang)));
        let alt = &i18n[lang]["social_image_alt"];
        assert!(home.contains(&format!(r#"<meta property="og:image:alt" content="{alt}">"#)), "{lang}");
        assert!(home.contains(&format!(r#"<meta name="twitter:image:alt" content="{alt}">"#)), "{lang}");
    }
}

/// The part of `html` from `open` up to the first `close` after it.
fn between<'h>(html: &'h str, open: &str, close: &str) -> &'h str {
    let i = html.find(open).unwrap_or_else(|| panic!("no {open}"));
    let rest = &html[i..];
    &rest[..rest.find(close).unwrap_or_else(|| panic!("{open} is never closed by {close}"))]
}

fn slug_of<'p>(pages: &'p [Page], id: &str, lang: &str) -> &'p str {
    let page = pages.iter().find(|p| p.meta.id == id).unwrap_or_else(|| panic!("no page {id}"));
    &page.meta.languages[lang].slug
}

#[test]
fn every_documentation_page_offers_the_whole_menu_its_headings_and_its_neighbours() {
    // The menu, the headings list and the pager are composed from docs.json for every page, so a
    // page that drops out of one of them does it silently: the page itself still renders.
    let out = built("docs-layout");
    let site = repo_root().join("site");
    let docs: DocsConfig = parse_json(&site.join("data").join("docs.json")).expect("docs.json");
    let pages = load_pages(&site).expect("pages");
    let order = docs.order();
    let mut checked = 0;

    for lang in &docs.languages {
        let hrefs: Vec<String> = order.iter().map(|id| url_path(lang, slug_of(&pages, id, lang))).collect();
        for (i, id) in order.iter().enumerate() {
            let rel = output_path(lang, slug_of(&pages, id, lang)).to_string_lossy().replace('\\', "/");
            let html = read(&out, &rel);

            let menu = between(&html, r#"<nav class="docnav""#, "</nav>");
            for href in &hrefs {
                assert!(menu.contains(&format!(r#"href="{href}""#)), "{rel}: the menu has no link to {href}");
            }
            assert_eq!(menu.matches("aria-current").count(), 1, "{rel}: one current page in the menu");
            assert!(menu.contains(&format!(r#"<a href="{}" aria-current="page">"#, hrefs[i])), "{rel}: the current page is not marked");

            let article = between(&html, r#"<article class="doc">"#, "</article>");
            let heads = headings(article, &rel).expect("headings");
            if !heads.is_empty() {
                let toc = between(&html, r#"<nav class="toc""#, "</nav>");
                assert_eq!(toc.matches("<li>").count(), heads.len(), "{rel}: one entry per heading");
                for (anchor, _) in &heads {
                    assert!(toc.contains(&format!(r##"href="#{anchor}""##)), "{rel}: no entry for #{anchor}");
                }
            }

            let pager = between(article, r#"<nav class="pager""#, "</nav>");
            match i.checked_sub(1) {
                Some(p) => assert!(pager.contains(&format!(r#"class="prev" href="{}""#, hrefs[p])), "{rel}: previous"),
                None => assert!(!pager.contains("class=\"prev\""), "{rel}: the first page has no previous one"),
            }
            match hrefs.get(i + 1) {
                Some(n) => assert!(pager.contains(&format!(r#"class="next" href="{n}""#)), "{rel}: next"),
                None => assert!(!pager.contains("class=\"next\""), "{rel}: the last page has no next one"),
            }
            checked += 1;
        }
    }
    assert_eq!(checked, order.len() * docs.languages.len(), "every page in every documentation language");
}

#[test]
fn the_404_offers_every_language_and_links_into_none_of_their_pages() {
    let out = built("404-langs");
    let (cfg, i18n) = configured();
    let page = read(&out, "404.html");
    for l in &cfg.languages {
        let name = &i18n[l]["language_name"];
        let href = if l == "en" { "/".to_string() } else { format!("/{l}/") };
        assert!(page.contains(&format!(r#"<a href="{href}" hreflang="{}" lang="{}">{name}</a>"#, i18n[l]["html_lang"], i18n[l]["html_lang"])), "404: {l}");
    }
    assert!(!page.contains("/404/\""), "the 404 must not offer itself as the English version");
}
