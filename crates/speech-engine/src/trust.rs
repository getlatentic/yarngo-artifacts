//! Who published a runtime, and whether what we were handed is current.
//!
//! A runtime is code this application runs, so a digest is not enough: it says
//! the bytes match what the manifest claimed, and says nothing about who wrote
//! the manifest. That question — along with freshness, rollback, expiry and key
//! rotation — is The Update Framework's, and is answered here by a TUF client
//! rather than by anything invented in this repository.
//!
//! What stays ours is how bytes arrive. Fetching goes through the same `curl`
//! the rest of the installer uses, so proxies, retries and certificate handling
//! keep behaving the way they already do on the machines this runs on.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tough::{
    Limits, RepositoryLoader, TargetName, Transport, TransportError, TransportErrorKind,
    TransportStream,
};
use url::Url;

/// Where trust starts and what it covers.
pub struct Anchor {
    /// The root role, shipped inside the application and therefore covered by
    /// its signature. Trust begins here or it does not begin.
    pub root: Vec<u8>,
    pub metadata: Url,
    pub targets: Url,
    /// Metadata already seen, kept between runs. Without somewhere durable to
    /// remember versions, being handed last year's snapshot again is invisible.
    pub datastore: PathBuf,
}

/// A repository that has verified itself against the anchor.
pub struct Trusted {
    repository: tough::Repository,
    /// TUF's client is asynchronous and the installer is not. Rather than
    /// colour the installer, the runtime lives here and is driven a call at a
    /// time.
    executor: tokio::runtime::Runtime,
}

impl Anchor {
    /// Load and verify the repository's metadata. Failure here means the
    /// repository is not ours, not current, or not intact — in all three cases
    /// there is nothing to install.
    pub fn open(self) -> Result<Trusted, String> {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| format!("could not start the update client: {e}"))?;

        std::fs::create_dir_all(&self.datastore)
            .map_err(|e| format!("could not prepare {}: {e}", self.datastore.display()))?;

        let repository = executor.block_on(
            RepositoryLoader::new(&self.root, self.metadata, self.targets)
                .transport(Curl)
                .limits(Limits::default())
                .datastore(self.datastore)
                .load(),
        )
        .map_err(|e| format!("{e}"))?;

        Ok(Trusted {
            repository,
            executor,
        })
    }
}

impl Trusted {
    /// A small target, such as the catalogue of runtimes on offer.
    pub fn read(&self, target: &str) -> Result<Vec<u8>, String> {
        let name = TargetName::new(target).map_err(|e| e.to_string())?;
        self.executor.block_on(async {
            let stream = self
                .repository
                .read_target(&name)
                .await
                .map_err(|e| format!("{e}"))?
                .ok_or_else(|| format!("{target} is not in this repository"))?;
            tough::IntoVec::into_vec(stream)
                .await
                .map_err(|e| format!("{e}"))
        })
    }

    /// A large target, such as a runtime archive, written as it arrives.
    ///
    /// The stream hands bytes over before the digest across the whole of it
    /// can be checked, and TUF's client says plainly not to use them if the
    /// stream then fails. So nothing appears at `into` until the stream has
    /// ended and every check has passed: the bytes accumulate in a temporary
    /// file the operating system named, and a failure anywhere drops it. A
    /// crash leaves at most an unreferenced temporary, never a plausible
    /// archive at the destination.
    pub fn fetch(
        &self,
        target: &str,
        into: &Path,
        report: &mut dyn FnMut(u64),
    ) -> Result<u64, String> {
        use futures_core::Stream;
        use std::io::Write;
        use std::pin::Pin;

        let name = TargetName::new(target).map_err(|e| e.to_string())?;
        let beside = into
            .parent()
            .ok_or_else(|| format!("{} has nowhere to live", into.display()))?;
        std::fs::create_dir_all(beside)
            .map_err(|e| format!("could not prepare {}: {e}", beside.display()))?;
        // In the destination's own directory, so promoting it is a rename on
        // one filesystem rather than a copy that could be caught half-done.
        let holding = tempfile::NamedTempFile::new_in(beside)
            .map_err(|e| format!("could not write beside {}: {e}", into.display()))?;

        let written = self.executor.block_on(async {
            let mut stream: Pin<Box<dyn Stream<Item = _> + Send + Sync>> = Box::pin(
                self.repository
                    .read_target(&name)
                    .await
                    .map_err(|e| format!("{e}"))?
                    .ok_or_else(|| format!("{target} is not in this repository"))?,
            );
            let mut file = holding.as_file();
            let mut written = 0u64;
            while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
                let chunk = chunk.map_err(|e| format!("{e}"))?;
                file.write_all(&chunk)
                    .map_err(|e| format!("could not write {}: {e}", into.display()))?;
                written += chunk.len() as u64;
                report(written);
            }
            file.flush()
                .map_err(|e| format!("could not write {}: {e}", into.display()))?;
            Ok::<u64, String>(written)
        })?;

        // Only a stream that ended cleanly reaches this line, and ending
        // cleanly is what "the hashes matched" means to the client.
        holding
            .persist(into)
            .map_err(|e| format!("could not finish {}: {e}", into.display()))?;
        Ok(written)
    }

    /// The targets this repository vouches for, for diagnosis when a name we
    /// expected is not among them.
    pub fn names(&self) -> Vec<String> {
        self.repository
            .targets()
            .signed
            .targets
            .keys()
            .map(|name| name.raw().to_string())
            .collect()
    }
}

/// `curl`, wearing the interface TUF's client expects.
#[derive(Debug, Clone)]
struct Curl;

/// Enough for any runtime we would publish, and far short of filling a disk.
/// The exact limit for each metadata role is TUF's, applied to the stream; this
/// only stops a hostile server from answering a small request with a large one.
const CEILING: &str = "2147483648";

/// Read back in pieces, so that a runtime archive is never held whole in memory.
const CHUNK: usize = 64 * 1024;

#[tough::async_trait]
impl Transport for Curl {
    async fn fetch(&self, url: Url) -> Result<TransportStream, TransportError> {
        let complain = |kind, why: String| {
            TransportError::new_with_cause(kind, url.as_str(), std::io::Error::other(why))
        };

        let (body, code) = run(&url).map_err(|(kind, why)| complain(kind, why))?;
        match status(code) {
            Verdict::Fetched => {}
            Verdict::Missing => {
                return Err(TransportError::new(
                    TransportErrorKind::FileNotFound,
                    url.as_str(),
                ))
            }
            Verdict::Refused(why) => return Err(complain(TransportErrorKind::Other, why)),
        }

        Ok(read_back(body, url))
    }
}

/// What a fetch that reached the server amounts to.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Fetched,
    /// Absence is its own answer: a client asking whether a newer role exists
    /// deserves "no" rather than "something went wrong".
    Missing,
    Refused(String),
}

/// `None` is a scheme that has no statuses, such as `file`.
fn status(code: Option<u16>) -> Verdict {
    match code {
        None => Verdict::Fetched,
        Some(404 | 410) => Verdict::Missing,
        Some(code) if code >= 300 => Verdict::Refused(format!("the server answered {code}")),
        Some(code) if code >= 400 => Verdict::Refused(format!("the server answered {code}")),
        
        // A redirect here means curl was told to follow them and did not, so it
        // is no more a body than an error page is.

        Some(_) => Verdict::Fetched,
    }
}

/// The downloaded file as a stream, deleted once the stream is dropped.
fn read_back(body: tempfile::NamedTempFile, url: Url) -> TransportStream {
    use std::io::Read;

    let start = std::fs::File::open(body.path())
        .map_err(|e| (e, url.clone()))
        .map(|file| (file, body));

    Box::pin(futures_util::stream::unfold(
        Some(start),
        move |state| {
            let url = url.clone();
            async move {
                let mut open = match state? {
                    Ok(open) => open,
                    Err((e, url)) => {
                        let kind = TransportErrorKind::Other;
                        return Some((Err(TransportError::new_with_cause(kind, url, e)), None));
                    }
                };

                let mut buffer = vec![0u8; CHUNK];
                match open.0.read(&mut buffer) {
                    Ok(0) => None,
                    Ok(read) => {
                        buffer.truncate(read);
                        Some((Ok(tough::Bytes::from(buffer)), Some(Ok(open))))
                    }
                    Err(e) => {
                        let kind = TransportErrorKind::Other;
                        Some((Err(TransportError::new_with_cause(kind, url, e)), None))
                    }
                }
            }
        },
    ))
}

/// Downloads to a temporary file and reports the status, if the scheme has one.
/// `curl` is asked not to fail on a status so that the status is ours to read.
fn run(url: &Url) -> Result<(tempfile::NamedTempFile, Option<u16>), (TransportErrorKind, String)> {
    if !matches!(url.scheme(), "https" | "file") {
        return Err((
            TransportErrorKind::UnsupportedUrlScheme,
            format!("{} is not a scheme we fetch over", url.scheme()),
        ));
    }

    // Named by the operating system rather than by us, because two fetches at
    // once must not be able to read each other's bytes.
    let into = tempfile::NamedTempFile::new()
        .map_err(|e| (TransportErrorKind::Other, e.to_string()))?;

    let output = Command::new("curl")
        .args(["-sS", "-L", "--retry", "2", "--max-time", "600"])
        .args(["--max-filesize", CEILING])
        .args(["-w", "%{http_code}"])
        .arg("-o")
        .arg(into.path())
        .arg(url.as_str())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| (TransportErrorKind::Other, format!("could not run curl: {e}")))?;

    if !output.status.success() {
        // 37 is curl declining to open a local file, which for a `file://` URL
        // is the same news as a 404.
        let kind = match output.status.code() {
            Some(37) => TransportErrorKind::FileNotFound,
            _ => TransportErrorKind::Other,
        };
        let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err((
            kind,
            if why.is_empty() { "download failed".into() } else { why },
        ));
    }

    // A status of 000 is what curl reports for schemes that have none.
    let code = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|code| *code != 0);
    Ok((into, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport() -> impl Transport {
        Curl
    }

    fn drive<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    fn at(path: &Path) -> Url {
        Url::from_file_path(path).expect("test paths are absolute")
    }

    #[test]
    fn a_file_that_is_there_comes_back_whole() {
        let scratch = tempfile::tempdir().expect("temp dir");
        let path = scratch.path().join("present");
        std::fs::write(&path, b"the bytes").expect("write");

        let stream = drive(transport().fetch(at(&path))).expect("fetch");
        let bytes = drive(tough::IntoVec::into_vec(stream)).expect("collect");
        assert_eq!(bytes, b"the bytes");
    }

    /// A diagnosis that says the file is missing beats one that says the
    /// download failed.
    #[test]
    fn a_file_that_is_not_there_is_reported_as_missing() {
        let scratch = tempfile::tempdir().expect("temp dir");
        let outcome = drive(transport().fetch(at(&scratch.path().join("absent"))));
        assert_eq!(
            outcome.err().map(|e| e.kind()),
            Some(TransportErrorKind::FileNotFound)
        );
    }

    /// Metadata and archives arrive over a channel we can authenticate or they
    /// do not arrive. TUF would still catch tampering, but there is no reason
    /// to hand anyone the chance to watch or to interfere.
    /// The branch a `file://` fixture can never reach, and the one that
    /// matters in the field: whether a status means a body, an absence, or a
    /// refusal.
    #[test]
    fn a_status_says_which_of_the_three_things_happened() {
        assert_eq!(status(None), Verdict::Fetched, "a scheme without statuses");
        assert_eq!(status(Some(200)), Verdict::Fetched);
        assert_eq!(status(Some(404)), Verdict::Missing);
        assert_eq!(status(Some(410)), Verdict::Missing);

        for code in [301, 302, 400, 403, 429, 500, 503] {
            assert!(
                matches!(status(Some(code)), Verdict::Refused(_)),
                "{code} was treated as a body"
            );
        }
    }

    /// A runtime archive is hundreds of megabytes, so the downloaded file is
    /// read back in pieces rather than handed over whole.
    #[test]
    fn a_large_file_arrives_in_pieces() {
        let body = tempfile::NamedTempFile::new().expect("temp file");
        let contents: Vec<u8> = (0..CHUNK * 2 + 7).map(|i| (i % 251) as u8).collect();
        std::fs::write(body.path(), &contents).expect("write");
        let at = Url::from_file_path(body.path()).expect("absolute");

        let pieces = drive(async {
            let mut stream = read_back(body, at);
            let mut pieces = Vec::new();
            while let Some(piece) = futures_util::StreamExt::next(&mut stream).await {
                pieces.push(piece.expect("piece"));
            }
            pieces
        });

        assert!(pieces.len() > 2, "handed over in one go: {} piece(s)", pieces.len());
        assert_eq!(pieces.concat(), contents);
    }

    #[test]
    fn plain_http_is_not_a_channel_we_fetch_over() {
        for url in ["http://example.invalid/root.json", "ftp://example.invalid/x"] {
            let outcome = drive(transport().fetch(Url::parse(url).expect("url")));
            assert_eq!(
                outcome.err().map(|e| e.kind()),
                Some(TransportErrorKind::UnsupportedUrlScheme),
                "{url} should not be fetched over"
            );
        }
    }
}
