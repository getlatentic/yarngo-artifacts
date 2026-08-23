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

use speech_engine::trust::Anchor;
use url::Url;

const TARGET: &str = "yarngo-runtime-spike.tar.gz";
const CONTENTS: &str = "a runtime archive, as far as this test is concerned";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tuf")
}

/// Open `served`, trusting only `root`. They are separate arguments because
/// pointing them at different repositories is the attack.
fn open(root: &Path, served: &Path) -> Result<(tempfile::TempDir, speech_engine::trust::Trusted), String> {
    let store = tempfile::tempdir().map_err(|e| e.to_string())?;
    let trusted = Anchor {
        root: fs::read(root).map_err(|e| format!("reading root.json: {e}"))?,
        metadata: directory(&served.join("metadata")),
        targets: directory(&served.join("targets")),
        datastore: store.path().join("seen"),
    }
    .open()?;
    Ok((store, trusted))
}

fn fetch(root: &Path, served: &Path) -> Result<Vec<u8>, String> {
    let (_store, trusted) = open(root, served)?;
    trusted.read(TARGET)
}

fn directory(path: &Path) -> Url {
    Url::from_directory_path(path).expect("fixture paths are absolute")
}

/// Refused, and refused for the stated reason. A fixture that had simply gone
/// missing would satisfy "this fails" in every one of these tests.
fn refused(outcome: Result<Vec<u8>, String>, because: &str) {
    let complaint = outcome.expect_err("this should not have been accepted");
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

#[test]
fn a_signed_repository_yields_the_target() {
    let repo = fixtures().join("repo");
    let bytes = fetch(&repo.join("root.json"), &repo).expect("fetch");
    assert_eq!(String::from_utf8_lossy(&bytes), CONTENTS);
}

#[test]
fn a_target_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    // The copy is a working repository until the moment it is edited, so the
    // refusal below is the edit and not the copying.
    fetch(&repo.join("root.json"), &repo).expect("the untouched copy should load");

    flip_a_byte(&only_file_in(&repo.join("targets")));
    refused(fetch(&repo.join("root.json"), &repo), "hash mismatch");
}

#[test]
fn metadata_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    fetch(&repo.join("root.json"), &repo).expect("the untouched copy should load");

    flip_a_byte(&repo.join("metadata/1.targets.json"));
    refused(fetch(&repo.join("root.json"), &repo), "hash mismatch");
}

/// The question a digest in a manifest cannot answer. The impostor repository
/// is correctly built and correctly signed — by keys that were never ours,
/// which is exactly what someone who can publish to the artifact host holds.
#[test]
fn a_repository_signed_by_someone_else_is_refused() {
    let theirs = fixtures().join("impostor");

    // Against their own root it is a perfectly good repository, and the bytes
    // they serve are the ones we would have served, so nothing about the target
    // gives them away.
    let bytes = fetch(&theirs.join("root.json"), &theirs)
        .expect("their repository is well formed");
    assert_eq!(String::from_utf8_lossy(&bytes), CONTENTS);

    refused(
        fetch(&fixtures().join("repo/root.json"), &theirs),
        "signature threshold",
    );
}

/// Freshness, which a digest also cannot answer: metadata signed by the right
/// keys and long out of date. Replaying an old snapshot is how someone keeps a
/// machine on the version they already know how to break.
#[test]
fn metadata_that_has_expired_is_refused() {
    let stale = fixtures().join("stale");
    refused(fetch(&stale.join("root.json"), &stale), "expired");
}

/// The archive path: written as it arrives rather than held in memory, because
/// a runtime is hundreds of megabytes.
#[test]
fn a_target_can_be_written_out_as_it_arrives() {
    let repo = fixtures().join("repo");
    let (store, trusted) = open(&repo.join("root.json"), &repo).expect("open");
    let into = store.path().join("archive");

    let mut seen = Vec::new();
    let written = trusted
        .fetch(TARGET, &into, &mut |so_far| seen.push(so_far))
        .expect("fetch");

    assert_eq!(fs::read(&into).expect("written file"), CONTENTS.as_bytes());
    assert_eq!(written, CONTENTS.len() as u64);
    assert_eq!(
        seen.last().copied(),
        Some(CONTENTS.len() as u64),
        "progress should end at the whole thing"
    );
}

#[test]
fn a_target_the_repository_does_not_vouch_for_is_not_fetched() {
    let repo = fixtures().join("repo");
    let (_store, trusted) = open(&repo.join("root.json"), &repo).expect("open");

    assert_eq!(trusted.names(), vec![TARGET.to_string()]);
    let complaint = trusted.read("something-else.tar.gz").expect_err("no such target");
    assert!(
        complaint.contains("not in this repository"),
        "unhelpful about a target that is not there: {complaint}"
    );
}
