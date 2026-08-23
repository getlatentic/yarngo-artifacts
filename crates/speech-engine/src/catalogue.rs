//! Which runtimes exist, at which versions, and which of them this build may
//! install.
//!
//! This is compatibility, not security. Whether the catalogue came from us is
//! settled before a byte of it is parsed, by [`crate::trust`]; what is left is
//! the ordinary question of whether two pieces of software fit together. The
//! three versions here answer three different questions and are deliberately
//! not one field:
//!
//! - `schema` — can this build read this document at all;
//! - `engine_api` — can this build hold a conversation with that engine;
//! - `version` — which implementation is installed, recorded so that "which
//!   runtime made this clip" has an answer.
//!
//! Names in a release are TUF target names rather than URLs, and carry no
//! digests. Where a target lives and what it hashes to are the repository's to
//! say; repeating either here would be a second answer to a question that
//! already has one. The names are flat, without path components — a name that
//! reads as a path is a name somebody will eventually make point somewhere.

use std::collections::HashMap;

/// The document this build knows how to read.
pub const SCHEMA: u32 = 1;

/// The engine conversation this build implements. A release declaring a higher
/// number describes an engine this application does not know how to drive, so
/// it is passed over rather than half-understood. That refusal is what makes
/// publishing a runtime without shipping an application safe.
pub const ENGINE_API: u32 = 1;



#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Catalogue {
    pub schema: u32,
    /// Keyed by runtime id, newest first or last — the order is not trusted.
    pub runtimes: HashMap<String, Vec<Release>>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Release {
    /// This runtime's own version, which is its identity once installed.
    pub version: String,
    /// The oldest application this release will work with.
    pub min_app_version: String,
    #[serde(default = "one")]
    pub engine_api: u32,
    /// Target names. The lock is what makes an install reproducible.
    pub lock: String,
    pub pyproject: String,
    /// The runtime's own implementation of the protocol, if it has one.
    /// Absent means it is run by the engine that ships with the application.
    #[serde(default)]
    pub engine: Option<String>,
}

fn one() -> u32 {
    1
}

impl Catalogue {
    /// The target the catalogue itself is published as.
    pub const TARGET: &'static str = "catalogue.json";

    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let catalogue: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("the catalogue is not readable: {e}"))?;
        if catalogue.schema != SCHEMA {
            return Err(format!(
                "the catalogue is written to schema {}, and this build reads {SCHEMA}",
                catalogue.schema
            ));
        }
        Ok(catalogue)
    }

    /// The newest release of `id` this build can use, and why the others were
    /// passed over when there is none.
    ///
    /// Told the application's version rather than reading it, so the decision
    /// can be checked at versions this build does not happen to be.
    pub fn best(&self, id: &str, app: &str) -> Result<&Release, String> {
        let Some(releases) = self.runtimes.get(id) else {
            return Err(format!("the catalogue does not offer {id}"));
        };

        let mut best: Option<&Release> = None;
        let mut passed = Vec::new();
        for release in releases {
            match release.usable_by(app) {
                Err(why) => passed.push(format!("{} — {why}", release.version)),
                Ok(()) if best.is_none_or(|b| newer(&release.version, &b.version)) => {
                    best = Some(release)
                }
                Ok(()) => {}
            }
        }

        best.ok_or_else(|| match passed.len() {
            0 => format!("the catalogue lists no releases of {id}"),
            _ => format!("no release of {id} fits this build: {}", passed.join("; ")),
        })
    }
}

impl Release {
    /// Whether this build may install this release.
    pub fn usable_by(&self, app: &str) -> Result<(), String> {
        // The version becomes a directory name. One that could carry a
        // separator would decide where the runtime lives, and that decision is
        // not the catalogue's to make.
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
        if self.version.is_empty()
            || !self.version.chars().all(plain)
            || self.version.starts_with('.')
        {
            return Err(format!("{:?} is not a version a directory can be named", self.version));
        }
        if self.engine_api > ENGINE_API {
            return Err(format!(
                "speaks engine api {}, and this build speaks {ENGINE_API}",
                self.engine_api
            ));
        }
        if !at_least(app, &self.min_app_version) {
            return Err(format!("needs yarngo {} or newer", self.min_app_version));
        }
        Ok(())
    }
}

/// `1.2.3` against `1.10.0`, without pulling in a version crate.
///
/// Two rules that a string comparison gets wrong. Numbers compare as numbers,
/// or `1.10` sorts below `1.9`. And a pre-release is *older* than the release
/// it precedes — `0.1.0-alpha` comes before `0.1.0` — which matters the moment
/// an alpha is published, because the naive reading has it the other way round
/// and would hand alpha users releases meant for the finished version.
pub fn at_least(have: &str, need: &str) -> bool {
    let (have_n, have_pre) = parts(have);
    let (need_n, need_pre) = parts(need);
    for i in 0..have_n.len().max(need_n.len()) {
        let (h, n) = (
            have_n.get(i).copied().unwrap_or(0),
            need_n.get(i).copied().unwrap_or(0),
        );
        if h != n {
            return h > n;
        }
    }
    // Same numbers: a pre-release satisfies a pre-release floor, but not a
    // finished one.
    !have_pre || need_pre
}

/// Strictly newer, so that equal versions leave the first one found in place.
fn newer(a: &str, b: &str) -> bool {
    at_least(a, b) && !at_least(b, a)
}

fn parts(version: &str) -> (Vec<u32>, bool) {
    let (numbers, pre) = match version.split_once('-') {
        Some((numbers, _)) => (numbers, true),
        None => (version, false),
    };
    let numbers = numbers
        .split('.')
        .map(|part| part.trim().parse().unwrap_or(0))
        .collect();
    (numbers, pre)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, min_app: &str, engine_api: u32) -> Release {
        Release {
            version: version.into(),
            min_app_version: min_app.into(),
            engine_api,
            lock: format!("mlx/{version}/uv.lock"),
            pyproject: format!("mlx/{version}/pyproject.toml"),
            engine: None,
        }
    }

    fn catalogue(releases: Vec<Release>) -> Catalogue {
        Catalogue {
            schema: SCHEMA,
            runtimes: HashMap::from([("mlx".to_string(), releases)]),
        }
    }

    #[test]
    fn numbers_compare_as_numbers_and_a_pre_release_comes_first() {
        assert!(at_least("1.10.0", "1.9.0"), "1.10 is after 1.9");
        assert!(!at_least("1.9.0", "1.10.0"));
        assert!(at_least("1.2.3", "1.2.3"));
        assert!(at_least("1.2", "1.2.0"), "absent parts are zero");

        assert!(!at_least("0.1.0-alpha.1", "0.1.0"), "an alpha is not the release");
        assert!(at_least("0.1.0", "0.1.0-alpha.1"));
        assert!(at_least("0.1.0-alpha.1", "0.1.0-alpha.1"));
    }

    /// The catalogue's order is not a promise, so picking is by version.
    #[test]
    fn the_newest_usable_release_is_chosen_whatever_the_order() {
        let listed = catalogue(vec![
            release("1.2.0", "0.1.0", 1),
            release("1.10.0", "0.1.0", 1),
            release("1.9.0", "0.1.0", 1),
        ]);
        assert_eq!(listed.best("mlx", "0.1.0").expect("a release").version, "1.10.0");
    }

    /// Publishing a runtime for a newer application must not break an older
    /// one; it takes the newest release that still fits.
    #[test]
    fn a_release_for_a_newer_application_is_passed_over() {
        let listed = catalogue(vec![
            release("1.0.0", "0.1.0", 1),
            release("2.0.0", "0.9.0", 1),
        ]);
        assert_eq!(listed.best("mlx", "0.1.0").expect("a release").version, "1.0.0");
        assert_eq!(listed.best("mlx", "0.9.0").expect("a release").version, "2.0.0");
    }

    /// An engine this build cannot hold a conversation with is not one to
    /// install, however new it is.
    #[test]
    fn a_release_speaking_a_later_engine_api_is_passed_over() {
        let listed = catalogue(vec![
            release("1.0.0", "0.1.0", 1),
            release("2.0.0", "0.1.0", ENGINE_API + 1),
        ]);
        assert_eq!(listed.best("mlx", "0.1.0").expect("a release").version, "1.0.0");
    }

    /// A version is about to be a directory name, so one that reads as a path
    /// is not a version.
    #[test]
    fn a_version_that_reads_as_a_path_is_not_installable() {
        for version in ["../escape", "a/b", "", ".hidden", "x\\y"] {
            let listed = catalogue(vec![release(version, "0.1.0", 1)]);
            let refused = listed.best("mlx", "9.9.9").expect_err("a path is not a version");
            assert!(refused.contains("version"), "{version:?}: {refused}");
        }
    }

    /// Nothing to install is a thing to say clearly: the reason each release
    /// was passed over is what tells someone whether to update the app or wait.
    #[test]
    fn when_nothing_fits_it_says_what_did_not() {
        let listed = catalogue(vec![
            release("2.0.0", "9.0.0", 1),
            release("3.0.0", "0.1.0", ENGINE_API + 1),
        ]);
        let why = listed.best("mlx", "0.1.0").expect_err("nothing fits");
        assert!(why.contains("2.0.0") && why.contains("9.0.0"), "{why}");
        assert!(why.contains("3.0.0") && why.contains("engine api"), "{why}");

        let missing = listed.best("torch", "0.1.0").expect_err("not offered");
        assert!(missing.contains("does not offer torch"), "{missing}");
    }

    /// A catalogue written to a schema this build does not read is not one to
    /// guess at.
    #[test]
    fn a_catalogue_from_a_later_schema_is_refused() {
        let later = serde_json::to_vec(&Catalogue {
            schema: SCHEMA + 1,
            runtimes: HashMap::new(),
        })
        .expect("serialise");

        let why = Catalogue::read(&later).expect_err("a schema we do not read");
        assert!(why.contains("schema"), "{why}");
    }

    #[test]
    fn a_release_may_leave_the_engine_out_and_be_run_by_the_one_that_ships() {
        let listed: Catalogue = serde_json::from_str(
            r#"{"schema":1,"runtimes":{"mlx":[
                 {"version":"1.0.0","min_app_version":"0.1.0",
                  "lock":"mlx/1.0.0/uv.lock","pyproject":"mlx/1.0.0/pyproject.toml"}]}}"#,
        )
        .expect("parse");

        let chosen = listed.best("mlx", "0.1.0").expect("a release");
        assert_eq!(chosen.engine, None);
        assert_eq!(chosen.engine_api, 1, "an unstated engine api is this one");
    }
}
