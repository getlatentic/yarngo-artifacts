//! What has been published, once it is established that we published it.
//!
//! Trust starts with the root role shipped inside the application, and so is
//! covered by the application's own signature. Without it there is nothing to
//! check a repository against, and no runtime is fetched at all — an install
//! then uses the recipe that shipped, which is the same thing it does with no
//! network.

use std::path::Path;

use crate::catalogue::{Catalogue, Release};
use crate::paths;
use crate::trust::{Anchor, Trusted};


/// Where the signed repository is served from. Metadata and targets sit under
/// it, so it is one address rather than two to keep in step.
///
/// Pages rather than `raw.githubusercontent.com`, which caches for five
/// minutes and is not meant to be used as a content network. The same files
/// from the same repository, served by something built to serve them.
///
/// Changing this address is free only until a build ships pointing at the old
/// one, because it is compiled into every copy — after that, the old address
/// has to keep answering for as long as those copies exist.
const REPOSITORY: &str = "https://getlatentic.github.io/yarngo-studio-artifacts/tuf/";

/// The root role, inside the bundle. The one file whose authenticity comes from
/// somewhere other than TUF, because it is where TUF starts.
const ROOT: &str = "tuf/root.json";

/// A release of one runtime, and the repository that vouches for it.
pub struct Published {
    trusted: Trusted,
    release: Release,
}

impl Published {
    /// The newest release of `id` this build can install, from the repository
    /// it ships trusting.
    ///
    /// Every failure is a reason not to fetch rather than a reason to stop:
    /// callers carry on with what shipped.
    pub fn newest(id: &str) -> Result<Self, String> {
        Self::offered_by(anchor()?, id, env!("CARGO_PKG_VERSION"))
    }

    /// Told where trust starts and which application is asking, rather than
    /// reaching for either, so what it chooses can be checked.
    pub fn offered_by(anchor: Anchor, id: &str, app: &str) -> Result<Self, String> {
        let trusted = anchor.open()?;
        let catalogue = Catalogue::read(&trusted.read(Catalogue::TARGET)?)?;
        let release = catalogue.best(id, app)?.clone();
        Ok(Self { trusted, release })
    }

    /// Which release this is. Its identity once installed, and the answer to
    /// "which runtime made this".
    pub fn version(&self) -> &str {
        &self.release.version
    }

    /// Put this release's recipe where `uv` will read it.
    ///
    /// Reports whether anything changed, so an install that is already on this
    /// release says nothing rather than announcing an update that did not
    /// happen.
    pub fn stage_recipe(&self, project: &Path) -> Result<bool, String> {
        let lock = self.trusted.read(&self.release.lock)?;
        let pyproject = self.trusted.read(&self.release.pyproject)?;

        let staged = project.join("uv.lock");
        if staged.exists() && std::fs::read(&staged).is_ok_and(|had| had == lock) {
            return Ok(false);
        }

        std::fs::create_dir_all(project).map_err(|e| e.to_string())?;
        write(&project.join("uv.lock"), &lock)?;
        write(&project.join("pyproject.toml"), &pyproject)?;
        Ok(true)
    }

    /// Put this release's own implementation of the protocol in its folder, if
    /// it has one.
    ///
    /// Reports whether the runtime now has an engine of its own. A release
    /// naming none is not a failure — it is run by the engine that ships with
    /// the application, which is what the first runtime does.
    pub fn stage_engine(&self, id: &str, folder: &Path) -> Result<bool, String> {
        let Some(target) = &self.release.engine else {
            return Ok(false);
        };

        // Held whole because it is small and because extraction reads it as
        // one archive; the caps in `extract_under` are what bound it.
        let archive = self.trusted.read(target)?;
        crate::runtime::unpack_engine(id, folder, &archive)?;
        Ok(true)
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Where trust starts for this build: the root role inside the bundle, and the
/// address the repository is served from.
///
/// A test may substitute the root — and only a test: the override is read
/// exclusively under `YARNGO_TEST_MODE`, which the data layer already refuses
/// to mix with a real installation. In production the root ships inside the
/// signed bundle or there is no root, and `YARNGO_REPOSITORY` alone redirects
/// nothing because a repository the shipped root did not sign does not open.
fn anchor() -> Result<Anchor, String> {
    let root = match test_root() {
        Some(substituted) => substituted,
        None => paths::resource(ROOT).ok_or_else(|| {
            "no root role ships with this build, so nothing is published to it".to_string()
        })?,
    };
    let root = std::fs::read(&root).map_err(|e| format!("cannot read {}: {e}", root.display()))?;

    at(
        &base(),
        root,
        // Trust state, not cache: being handed last year's metadata again is
        // only visible to something that remembers this year's. It lives in
        // its own tree so that no runtime install, removal or reinstall ever
        // touches it — forgetting is what a rollback needs.
        paths::data_dir().join("trust").join("runtime-repository"),
    )
}

/// An anchor for a repository served from `base`, wherever that is.
pub fn at(base: &str, root: Vec<u8>, datastore: std::path::PathBuf) -> Result<Anchor, String> {
    // Joining against a base without a trailing slash would discard its last
    // segment.
    let base = if base.ends_with('/') { base.to_string() } else { format!("{base}/") };
    let base =
        url::Url::parse(&base).map_err(|e| format!("the repository address is not a url: {e}"))?;

    Ok(Anchor {
        root,
        metadata: base.join("metadata/").map_err(|e| e.to_string())?,
        targets: base.join("targets/").map_err(|e| e.to_string())?,
        datastore,
    })
}

fn test_root() -> Option<std::path::PathBuf> {
    std::env::var_os("YARNGO_TEST_MODE")?;
    std::env::var_os("YARNGO_TRUST_ROOT").map(std::path::PathBuf::from)
}

/// Overridable so a release can be staged somewhere else before it is
/// published. On its own this redirects nothing: the root role still ships with
/// the application, and a repository it did not sign does not open.
fn base() -> String {
    std::env::var("YARNGO_REPOSITORY").unwrap_or_else(|_| REPOSITORY.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_address_without_a_trailing_slash_names_the_same_place() {
        let store = std::path::PathBuf::from("/tmp/unused");
        let named = |base| at(base, Vec::new(), store.clone()).expect("anchor");

        let slash = named("https://example.test/tuf/");
        let bare = named("https://example.test/tuf");
        assert_eq!(bare.metadata, slash.metadata);
        assert_eq!(bare.targets, slash.targets);
        assert_eq!(
            slash.metadata.as_str(),
            "https://example.test/tuf/metadata/"
        );
    }
}
