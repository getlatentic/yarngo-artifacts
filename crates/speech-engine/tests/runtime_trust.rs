//! What a signed repository establishes that a digest in a manifest does not.
//!
//! A runtime is code the application runs, so the question is not only whether
//! the bytes match what the manifest said — it is who wrote the manifest, and
//! whether it is the current one. Those are the properties we would otherwise
//! have to invent, so they are demonstrated against the library that already
//! has them rather than described in a design note.
//!
//! The repositories under `tests/fixtures/tuf` are built by
//! `scripts/make-tuf-fixture.sh`.

use std::fs;
use std::path::{Path, PathBuf};

use tough::{FilesystemTransport, Limits, RepositoryLoader, TargetName};
use url::Url;

const TARGET: &str = "yarngo-runtime-spike.tar.gz";
const CONTENTS: &str = "a runtime archive, as far as this test is concerned";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tuf")
}

/// Fetch `TARGET` from `served`, trusting only `root`. They are separate
/// arguments because pointing them at different repositories is the attack.
async fn fetch(root: &Path, served: &Path) -> Result<Vec<u8>, String> {
    let anchor = fs::read(root).map_err(|e| format!("reading root.json: {e}"))?;
    let store = tempfile::tempdir().map_err(|e| e.to_string())?;

    let repository = RepositoryLoader::new(
        &anchor,
        directory(&served.join("metadata")),
        directory(&served.join("targets")),
    )
    .transport(FilesystemTransport)
    .limits(Limits::default())
    .datastore(store.path())
    .load()
    .await
    .map_err(|e| format!("{e}"))?;

    let name = TargetName::new(TARGET).map_err(|e| format!("{e}"))?;
    let stream = repository
        .read_target(&name)
        .await
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "no such target".to_string())?;

    tough::IntoVec::into_vec(stream)
        .await
        .map_err(|e| format!("{e}"))
}

fn directory(path: &Path) -> Url {
    Url::from_directory_path(path).expect("fixture paths are absolute")
}

/// Refused, and refused for the stated reason. A fixture that had simply gone
/// missing would satisfy "this fails" in every one of these tests.
fn refused(outcome: Result<Vec<u8>, String>, because: &str) {
    let complaint = outcome.err().expect("this should not have been accepted");
    assert!(
        complaint.to_lowercase().contains(because),
        "refused, but not over {because}: {complaint}"
    );
}

/// A writable copy, so a test can tamper without editing the fixture.
fn copy_of(name: &str) -> (tempfile::TempDir, PathBuf) {
    let scratch = tempfile::tempdir().expect("temp dir");
    let to = scratch.path().join(name);
    copy_tree(&fixtures().join(name), &to);
    (scratch, to)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

/// Change one byte in place, leaving the length alone, so that length is not
/// what gives the edit away.
fn flip_a_byte(path: &Path) {
    let mut bytes = fs::read(path).expect("read to tamper with");
    let last = bytes.len() - 2;
    bytes[last] ^= 0x20;
    fs::write(path, bytes).expect("write tampered");
}

fn only_file_in(dir: &Path) -> PathBuf {
    let mut found: Vec<_> = fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("dir entry").path())
        .collect();
    assert_eq!(found.len(), 1, "expected one file in {}", dir.display());
    found.pop().expect("the one file")
}

#[tokio::test]
async fn a_signed_repository_yields_the_target() {
    let repo = fixtures().join("repo");
    let bytes = fetch(&repo.join("root.json"), &repo).await.expect("fetch");
    assert_eq!(String::from_utf8_lossy(&bytes), CONTENTS);
}

#[tokio::test]
async fn a_target_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    // The copy is a working repository until the moment it is edited, so the
    // refusal below is the edit and not the copying.
    fetch(&repo.join("root.json"), &repo)
        .await
        .expect("the untouched copy should load");

    flip_a_byte(&only_file_in(&repo.join("targets")));
    refused(fetch(&repo.join("root.json"), &repo).await, "hash mismatch");
}

#[tokio::test]
async fn metadata_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    fetch(&repo.join("root.json"), &repo)
        .await
        .expect("the untouched copy should load");

    flip_a_byte(&repo.join("metadata/1.targets.json"));
    refused(fetch(&repo.join("root.json"), &repo).await, "hash mismatch");
}

/// The question a digest in a manifest cannot answer. The impostor repository
/// is correctly built and correctly signed — by keys that were never ours,
/// which is exactly what someone who can publish to the artifact host holds.
#[tokio::test]
async fn a_repository_signed_by_someone_else_is_refused() {
    let theirs = fixtures().join("impostor");

    // Against their own root it is a perfectly good repository, and the bytes
    // they serve are the ones we would have served, so nothing about the target
    // gives them away.
    let bytes = fetch(&theirs.join("root.json"), &theirs)
        .await
        .expect("their repository is well formed");
    assert_eq!(String::from_utf8_lossy(&bytes), CONTENTS);

    refused(
        fetch(&fixtures().join("repo/root.json"), &theirs).await,
        "signature threshold",
    );
}

/// Freshness, which a digest also cannot answer: metadata signed by the right
/// keys and long out of date. Replaying an old snapshot is how someone keeps a
/// machine on the version they already know how to break.
#[tokio::test]
async fn metadata_that_has_expired_is_refused() {
    let stale = fixtures().join("stale");
    refused(fetch(&stale.join("root.json"), &stale).await, "expired");
}
