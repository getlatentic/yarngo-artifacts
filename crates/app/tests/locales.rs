//! Every phrase the interface asks for exists, in every language.
//!
//! rust-i18n answers a key it does not have with the key itself, so a missing
//! phrase is not a build error or a blank space — it is `status.built_in_ready`
//! rendered into the status bar, in front of someone on their first run. That
//! is exactly what this test was written after finding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The languages this application claims to speak. A phrase present in one and
/// missing from another is a half-translated interface, which reads worse than
/// an untranslated one.
const LOCALES: [&str; 2] = ["en", "sk"];

fn app() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// `section.key -> the locales it is written in`, read from the file rather
/// than from a parser we would then have to trust.
fn defined() -> BTreeMap<String, BTreeSet<String>> {
    let text = std::fs::read_to_string(app().join("locales/app.yml")).expect("locales");
    let mut found = BTreeMap::new();
    let (mut section, mut key) = (String::new(), String::new());

    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let name = line.trim().trim_end_matches(':').split(':').next().unwrap_or_default();
        match indent {
            0 if line.trim_end().ends_with(':') => {
                section = name.to_string();
                key.clear();
            }
            2 if line.trim_end().ends_with(':') => {
                key = name.to_string();
                found.insert(format!("{section}.{key}"), BTreeSet::new());
            }
            4 if !section.is_empty() && !key.is_empty() => {
                if let Some(locales) = found.get_mut(&format!("{section}.{key}")) {
                    locales.insert(name.to_string());
                }
            }
            _ => {}
        }
    }
    found
}

/// Every key handed to `t!` in the interface.
///
/// Two things this has to get right, both learned by getting them wrong:
/// `format!(` ends in `t!(` too, and a call long enough to wrap puts its key on
/// the next line.
fn asked_for() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for entry in std::fs::read_dir(app().join("src")).expect("src") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        keys.extend(keys_in(&std::fs::read_to_string(&path).expect("read")));
    }
    keys
}

fn keys_in(source: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    while let Some(found) = source[at..].find("t!(") {
        let start = at + found;
        at = start + 3;
        // `format!(`, `assert!(` and friends all end in `t!(`.
        let before = start.checked_sub(1).map(|i| bytes[i] as char);
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let rest = source[at..].trim_start();
        let Some(quoted) = rest.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = quoted.find('"') {
            keys.insert(quoted[..end].to_string());
        }
    }
    keys
}

#[test]
fn the_scanner_reads_calls_and_not_the_things_that_look_like_them() {
    let source = r#"
        let a = t!("one.two");
        let b = format!("{:.0}%", x);
        let c = t!(
            "three.four",
            length = n
        );
        assert!(t.is_empty(), "not.a.key");
    "#;
    assert_eq!(
        keys_in(source),
        ["one.two", "three.four"].map(ToString::to_string).into_iter().collect()
    );
}

#[test]
fn every_phrase_the_interface_asks_for_exists() {
    let defined = defined();
    let missing: Vec<String> =
        asked_for().into_iter().filter(|key| !defined.contains_key(key)).collect();
    assert!(
        missing.is_empty(),
        "these would render as their own key, in front of somebody: {missing:?}"
    );
}

#[test]
fn every_phrase_is_written_in_every_language() {
    let wanted: BTreeSet<String> = LOCALES.iter().map(ToString::to_string).collect();
    let thin: Vec<(String, BTreeSet<String>)> = defined()
        .into_iter()
        .filter(|(_, locales)| *locales != wanted)
        .collect();
    assert!(thin.is_empty(), "half-translated: {thin:?}");
}

/// A phrase nothing asks for is one somebody will keep translating.
///
/// Named anywhere in the source, not only inside `t!`: a plural picks its key
/// with an `if` and hands the variable over, and that is still the interface
/// asking for it.
#[test]
fn nothing_is_translated_that_nothing_asks_for() {
    let mut source = String::new();
    for entry in std::fs::read_dir(app().join("src")).expect("src") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            source.push_str(&std::fs::read_to_string(&path).expect("read"));
        }
    }

    let orphaned: Vec<String> = defined()
        .into_keys()
        .filter(|key| !source.contains(&format!("\"{key}\"")))
        .collect();
    assert!(orphaned.is_empty(), "defined and never asked for: {orphaned:?}");
}
