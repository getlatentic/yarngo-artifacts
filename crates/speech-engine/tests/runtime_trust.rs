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

use speech_engine::catalogue::Catalogue;
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

/// Targets are stored under a name beginning with their digest, so a test
/// names the one it means by its suffix.
fn target_file(dir: &Path, ending: &str) -> PathBuf {
    let mut found: Vec<_> = fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|path| path.to_string_lossy().ends_with(ending))
        .collect();
    assert_eq!(found.len(), 1, "expected one {ending} in {}", dir.display());
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

    flip_a_byte(&target_file(&repo.join("targets"), TARGET));
    refused(fetch(&repo.join("root.json"), &repo), "hash mismatch");
}

#[test]
fn metadata_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    fetch(&repo.join("root.json"), &repo).expect("the untouched copy should load");

    flip_a_byte(&target_file(&repo.join("metadata"), "targets.json"));
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

    let vouched = trusted.names();
    assert!(
        vouched.contains(&Catalogue::TARGET.to_string()),
        "the catalogue is not among {vouched:?}"
    );
    assert!(
        !vouched.iter().any(|name| name == "something-else.tar.gz"),
        "the repository claims to vouch for a target that was never published"
    );
    let complaint = trusted.read("something-else.tar.gz").expect_err("no such target");
    assert!(
        complaint.contains("not in this repository"),
        "unhelpful about a target that is not there: {complaint}"
    );
}

/// The whole shape, end to end: trust the repository, read what it offers,
/// choose. The catalogue is a target like any other, so nothing it says about
/// versions rests on anything weaker than the archives it names.
#[test]
fn what_to_install_is_read_from_a_catalogue_the_repository_vouches_for() {
    let repo = fixtures().join("repo");
    let (_store, trusted) = open(&repo.join("root.json"), &repo).expect("open");

    let offered = Catalogue::read(&trusted.read(Catalogue::TARGET).expect("catalogue"))
        .expect("readable");

    // 2.0.0 is there and is newer, and wants an application nine major versions
    // away, so the answer is the newest one that actually fits.
    let chosen = offered.best("mlx", "0.1.0-alpha.1").expect("a release");
    assert_eq!(chosen.version, "1.0.0");
    assert_eq!(chosen.lock, "mlx-1.0.0.uv.lock");

    let later = offered.best("mlx", "9.0.0").expect("a release");
    assert_eq!(later.version, "2.0.0");
}

/// A catalogue edited after signing is the interesting one: it is the document
/// that decides which code gets installed.
#[test]
fn a_catalogue_edited_after_signing_is_refused() {
    let (_scratch, repo) = copy_of("repo");
    let (store, trusted) = open(&repo.join("root.json"), &repo).expect("open");
    trusted.read(Catalogue::TARGET).expect("the untouched copy");
    drop((store, trusted));

    flip_a_byte(&target_file(&repo.join("targets"), Catalogue::TARGET));

    let (_store, trusted) = open(&repo.join("root.json"), &repo).expect("open");
    refused(trusted.read(Catalogue::TARGET), "hash mismatch");
}

/// The whole wiring, from a signed repository to files on disk that `uv` and
/// the engine loader will read.
#[test]
fn a_release_is_staged_from_the_repository_that_vouched_for_it() {
    let repo = fixtures().join("repo");
    let scratch = tempfile::tempdir().expect("temp dir");
    let anchor = speech_engine::published::at(
        directory(&repo).as_str(),
        fs::read(repo.join("root.json")).expect("root"),
        scratch.path().join("seen"),
    )
    .expect("anchor");

    let published = speech_engine::published::Published::offered_by(anchor, "mlx", "0.1.0-alpha.1")
        .expect("a release");
    assert_eq!(published.version(), "1.0.0");

    let project = scratch.path().join("project");
    assert!(published.stage_recipe(&project).expect("stage"), "nothing changed");
    assert!(
        fs::read_to_string(project.join("uv.lock")).expect("lock").contains("requires-python"),
        "the lock is not the published one"
    );
    assert!(
        fs::read_to_string(project.join("pyproject.toml"))
            .expect("pyproject")
            .contains("mlx-runtime"),
        "the pyproject is not the published one"
    );

    // Staging the same release again changes nothing, so an install already on
    // it says nothing rather than announcing an update that did not happen.
    assert!(!published.stage_recipe(&project).expect("stage again"));

    let folder = scratch.path().join("runtime");
    assert!(published.stage_engine("mlx", &folder).expect("engine"), "no engine staged");
    assert_eq!(
        fs::read_to_string(folder.join("engine.py")).expect("engine.py"),
        "# the published engine\n"
    );
}

/// Nothing is fetched from a repository the shipped root role did not sign,
/// however well formed it is — which is the property the whole arrangement
/// exists for.
#[test]
fn a_release_is_not_staged_from_someone_elses_repository() {
    let scratch = tempfile::tempdir().expect("temp dir");
    let anchor = speech_engine::published::at(
        directory(&fixtures().join("impostor")).as_str(),
        fs::read(fixtures().join("repo/root.json")).expect("our root"),
        scratch.path().join("seen"),
    )
    .expect("anchor");

    let complaint =
        speech_engine::published::Published::offered_by(anchor, "mlx", "0.1.0-alpha.1")
            .err()
            .map(|e| e.to_lowercase())
            .expect("another publisher's repository was accepted");
    assert!(complaint.contains("signature threshold"), "{complaint}");
}

/// Fetches that overlap must not read each other's bytes.
///
/// They did, once: the download went to a file named for the process, so two at
/// once were the same file. The symptom was a hash mismatch on bytes that were
/// perfectly good — unexplainable from the message, and the sort of thing found
/// in the field rather than here. Fetching moved when TUF came in, so the risk
/// moved with it. Concurrent, because sequential fetches never collided and
/// sequential is what had been tested.
#[test]
fn overlapping_fetches_do_not_read_each_others_bytes() {
    let repo = fixtures().join("repo");
    let wanted = [
        (TARGET, CONTENTS),
        (Catalogue::TARGET, "\"schema\": 1"),
        ("mlx-1.0.0.uv.lock", "requires-python"),
        ("mlx-1.0.0.pyproject.toml", "mlx-runtime"),
    ];

    std::thread::scope(|scope| {
        for _ in 0..3 {
            for (target, expected) in wanted {
                let repo = repo.clone();
                scope.spawn(move || {
                    let (_store, trusted) = open(&repo.join("root.json"), &repo).expect("open");
                    let got = trusted.read(target).expect("read");
                    assert!(
                        String::from_utf8_lossy(&got).contains(expected),
                        "{target} came back as somebody else's bytes"
                    );
                });
            }
        }
    });
}

/// Being served last week's repository, by the publisher who signed it.
///
/// Every signature is genuine and nothing has expired, so nothing about the
/// metadata itself gives it away — the only thing that does is remembering what
/// was already seen. Replaying old metadata is how someone holds a machine on
/// the version they know how to break, and it needs no keys at all.
#[test]
fn metadata_older_than_what_was_already_seen_is_refused() {
    let store = tempfile::tempdir().expect("temp dir");
    let seen = store.path().join("seen");

    let anchor = |served: PathBuf| {
        speech_engine::published::at(
            directory(&served).as_str(),
            fs::read(fixtures().join("repo/root.json")).expect("root"),
            seen.clone(),
        )
        .expect("anchor")
        .open()
    };

    anchor(fixtures().join("repo")).expect("this week's repository");

    // The same keys, the same runtime, an earlier version of the metadata.
    let complaint = anchor(fixtures().join("rollback"))
        .err()
        .map(|why| why.to_lowercase())
        .expect("last week's repository was accepted");
    assert!(
        complaint.contains("previously fetched"),
        "refused, but not as a rollback: {complaint}"
    );
}
