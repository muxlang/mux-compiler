use crate::{Context, Result, receipt};
use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const API: &str = "https://api.github.com/repos/muxlang/mux-compiler/releases/latest";
const MAX_METADATA: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
#[derive(Deserialize)]
struct Metadata {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}
pub(crate) struct Release {
    pub version: Version,
    assets: Vec<Asset>,
}

fn validate_url(url: &str, fixtures: bool) -> Result<()> {
    let uri: ureq::http::Uri = url.parse().context("Invalid release URL")?;
    if uri.scheme_str() == Some("https") {
        return Ok(());
    }
    if fixtures
        && uri.scheme_str() == Some("http")
        && matches!(
            uri.host(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
    {
        return Ok(());
    }
    Err("Release URLs must use HTTPS (HTTP is allowed only for loopback test fixtures)".into())
}
fn fixture_mode() -> bool {
    std::env::var_os("MUX_UPDATE_API_URL").is_some()
}
const DOWNLOAD_WORKER: &str = "__update-download";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const CANCEL_POLL: Duration = Duration::from_millis(20);

pub fn run_download_worker(arguments: &[OsString]) -> Option<Result<()>> {
    if arguments
        .first()
        .is_none_or(|argument| argument != DOWNLOAD_WORKER)
    {
        return None;
    }
    Some((|| {
        if arguments.len() != 5 {
            return Err("Invalid download worker arguments".into());
        }
        let url = arguments[1].to_str().ok_or("Invalid download URL")?;
        let limit: u64 = arguments[2]
            .to_str()
            .ok_or("Invalid download limit")?
            .parse()
            .context("Invalid download limit")?;
        if limit > MAX_ARCHIVE {
            return Err("Invalid download limit".into());
        }
        let fixtures = match arguments[3].to_str() {
            Some("true") => true,
            Some("false") => false,
            _ => return Err("Invalid download fixture mode".into()),
        };
        let bytes = fetch_with(url, limit, fixtures)?;
        fs::write(Path::new(&arguments[4]), bytes).context("Write downloaded release data")
    })())
}

struct DownloadChild(Child);
impl Drop for DownloadChild {
    fn drop(&mut self) {
        // Reap before deleting staging files, including on cancellation and errors.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    crate::cancellation::check()?;
    let directory = tempfile::tempdir().context("Create download staging directory")?;
    let output = directory.path().join("download");
    let errors = directory.path().join("errors");
    let stderr = fs::File::create(&errors).context("Create download error log")?;
    let executable = std::env::current_exe().context("Locate download worker")?;
    // Keep blocking HTTP reads in a child so this process can poll cancellation.
    // Dropping the guard kills and reaps that child, closing its request promptly.
    let child = Command::new(executable)
        .arg(DOWNLOAD_WORKER)
        .arg(url)
        .arg(limit.to_string())
        .arg(fixture_mode().to_string())
        .arg(&output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("Start release download")?;
    let mut child = DownloadChild(child);
    let started = Instant::now();
    loop {
        crate::cancellation::check()?;
        if let Some(status) = child.0.try_wait().context("Wait for release download")? {
            if !status.success() {
                let error = fs::read_to_string(&errors).context("Read download error log")?;
                return Err(format!(
                    "Release download failed ({status}): {}",
                    error.trim()
                ));
            }
            let file = fs::File::open(&output).context("Open downloaded release data")?;
            let mut bytes = Vec::new();
            Interruptible(file)
                .take(limit + 1)
                .read_to_end(&mut bytes)
                .context("Read downloaded release data")?;
            crate::cancellation::check()?;
            if bytes.len() as u64 > limit {
                return Err("Release download exceeds size limit".into());
            }
            return Ok(bytes);
        }
        if started.elapsed() >= DOWNLOAD_TIMEOUT {
            return Err("Release download timed out".into());
        }
        std::thread::sleep(CANCEL_POLL);
    }
}
fn fetch_with(url: &str, limit: u64, fixtures: bool) -> Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .max_redirects(0)
        .build();
    let agent: ureq::Agent = config.into();
    let mut current = url.to_owned();
    for _ in 0..=5 {
        validate_url(&current, fixtures)?;
        let mut response = agent
            .get(&current)
            .header("User-Agent", "mux-update")
            .header("Accept", "application/vnd.github+json")
            .call()
            .context("Download release data")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .ok_or("Release redirect has no Location")?
                .to_str()
                .context("Invalid redirect Location")?;
            current = resolve_redirect(&current, location)?;
            continue;
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .context("Read release data")?;
        if bytes.len() as u64 > limit {
            return Err("Release download exceeds size limit".into());
        }
        return Ok(bytes);
    }
    Err("Too many release download redirects".into())
}
fn resolve_redirect(current: &str, location: &str) -> Result<String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.into());
    }
    let uri: ureq::http::Uri = current.parse().context("Invalid current release URL")?;
    let origin = format!(
        "{}://{}",
        uri.scheme_str().ok_or("Missing release scheme")?,
        uri.authority().ok_or("Missing release host")?
    );
    if location.starts_with("//") {
        return Ok(format!(
            "{}:{location}",
            uri.scheme_str().ok_or("Missing release scheme")?
        ));
    }
    if location.starts_with('/') {
        return Ok(format!("{origin}{location}"));
    }
    let parent = uri.path().rsplit_once('/').map_or("", |(parent, _)| parent);
    Ok(format!("{origin}{parent}/{location}"))
}
pub(crate) fn latest() -> Result<Release> {
    let url = std::env::var("MUX_UPDATE_API_URL").unwrap_or_else(|_| API.into());
    parse_release(&fetch(&url, MAX_METADATA)?)
}
fn parse_release(bytes: &[u8]) -> Result<Release> {
    let metadata: Metadata =
        serde_json::from_slice(bytes).context("Invalid GitHub release metadata")?;
    let version = Version::parse(
        metadata
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&metadata.tag_name),
    )
    .context("Invalid release version")?;
    if metadata.draft || metadata.prerelease || !version.pre.is_empty() {
        return Err("GitHub latest release is not a stable release".into());
    }
    Ok(Release {
        version,
        assets: metadata.assets,
    })
}

fn asset<'a>(release: &'a Release, name: &str) -> Result<&'a str> {
    let mut matches = release.assets.iter().filter(|a| a.name == name);
    let first = matches
        .next()
        .ok_or_else(|| format!("Release has no asset {name}"))?;
    if matches.next().is_some() {
        return Err(format!("Release has duplicate asset {name}"));
    }
    Ok(&first.browser_download_url)
}
pub(crate) fn stage(release: &Release, target: &str, directory: &Path) -> Result<PathBuf> {
    receipt::validate_target(target)?;
    let extension = if target.starts_with("windows-") {
        "zip"
    } else {
        "tar.gz"
    };
    let name = format!("mux-{target}.{extension}");
    let archive = fetch(asset(release, &name)?, MAX_ARCHIVE)?;
    let checksum = fetch(asset(release, &format!("{name}.sha256"))?, 4096)?;
    verify_checksum(&archive, &checksum)?;
    extract(&archive, extension, directory, target)?;
    let bundle = directory.join(format!("mux-{target}"));
    receipt::bundle_files(&bundle)?;
    Ok(bundle)
}
fn verify_checksum(archive: &[u8], checksum: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(checksum).context("Invalid checksum encoding")?;
    let expected = text.split_whitespace().next().ok_or("Empty checksum")?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid SHA-256 checksum".into());
    }
    let mut digest = Sha256::new();
    for chunk in archive.chunks(64 * 1024) {
        crate::cancellation::check()?;
        digest.update(chunk);
    }
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("Checksum verification failed; installation was not changed".into());
    }
    Ok(())
}
fn safe_path(path: &Path, target: &str, directory: bool) -> Result<()> {
    let parts: Vec<_> = path.components().collect();
    if parts.iter().any(|p| !matches!(p, Component::Normal(_)))
        || path.to_string_lossy().contains(['\\', ':'])
    {
        return Err("Unsafe archive path".into());
    }
    let root = format!("mux-{target}");
    if parts.first().map(|p| p.as_os_str()) != Some(std::ffi::OsStr::new(&root)) {
        return Err("Unexpected archive root".into());
    }
    if parts.len() >= 2 && !matches!(parts[1].as_os_str().to_str(), Some("bin" | "lib")) {
        return Err("Unexpected archive layout".into());
    }
    if parts.len() > 3 || (!directory && parts.len() != 3) {
        return Err("Unexpected archive layout".into());
    }
    if let Some(name) = parts.get(2) {
        receipt::validate_name(name.as_os_str().to_str().ok_or("Non-UTF8 archive path")?)?;
    }
    Ok(())
}
fn extract(bytes: &[u8], extension: &str, directory: &Path, target: &str) -> Result<()> {
    if extension == "zip" {
        extract_zip(bytes, directory, target)
    } else {
        extract_tar(bytes, directory, target)
    }
}
struct Interruptible<R>(R);
impl<R: Read> Read for Interruptible<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        crate::cancellation::check().map_err(std::io::Error::other)?;
        let length = buffer.len().min(64 * 1024);
        self.0.read(&mut buffer[..length])
    }
}

fn extract_tar(bytes: &[u8], directory: &Path, target: &str) -> Result<()> {
    let mut archive = tar::Archive::new(Interruptible(GzDecoder::new(bytes)));
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for entry in archive.entries().context("Read archive")? {
        crate::cancellation::check()?;
        let mut entry = entry.context("Read archive entry")?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("Archive contains a link or special file".into());
        }
        let path = entry.path().context("Read archive path")?.into_owned();
        safe_path(&path, target, kind.is_dir())?;
        if !seen.insert(path.clone()) {
            return Err("Archive contains duplicate paths".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Archive size overflow")?;
        if total > MAX_ARCHIVE * 4 {
            return Err("Expanded archive exceeds size limit".into());
        }
        if !entry
            .unpack_in(directory)
            .context("Extract archive entry")?
        {
            return Err("Unsafe archive entry".into());
        }
    }
    Ok(())
}
fn extract_zip(bytes: &[u8], directory: &Path, target: &str) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("Read ZIP archive")?;
    let mut total = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        crate::cancellation::check()?;
        let mut entry = archive.by_index(index).context("Read ZIP entry")?;
        let file_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if entry.is_symlink() || !matches!(file_type, 0 | 0o100000 | 0o040000) {
            return Err("Archive contains a link or special file".into());
        }
        let path = PathBuf::from(entry.name().context("Read ZIP entry name")?.as_ref());
        safe_path(&path, target, entry.is_dir())?;
        if !seen.insert(path.clone()) {
            return Err("Archive contains duplicate paths".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Archive size overflow")?;
        if total > MAX_ARCHIVE * 4 {
            return Err("Expanded archive exceeds size limit".into());
        }
        let destination = directory.join(path);
        if entry.is_dir() {
            fs::create_dir_all(&destination).context("Create ZIP directory")?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).context("Create ZIP directory")?;
            }
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .context("Create ZIP output")?;
            std::io::copy(&mut Interruptible(&mut entry), &mut output)
                .context("Extract ZIP file")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_is_mandatory() {
        assert!(verify_checksum(b"hello", b"bad").is_err());
        assert!(
            verify_checksum(
                b"hello",
                format!("{:x} file", Sha256::digest(b"hello")).as_bytes()
            )
            .is_ok()
        );
        assert!(
            verify_checksum(
                b"changed",
                format!("{:x}", Sha256::digest(b"hello")).as_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn archive_paths_are_confined() {
        for path in [
            "../escape",
            "/mux-linux-x86_64/bin/mux",
            "mux-linux-x86_64/bin/../lib/mux",
            "mux-linux-x86_64/bin/a/b",
            "mux-linux-x86_64/bin/a\\b",
            "other/bin/mux",
        ] {
            assert!(
                safe_path(Path::new(path), "linux-x86_64", false).is_err(),
                "{path}"
            );
        }
        assert!(safe_path(Path::new("mux-linux-x86_64/bin/mux"), "linux-x86_64", false).is_ok());
    }
    #[test]
    fn http_requires_loopback_fixture() {
        assert!(validate_url("http://127.0.0.1:8000/latest", true).is_ok());
        assert!(validate_url("http://127.0.0.1.evil/latest", true).is_err());
        assert!(validate_url("http://127.0.0.1/latest", false).is_err());
        assert!(validate_url("http://example.com/latest", true).is_err());
    }
    fn serve(body: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/latest", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (url, thread)
    }
    #[test]
    fn local_metadata_selects_exact_asset_and_stable_version() {
        let body = serde_json::json!({"tag_name":"v0.15.0", "draft":false,"prerelease":false,"assets":[{"name":"mux-linux-x86_64.tar.gz","browser_download_url":"https://example.com/precise-release/archive"}]}).to_string();
        let (url, thread) = serve(body.into_bytes());
        let release = parse_release(&fetch_with(&url, MAX_METADATA, true).unwrap()).unwrap();
        thread.join().unwrap();
        assert_eq!(release.version, Version::new(0, 15, 0));
        assert_eq!(
            asset(&release, "mux-linux-x86_64.tar.gz").unwrap(),
            "https://example.com/precise-release/archive"
        );
        assert!(asset(&release, "mux-windows-x86_64.zip").is_err());
        assert!(Version::new(0, 14, 0) < release.version);
        assert!(Version::new(0, 15, 0) >= release.version);
        assert!(Version::new(0, 16, 0) >= release.version);
    }
    #[test]
    fn rejects_unstable_or_invalid_metadata() {
        for (tag, draft, pre) in [
            ("v1.0.0", true, false),
            ("v1.0.0", false, true),
            ("v1.0.0-beta.1", false, false),
            ("garbage", false, false),
        ] {
            let body =
                serde_json::json!({"tag_name":tag,"draft":draft,"prerelease":pre,"assets":[]});
            assert!(parse_release(body.to_string().as_bytes()).is_err());
        }
    }
    #[test]
    fn local_download_is_bounded() {
        let (url, thread) = serve(vec![0; 64]);
        assert!(fetch_with(&url, 16, true).is_err());
        thread.join().unwrap();
    }
    #[test]
    fn extraction_rejects_tar_links_and_zip_traversal() {
        let stage = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder
            .append_link(&mut header, "mux-linux-x86_64/bin/mux", "/tmp/escape")
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gzip, &bytes).unwrap();
        assert!(extract_tar(&gzip.finish().unwrap(), stage.path(), "linux-x86_64").is_err());
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../escape", zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, b"unsafe").unwrap();
        assert!(
            extract_zip(
                &writer.finish().unwrap().into_inner(),
                stage.path(),
                "linux-x86_64"
            )
            .is_err()
        );
    }
    fn redirect(location: String) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/redirect", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(stream,"HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        (url, thread)
    }
    #[test]
    fn redirects_follow_loopback_and_reject_external_http_before_request() {
        let (destination, server) = serve(b"release".to_vec());
        let (url, redirect_server) = redirect(destination);
        assert_eq!(fetch_with(&url, 1024, true).unwrap(), b"release");
        redirect_server.join().unwrap();
        server.join().unwrap();
        let (url, server) = redirect("http://example.invalid/release".into());
        let error = fetch_with(&url, 1024, true).unwrap_err();
        assert!(error.contains("Release URLs must use HTTPS"), "{error}");
        server.join().unwrap();
    }
    #[test]
    fn extracts_regular_tar_and_zip_files_and_rejects_duplicates_and_links() {
        use std::io::Write;
        let file = "mux-linux-x86_64/bin/mux";
        for duplicate in [false, true] {
            let stage = tempfile::tempdir().unwrap();
            let mut builder = tar::Builder::new(Vec::new());
            for _ in 0..if duplicate { 2 } else { 1 } {
                let mut header = tar::Header::new_gnu();
                header.set_size(7);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, file, &b"payload"[..])
                    .unwrap();
            }
            let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gzip.write_all(&builder.into_inner().unwrap()).unwrap();
            let result = extract_tar(&gzip.finish().unwrap(), stage.path(), "linux-x86_64");
            if duplicate {
                assert!(result.unwrap_err().contains("duplicate"));
            } else {
                result.unwrap();
                assert_eq!(fs::read(stage.path().join(file)).unwrap(), b"payload");
            }
        }
        for link in [false, true] {
            let stage = tempfile::tempdir().unwrap();
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            if link {
                writer
                    .add_symlink(file, "/escape", zip::write::SimpleFileOptions::default())
                    .unwrap();
            } else {
                writer
                    .add_directory(
                        "mux-linux-x86_64/bin/",
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
                writer
                    .start_file(file, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"payload").unwrap();
            }
            let result = extract_zip(
                &writer.finish().unwrap().into_inner(),
                stage.path(),
                "linux-x86_64",
            );
            if link {
                assert!(result.unwrap_err().contains("link or special"));
            } else {
                result.unwrap();
                assert_eq!(fs::read(stage.path().join(file)).unwrap(), b"payload");
            }
        }
    }

    #[test]
    fn metadata_rejects_duplicate_assets_and_checksum_encoding_errors() {
        let metadata = br#"{"tag_name":"1.0.0","draft":false,"prerelease":false,"assets":[{"name":"bundle","browser_download_url":"https://example.com/one"},{"name":"bundle","browser_download_url":"https://example.com/two"}]}"#;
        let release = parse_release(metadata).unwrap();
        assert!(asset(&release, "bundle").unwrap_err().contains("duplicate"));
        assert!(verify_checksum(b"", b"").unwrap_err().contains("Empty"));
        assert!(
            verify_checksum(b"", &[255])
                .unwrap_err()
                .contains("encoding")
        );
        assert!(parse_release(b"invalid").is_err());
    }

    #[test]
    fn relative_redirects_preserve_scheme_and_origin() {
        for (location, expected) in [
            ("/file", "https://example.com/file"),
            ("file", "https://example.com/releases/file"),
            ("//cdn.example.com/file", "https://cdn.example.com/file"),
            (
                "https://cdn.example.com/file",
                "https://cdn.example.com/file",
            ),
        ] {
            assert_eq!(
                resolve_redirect("https://example.com/releases/latest", location).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn worker_requires_exact_mode_and_valid_bounded_arguments() {
        assert!(run_download_worker(&[]).is_none());
        assert!(run_download_worker(&["other".into()]).is_none());
        for args in [
            vec![DOWNLOAD_WORKER],
            vec![
                DOWNLOAD_WORKER,
                "https://example.com",
                "invalid",
                "false",
                "unused",
            ],
            vec![
                DOWNLOAD_WORKER,
                "https://example.com",
                "1099511627776",
                "false",
                "unused",
            ],
            vec![
                DOWNLOAD_WORKER,
                "https://example.com",
                "16",
                "invalid",
                "unused",
            ],
            vec![
                DOWNLOAD_WORKER,
                "http://example.com",
                "16",
                "false",
                "unused",
            ],
        ] {
            assert!(
                run_download_worker(&args.into_iter().map(OsString::from).collect::<Vec<_>>())
                    .unwrap()
                    .is_err()
            );
        }
    }
}
