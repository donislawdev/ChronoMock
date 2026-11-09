//! `chrono calendars` - the calendar catalogue, read by the engine for every surface (R5-D50, the
//! pattern of ADR-22).
//!
//! The window offered three calendars written into its own code, and a preset whose market named a
//! fourth left the calendar already picked in place without a word, so a business-day step counted
//! in the wrong country (R5-P12). The engine is the one reader of the calendar files, so it is the one
//! that says which there are. This module lists the folder `--calendar` looks in through
//! [`calendar_title_at`] - the reader `--calendar` uses once it has found a file - and hands each file
//! on as a calendar with its title, or as a refusal with the sentence `--calendar` gives for it.
//!
//! The invariant is the point: a calendar is listed exactly when `calc --calendar <id>` counts in it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::calendar::{calendar_title_at, find_catalogue_dir, is_valid_catalogue_id, parse_catalogue_args};
use crate::output::{diag, out, outln};

/// The machine shape of the catalogue - additive fields only, like every other `chronomock.*/1`.
const CALENDARS_SCHEMA: &str = "chronomock.calendars/1";

/// The whole catalogue: where it was read from, the calendars it holds, and the files it left out.
#[derive(Debug, Serialize)]
pub(crate) struct CalendarCatalogueJson {
    schema: &'static str,
    dir: String,
    calendars: Vec<CalendarEntryJson>,
    refused: Vec<RefusedJson>,
}

/// One calendar, by the id `--calendar` takes, with what a person knows it by.
#[derive(Debug, Serialize)]
struct CalendarEntryJson {
    file: String,
    id: String,
    country: String,
    /// The file's own name for the calendar, absent when the file gives none.
    name: Option<NameJson>,
}

#[derive(Debug, Serialize)]
struct NameJson {
    en: String,
    local: String,
}

/// A file the catalogue left out, with the sentence `--calendar` gives for it. `id` is the name the
/// file is found by, absent when that name cannot be one.
#[derive(Debug, Serialize)]
struct RefusedJson {
    file: String,
    id: Option<String>,
    reason: String,
}

/// Read every calendar file in `dir`. `Err` only when the folder itself cannot be read - a file that
/// cannot be used is a refusal inside the answer, not a failure of it.
pub(crate) fn read_calendar_catalogue(dir: &Path) -> Result<CalendarCatalogueJson, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read the calendar folder {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut refused = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                let is_json = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
                if is_json && path.is_file() {
                    files.push(path);
                }
            }
            // Said, not skipped: a folder whose listing broke half way is not a folder with fewer files.
            Err(e) => refused.push(RefusedJson {
                file: String::new(),
                id: None,
                reason: format!("an entry of {} could not be read: {e}", dir.display()),
            }),
        }
    }
    // By file name without case, the way Windows orders and finds them - stable on every machine.
    files.sort_by_key(|path| file_name(path).to_lowercase());

    let mut calendars = Vec::new();
    for path in files {
        match catalogue_entry(&path) {
            Ok(entry) => calendars.push(entry),
            Err(refusal) => refused.push(refusal),
        }
    }
    Ok(CalendarCatalogueJson { schema: CALENDARS_SCHEMA, dir: dir.display().to_string(), calendars, refused })
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// One file, listed or refused.
fn catalogue_entry(path: &Path) -> Result<CalendarEntryJson, RefusedJson> {
    let file = file_name(path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    // `--calendar` takes an id, and an id is the file name - a name it refuses can never be used.
    if !is_valid_catalogue_id(&stem) {
        return Err(RefusedJson {
            reason: format!(
                "'{file}' cannot be named by --calendar - a calendar's file name is its id, made of letters, digits, '-' and '_'"
            ),
            file,
            id: None,
        });
    }
    match calendar_title_at(path, &stem) {
        Ok(title) => Ok(CalendarEntryJson {
            file,
            id: stem,
            country: title.country,
            name: title.name.map(|(en, local)| NameJson { en, local }),
        }),
        Err(reason) => Err(RefusedJson { file, id: Some(stem), reason }),
    }
}

/// `chrono calendars [--dir <folder>] [--json]`. Exit 0 when the folder was read, whatever it held - a
/// refused file is part of the answer. Exit 1 for a bad argument or a folder that is missing or cannot
/// be read.
pub(crate) fn calendars_run(args: &[String]) -> i32 {
    let parsed = match parse_catalogue_args("calendars", args) {
        Ok(parsed) => parsed,
        Err(e) => {
            diag!("chrono calendars: {e}");
            crate::cli::print_calendars_usage(crate::output::Stream::Err);
            return 1;
        }
    };
    let Some(dir) = parsed.dir.or_else(|| find_catalogue_dir("calendars")) else {
        diag!(
            "chrono calendars: no calendar folder ({}) (calendars.folder_missing)",
            crate::calendar::catalogue_search_places("calendars")
        );
        return 1;
    };
    let catalogue = match read_calendar_catalogue(&dir) {
        Ok(catalogue) => catalogue,
        Err(e) => {
            // Two keys, as for presets: a folder that is not there is an install to repair, one that is
            // there and refuses to be read is a permission to look at. Written out in full, so the
            // translation guard (wire_keys.rs) sees both.
            if dir.exists() {
                diag!("chrono calendars: {e} (calendars.folder_unreadable)");
            } else {
                diag!("chrono calendars: {e} (calendars.folder_missing)");
            }
            return 1;
        }
    };
    if parsed.json {
        match serde_json::to_string(&catalogue) {
            Ok(json) => outln!("{json}"),
            Err(e) => {
                diag!("chrono calendars: cannot write the catalogue: {e}");
                return 1;
            }
        }
    } else {
        out!("{}", render_calendar_catalogue(&catalogue));
    }
    0
}

/// The catalogue for a person: one line per calendar, then every file left out with its reason.
fn render_calendar_catalogue(catalogue: &CalendarCatalogueJson) -> String {
    let mut out = String::from("Chrono Mock - calendar catalogue\n");
    out.push_str(&format!("  folder:    {}\n", catalogue.dir));
    out.push_str(&format!(
        "  calendars: {} listed, {} left out\n",
        catalogue.calendars.len(),
        catalogue.refused.len()
    ));
    let id_width = catalogue.calendars.iter().map(|c| c.id.len()).max().unwrap_or(0);
    if !catalogue.calendars.is_empty() {
        out.push('\n');
    }
    for c in &catalogue.calendars {
        let name = c.name.as_ref().map(|n| n.en.as_str()).unwrap_or_default();
        out.push_str(&format!("  {:<id_width$}  {:<3}  {name}\n", c.id, c.country));
    }
    if !catalogue.refused.is_empty() {
        out.push_str("\n  left out - `--calendar` refuses these too:\n");
        for r in &catalogue.refused {
            out.push_str(&format!("    {}  {}\n", r.file, r.reason));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped_calendars() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../calendars")
    }

    /// A folder holding the shipped Polish calendar under its own name, plus `files` written as given.
    fn folder_with(files: &[(&str, &str)]) -> PathBuf {
        let dir = crate::testutil::unique_temp_dir("chrono-calendar-catalogue");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(shipped_calendars().join("pl.json"), dir.join("pl.json")).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        dir
    }

    /// Relative to the crate, where cargo runs its tests, so the folder reads the same on every machine.
    const RECORDED_SHIPPED: &str = "tests/data/calendars-shipped.json";

    /// The SHIPPED catalogue gives exactly the answer recorded beside the tests. That file is what the
    /// window's tests read as the real list of calendars, so this is one end of a bridge: a change to
    /// what the engine hands on turns this red, and a change to what the window makes of it turns that
    /// red. A deliberate change is recorded with `CHRONO_BLESS=1` and read as a diff.
    #[test]
    fn the_shipped_calendars_give_their_recorded_answer() {
        let catalogue = read_calendar_catalogue(Path::new("../../calendars")).expect("the shipped calendars are readable");
        let got = serde_json::to_string_pretty(&catalogue).expect("the catalogue serialises") + "\n";
        if std::env::var_os("CHRONO_BLESS").is_some() {
            std::fs::write(RECORDED_SHIPPED, &got).expect("the recorded answer is writable");
            return;
        }
        let want = std::fs::read_to_string(RECORDED_SHIPPED).expect("the recorded answer exists").replace("\r\n", "\n");
        assert_eq!(got, want, "the shipped calendars no longer give the recorded answer - record a deliberate change with CHRONO_BLESS=1");
    }

    /// Every shipped calendar is listed and none is refused - a shipped file the catalogue leaves out is
    /// a calendar nobody can pick in the window.
    #[test]
    fn every_shipped_calendar_is_listed_with_its_name() {
        let dir = shipped_calendars();
        let catalogue = read_calendar_catalogue(&dir).expect("the shipped calendars are readable");
        let files = std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json")).count();
        assert!(catalogue.refused.is_empty(), "refused: {:?}", catalogue.refused);
        assert_eq!(catalogue.calendars.len(), files);
        assert!(files >= 3, "the shipped calendars look missing: {files}");
        assert!(catalogue.calendars.iter().all(|c| c.name.is_some()), "a shipped calendar without its name");
    }

    /// A calendar is listed exactly when `--calendar` counts in it: a file the reader refuses is left out
    /// with the very sentence `calc --calendar` gives for it, and a file name that cannot be an id is left
    /// out as that.
    #[test]
    fn a_calendar_is_listed_exactly_when_calendar_takes_it() {
        let pl = std::fs::read_to_string(shipped_calendars().join("pl.json")).unwrap();
        let other_id = pl.replacen("\"id\": \"pl\"", "\"id\": \"de\"", 1);
        let no_name = pl.replacen("\"name\": {\n    \"en\": \"Poland\",\n    \"local\": \"Polska\"\n  },\n", "", 1);
        assert_ne!(no_name, pl, "the fixture lost its name block");
        let dir = folder_with(&[
            ("broken.json", "{ not json"),
            ("other.json", &other_id),
            ("no-name.json", &no_name.replacen("\"id\": \"pl\"", "\"id\": \"no-name\"", 1)),
            ("two words.json", &pl),
            ("notes.txt", "not a catalogue file"),
        ]);
        let catalogue = read_calendar_catalogue(&dir).unwrap();

        let listed: Vec<&str> = catalogue.calendars.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(listed, ["no-name", "pl"], "listed: {listed:?}");
        let refused: Vec<&str> = catalogue.refused.iter().map(|r| r.file.as_str()).collect();
        assert_eq!(refused, ["broken.json", "other.json", "two words.json"], "refused: {refused:?}");

        for r in &catalogue.refused {
            match &r.id {
                Some(id) => {
                    let used = crate::calendar::load_calendar_at(&dir.join(&r.file), id).unwrap_err();
                    assert_eq!(r.reason, used, "{} is refused in other words than --calendar uses", r.file);
                }
                None => assert!(r.reason.contains("cannot be named by --calendar"), "{}", r.reason),
            }
        }
        let pl_entry = &catalogue.calendars[1];
        assert_eq!(pl_entry.country, "PL");
        let name = pl_entry.name.as_ref().expect("pl has its name");
        assert_eq!((name.en.as_str(), name.local.as_str()), ("Poland", "Polska"));
        assert!(catalogue.calendars[0].name.is_none(), "a file without a name is listed without one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_is_an_error_not_an_empty_catalogue() {
        let missing = crate::testutil::unique_temp_dir("chrono-no-calendars");
        let err = read_calendar_catalogue(&missing).unwrap_err();
        assert!(err.contains("cannot read the calendar folder"), "{err}");
    }

    #[test]
    fn the_text_names_the_folder_counts_and_every_file_left_out() {
        let dir = folder_with(&[("broken.json", "{ not json")]);
        let catalogue = read_calendar_catalogue(&dir).unwrap();
        let text = render_calendar_catalogue(&catalogue);
        assert!(text.contains("calendars: 1 listed, 1 left out"), "{text}");
        assert!(text.contains("pl  PL   Poland"), "{text}");
        assert!(text.contains("left out - `--calendar` refuses these too:"), "{text}");
        assert!(text.contains("broken.json"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendars_names_itself_when_it_refuses_an_argument() {
        let words = ["--calendar".to_string()];
        assert_eq!(parse_catalogue_args("calendars", &words).unwrap_err(), "unknown argument '--calendar' for calendars");
    }
}
