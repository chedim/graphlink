//! The citation file (CITATION.cff): what Zenodo and GitHub read for the project's citation. It names the project and its repository, its license is the
//! crate's, and a `version` in it (written at the release commit, with `date-released`) is the crate's version.
use std::path::Path;

fn read(path: &Path) -> String { std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())) }

/// The value of a top-level `key: value` line, without quotes.
fn field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(&format!("{key}:")).map(|v| v.trim().trim_matches('"').to_string()))
}

const REPO: &str = "graphlink";

fn root() -> &'static Path { Path::new(env!("CARGO_MANIFEST_DIR")) .parent().unwrap() }

#[test]
fn the_citation_file_names_the_project_its_repository_and_its_license() {
    let c = read(&root().join("CITATION.cff"));
    assert_eq!(field(&c, "cff-version").as_deref(), Some("1.2.0"));
    assert_eq!(field(&c, "type").as_deref(), Some("software"));
    assert_eq!(field(&c, "title").as_deref(), Some(REPO));
    assert_eq!(field(&c, "repository-code").as_deref(), Some(format!("https://github.com/chedim/{REPO}").as_str()));
    assert!(c.lines().any(|l| l == "authors:"), "authors are required");
    assert!(field(&c, "message").is_some(), "a message");
    assert!(c.lines().any(|l| l.starts_with("abstract:")), "an abstract");
    let cargo = read(&root().join("Cargo.toml"));
    assert!(cargo.lines().any(|l| l.trim() == format!("license = \"{}\"", field(&c, "license").unwrap())), "the license is the crate's");
}

#[test]
fn a_version_in_the_citation_file_is_the_crates_version() {
    let c = read(&root().join("CITATION.cff"));
    let Some(v) = field(&c, "version") else { return };   // written at the release commit
    let cargo = read(&root().join("Cargo.toml"));
    let crate_v = cargo.lines().find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string())).expect("a version in Cargo.toml");
    assert_eq!(v, crate_v);
    assert!(field(&c, "date-released").is_some(), "a version comes with its date");
}

/// The DOI Zenodo gave the project: the citation file names it (`doi`, and the concept and version DOIs under `identifiers`), and the README shows it as a badge
/// near the top, linking to it.
#[test]
fn a_doi_in_the_citation_file_is_the_readmes_badge_and_is_listed_as_an_identifier() {
    let c = read(&root().join("CITATION.cff"));
    let Some(doi) = field(&c, "doi") else { return };
    assert!(c.contains(&format!("value: \"{doi}\"")), "the concept DOI is also an identifier");
    assert!(c.contains("description: \"Concept DOI"), "and says it is the concept DOI");
    let readme = read(&root().join("README.md"));
    let top: String = readme.lines().take(6).collect::<Vec<_>>().join("\n");
    assert!(top.contains(&format!("[![DOI](https://zenodo.org/badge/DOI/{doi}.svg)](https://doi.org/{doi})")), "the badge is in the first lines of the README: {top}");
}

/// Every DOI in the citation file is a Zenodo DOI of the form 10.5281/zenodo.<digits> and is listed once, and the version DOIs name different versions (a guard: the DOIs are copied from
/// Zenodo's records by hand after each release).
#[test]
fn the_dois_in_the_citation_file_are_well_formed_and_listed_once() {
    let c = read(&root().join("CITATION.cff"));
    let dois: Vec<&str> = c.lines().filter_map(|l| l.trim().strip_prefix("value: \"").and_then(|v| v.strip_suffix('"'))).collect();
    assert!(!dois.is_empty(), "no DOI listed");
    for d in &dois { assert!(d.strip_prefix("10.5281/zenodo.").map_or(false, |n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())), "{d}"); }
    let mut once = dois.clone(); once.sort(); once.dedup();
    assert_eq!(once.len(), dois.len(), "a DOI is listed twice: {dois:?}");
    let versions: Vec<&str> = c.lines().filter_map(|l| l.trim().strip_prefix("description: \"Version DOI of ").and_then(|v| v.strip_suffix('"'))).collect();
    let mut v = versions.clone(); v.sort(); v.dedup();
    assert_eq!(v.len(), versions.len(), "two version DOIs name the same version: {versions:?}");
}
