//! Talks to the real GitHub API.
#[test]
#[ignore = "network"]
fn latest_release_can_be_read() {
    // Ok(None) is fine (a release without an installer); an error is not.
    let r = p2c_update::fetch_latest().unwrap();
    println!("{r:?}");
}

/// The signed checksums made by scripts/package-windows.sh verify with the
/// public key built into the app.
#[test]
#[ignore = "needs dist/ from scripts/package-windows.sh"]
fn packaged_checksums_verify() {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dist");
    let sums = std::fs::read_to_string(dist.join("SHA256SUMS")).unwrap();
    let sig = std::fs::read_to_string(dist.join("SHA256SUMS.sig")).unwrap();
    p2c_update::verify_sums(&sums, &sig, p2c_update::RELEASE_PUBLIC_KEY).unwrap();
    let setup = std::fs::read_dir(&dist)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("Page2Cam-Setup-"))
        .unwrap();
    let want = p2c_update::expected_hash(&sums, &setup).unwrap();
    assert_eq!(p2c_update::file_sha256(&dist.join(&setup)).unwrap(), want);
}

/// The live latest release is found, its URLs pass the checks, and its signed
/// checksums verify with the key built into the app.
#[test]
#[ignore = "network"]
fn live_release_verifies() {
    let r = p2c_update::fetch_latest().unwrap().expect("latest release has an installer");
    let get = |u: &str| {
        let out = std::process::Command::new("curl")
            .args(["-fsSL", "--proto", "=https", "--proto-redir", "=https", "--url", u])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let sums = get(&r.sums_url);
    p2c_update::verify_sums(&sums, &get(&r.sig_url), p2c_update::RELEASE_PUBLIC_KEY).unwrap();
    assert!(p2c_update::expected_hash(&sums, &r.setup_name).is_some());
    println!("{} verified: {}", r.version, r.setup_url);
}
