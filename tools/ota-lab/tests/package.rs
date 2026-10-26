use std::{
    fs,
    io::{Cursor, Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

fn signed_zip(path: &Path) -> [u8; 32] {
    // Public, deterministic fixture key; never a release key.
    let key = zipsign_api::SigningKey::from_bytes(&[42; 32]);
    let public = key.verifying_key().to_bytes();
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            "periph-in-one.exe",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    archive.write_all(b"fixture-payload").unwrap();
    let mut source = archive.finish().unwrap();
    source.set_position(0);
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    zipsign_api::sign::copy_and_sign_zip(
        &mut source,
        &mut output,
        &[key],
        Some(path.file_name().unwrap().to_str().unwrap().as_bytes()),
    )
    .unwrap();
    public
}

#[test]
fn bounded_download_verifies_original_filename_and_rejects_wrong_key_rename_and_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("release-0.0.2-ota.zip");
    let public = signed_zip(&source);
    let bytes = fs::read(&source).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).unwrap() > 0);
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        socket.write_all(&bytes).unwrap();
    });
    let cache = dir.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let downloaded = cache.join(source.file_name().unwrap());
    self_update::Download::from_url(format!("http://{address}/fixture.zip"))
        .timeout(Duration::from_secs(5))
        .max_download_size(1024 * 1024)
        .show_download_progress(false)
        .download_to(&mut fs::File::create(&downloaded).unwrap())
        .unwrap();
    server.join().unwrap();
    self_update::verify_signature(&downloaded, &[public]).unwrap();
    let wrong_key = zipsign_api::SigningKey::from_bytes(&[43; 32])
        .verifying_key()
        .to_bytes();
    assert!(self_update::verify_signature(&downloaded, &[wrong_key]).is_err());
    let renamed = cache.join("renamed.zip");
    fs::copy(&downloaded, &renamed).unwrap();
    assert!(self_update::verify_signature(&renamed, &[public]).is_err());
    fs::write(&downloaded, b"truncated").unwrap();
    assert!(self_update::verify_signature(&downloaded, &[public]).is_err());
}

#[test]
fn library_empty_key_list_is_a_bypass_that_application_must_reject() {
    assert!(self_update::verify_signature("nonexistent.zip", &[]).is_ok());
}
