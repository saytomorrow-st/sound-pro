//! Integration tests for the bank downloader (SPEC-067 §6).
//!
//! Every test serves the license source and the archive from a local
//! `http://127.0.0.1` server (no real internet), then drives
//! [`daw_app::bank_download::download_bank_into_with_https`] with
//! `https_required = false`. The production `download_bank` path always
//! enforces HTTPS; the HTTPS enforcement itself is unit-tested in
//! `bank_download.rs`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use daw_app::bank_download::{DownloadError, Progress, download_bank_into_with_https};
use daw_app::bank_library::{CatalogEntry, Download, License, LicenseCheck};
use sha2::{Digest, Sha256};

/// A tiny single-threaded HTTP server that serves fixed responses and stops
/// when dropped. Each request gets `handler(request_index, target)`.
struct TestServer {
    addr: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(handler: impl Fn(usize, &str) -> ServerResponse + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut index = 0usize;
            for stream in listener.incoming() {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 8192];
                let mut read = 0;
                loop {
                    let n = stream.read(&mut buf[read..]).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    read += n;
                    if read >= buf.len() || buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..read]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let resp = handler(index, target);
                index += 1;
                write_response(&mut stream, &resp);
            }
        });
        TestServer {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.addr, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct ServerResponse {
    status: u16,
    body: Vec<u8>,
    location: Option<String>,
}

impl ServerResponse {
    fn ok(body: Vec<u8>) -> Self {
        ServerResponse {
            status: 200,
            body,
            location: None,
        }
    }
    fn redirect(location: String) -> Self {
        ServerResponse {
            status: 302,
            body: Vec::new(),
            location: Some(location),
        }
    }
}

fn write_response(stream: &mut std::net::TcpStream, resp: &ServerResponse) {
    let reason = match resp.status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
        resp.status,
        reason,
        resp.body.len()
    );
    if let Some(loc) = &resp.location {
        head.push_str(&format!("Location: {loc}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

fn sha256(data: &[u8]) -> String {
    let mut s = String::with_capacity(64);
    for b in Sha256::digest(data) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn entry(url: String, license_url: String, format: &str, size: u64, hash: String) -> CatalogEntry {
    CatalogEntry {
        schema: 1,
        id: "test-bank".to_owned(),
        name: "Test Bank".to_owned(),
        family: "piano".to_owned(),
        version: "1".to_owned(),
        author: "Test Author".to_owned(),
        license: License {
            spdx: "CC0-1.0".to_owned(),
            url: "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
            attribution_required: false,
        },
        attribution: String::new(),
        source_url: url.clone(),
        sha256: Some(hash.clone()),
        size_bytes: Some(size),
        verified: true,
        license_check: Some(LicenseCheck {
            url: license_url,
            sha256: None,
            mode: "metadata".to_owned(),
        }),
        download: Some(Download {
            url,
            sha256: hash,
            size_bytes: size,
        }),
        sfz: None,
        format: format.to_owned(),
    }
}

fn cancel_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn temp_banks_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bank-download-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Helper: builds a zip in memory and returns its bytes.
fn make_zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap();
    }
    cursor.into_inner()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn success_downloads_verifies_and_installs() {
    let archive = make_zip_bytes(&[
        ("bank/bank.sfz", b"<region> sample=bank.wav"),
        ("bank/bank.wav", b"RIFFfake"),
    ]);
    let archive_hash = sha256(&archive);
    let archive_size = archive.len() as u64;

    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, target| {
        if idx == 0 {
            assert_eq!(target, "/license");
            ServerResponse::ok(license_body.clone())
        } else {
            assert_eq!(target, "/archive");
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let mut e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        archive_size,
        archive_hash,
    );
    e.sfz = Some("bank/bank.sfz".to_owned());

    let banks = temp_banks_dir("success");
    let progress = Progress::new(archive_size);
    let result = download_bank_into_with_https(&e, &banks, &progress, &cancel_flag(), false);
    let bank = result.expect("download succeeds");
    assert_eq!(bank.id, "test-bank");
    assert_eq!(progress.bytes(), archive_size);
    assert!(
        banks
            .join("test-bank")
            .join("bank")
            .join("bank.sfz")
            .exists()
    );
    assert!(banks.join("banks.json").exists());
}

#[test]
fn hash_mismatch_is_rejected() {
    let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
    let size = archive.len() as u64;
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        size,
        "a".repeat(64),
    );

    let banks = temp_banks_dir("hash-mismatch");
    let result =
        download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
    assert!(matches!(result, Err(DownloadError::HashMismatch)));
    assert!(!banks.join("test-bank").exists());
    assert!(!banks.join("banks.json").exists());
}

#[test]
fn license_mismatch_is_rejected() {
    let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
    let size = archive.len() as u64;
    let archive_hash = sha256(&archive);
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC-BY-3.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        size,
        archive_hash,
    );

    let banks = temp_banks_dir("license-mismatch");
    let result =
        download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
    assert!(matches!(result, Err(DownloadError::LicenseMismatch)));
    assert!(!banks.join("test-bank").exists());
}

#[test]
fn truncated_download_is_rejected() {
    let archive = make_zip_bytes(&[("bank/bank.sfz", b"full-contents")]);
    let declared_size = archive.len() as u64 + 100;
    let archive_hash = sha256(&archive);
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        declared_size,
        archive_hash,
    );

    let banks = temp_banks_dir("truncated");
    let result = download_bank_into_with_https(
        &e,
        &banks,
        &Progress::new(declared_size),
        &cancel_flag(),
        false,
    );
    assert!(matches!(
        result,
        Err(DownloadError::Truncated { expected, .. }) if expected == declared_size
    ));
    assert!(!banks.join("test-bank").exists());
}

#[test]
fn redirect_to_other_host_is_rejected() {
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();
    let other = TestServer::start(|_idx, target| {
        assert_eq!(target, "/evil");
        ServerResponse::ok(b"evil archive".to_vec())
    });

    let server = TestServer::start(move |idx, target| {
        if idx == 0 {
            assert_eq!(target, "/license");
            ServerResponse::ok(license_body.clone())
        } else {
            assert_eq!(target, "/archive");
            ServerResponse::redirect(other.url("/evil"))
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        10,
        "a".repeat(64),
    );

    let banks = temp_banks_dir("redirect-other-host");
    let result =
        download_bank_into_with_https(&e, &banks, &Progress::new(10), &cancel_flag(), false);
    assert!(matches!(result, Err(DownloadError::RedirectToOtherHost(_))));
    assert!(!banks.join("test-bank").exists());
}

#[test]
fn zip_slip_archive_is_rejected() {
    let archive = make_zip_bytes(&[("../evil.sfz", b"x")]);
    let size = archive.len() as u64;
    let archive_hash = sha256(&archive);
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        size,
        archive_hash,
    );

    let banks = temp_banks_dir("zip-slip");
    let result =
        download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel_flag(), false);
    assert!(matches!(result, Err(DownloadError::UnsafeEntry(_))));
    assert!(!banks.join("test-bank").exists());
}

#[test]
fn oversize_archive_is_rejected() {
    let archive = make_zip_bytes(&[("bank/bank.sfz", b"0123456789")]);
    let declared_size = 5u64;
    let archive_hash = sha256(&archive);
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        declared_size,
        archive_hash,
    );

    let banks = temp_banks_dir("oversize");
    let result = download_bank_into_with_https(
        &e,
        &banks,
        &Progress::new(declared_size),
        &cancel_flag(),
        false,
    );
    assert!(matches!(
        result,
        Err(DownloadError::Truncated { expected, .. }) if expected == declared_size
    ));
    assert!(!banks.join("test-bank").exists());
}

#[test]
fn cancel_aborts_download() {
    let archive = make_zip_bytes(&[("bank/bank.sfz", b"data")]);
    let size = archive.len() as u64;
    let archive_hash = sha256(&archive);
    let archive_arc = Arc::new(archive);
    let license_body: Vec<u8> = br#"{"license":{"spdx_id":"CC0-1.0"}}"#.to_vec();

    let cancel = cancel_flag();
    let cancel_for_handler = Arc::clone(&cancel);
    let server = TestServer::start(move |idx, _target| {
        if idx == 0 {
            ServerResponse::ok(license_body.clone())
        } else {
            cancel_for_handler.store(true, Ordering::Relaxed);
            ServerResponse::ok((*archive_arc).clone())
        }
    });

    let e = entry(
        server.url("/archive"),
        server.url("/license"),
        "zip",
        size,
        archive_hash,
    );

    let banks = temp_banks_dir("cancel");
    let result = download_bank_into_with_https(&e, &banks, &Progress::new(size), &cancel, false);
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert!(!banks.join("test-bank").exists());
}
