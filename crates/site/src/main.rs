//! Builds the static website into a directory ready for GitHub Pages.
//!
//!     cargo run -p chrono-site                 # build into dist/site, report problems
//!     cargo run -p chrono-site -- --strict     # ...and fail on a dead internal link
//!
//! `--strict` is what the publishing workflow uses. Without it the build still says
//! what is wrong but produces the site anyway, so that a page can be looked at while
//! the pages it links to are still being written.
//!
//! The work itself is in the library (`render`), so the end-to-end guard in `tests/`
//! exercises the same code path this binary does.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono_site::{render, repo_root};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict");

    let out = match args.iter().position(|a| a == "--out") {
        Some(i) => match args.get(i + 1) {
            Some(dir) => PathBuf::from(dir),
            None => {
                eprintln!("chrono-site: --out needs a directory");
                return ExitCode::from(2);
            }
        },
        None => repo_root().join("dist").join("site"),
    };

    let report = match render::build(&repo_root(), &out) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("chrono-site: {e}");
            return ExitCode::from(1);
        }
    };

    println!(
        "chrono-site: {} pages in {} languages -> {}",
        report.pages_written,
        report.languages,
        out.display()
    );

    if report.dangling_count() == 0 {
        return ExitCode::SUCCESS;
    }

    // Reported whether or not it is fatal. A build that quietly drops a dead link is
    // how one gets published.
    eprintln!(
        "\nchrono-site: {} internal link(s) point at pages that do not exist yet:",
        report.dangling_count()
    );
    for (page, links) in &report.dangling_links {
        for link in links {
            eprintln!("  {page} -> {link}");
        }
    }

    if strict {
        eprintln!("\nrefusing to publish a site with dead internal links (--strict)");
        return ExitCode::from(1);
    }
    eprintln!("\n(not fatal without --strict - these pages are still to be written)");
    ExitCode::SUCCESS
}
