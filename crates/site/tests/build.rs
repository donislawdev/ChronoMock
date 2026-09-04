//! End-to-end guard: build the real site and read what came out.
//!
//! The unit tests in the library cover the rules in isolation. This file covers the
//! thing those cannot - that the rules, the content and the configuration in this
//! repository still compose into pages that are correct. Every assertion here failed
//! at least once while the generator was being written.

use std::fs;
use std::path::{Path, PathBuf};

use chrono_site::{render, repo_root};

/// Build into a directory of this test's own, so tests running in parallel do not
/// wipe each other's output.
fn built(name: &str) -> PathBuf {
    let out = std::env::temp_dir().join(format!("chrono-site-test-{name}"));
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
        en.contains(r#"class="lang" href="/pl/" hreflang="pl" title="Ta strona po polsku">Polski<"#),
        "the English page must offer Polski, in Polish"
    );

    let pl = read(&out, "pl/index.html");
    assert!(
        pl.contains(r#"class="lang" href="/" hreflang="en" title="This page in English">English<"#),
        "the Polish page must offer English, in English"
    );
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
