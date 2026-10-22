//! Bounded immutable inputs for the trusted Helm OpenPGP verifier.
//! This module performs no signature cryptography. The lifecycle supervisor
//! must run argv() with the exact approved Helm 4.3.1 and require success.
use crate::{Result, helm_oci, safety};
use base64::Engine;
use ring::digest::{SHA256, digest};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const CHART_LIMIT: u64 = 8 * 1024 * 1024;
const PROVENANCE_LIMIT: u64 = 1024 * 1024;
const KEYRING_LIMIT: u64 = 1024 * 1024;

#[derive(Clone)]
struct Input {
    source: PathBuf,
    sealed: PathBuf,
    sha256: String,
    limit: u64,
}
#[derive(Clone)]
pub struct Prepared {
    chart: Input,
    provenance: Input,
    keyring: Input,
    filename: String,
}
impl Prepared {
    pub fn argv(&self) -> Vec<String> {
        vec![
            "verify".into(),
            self.chart.sealed.to_string_lossy().into_owned(),
            "--keyring".into(),
            self.keyring.sealed.to_string_lossy().into_owned(),
            "--color=never".into(),
        ]
    }
    pub fn summary(&self) -> String {
        format!(
            "Required Helm OpenPGP provenance verification\nSigned archive filename: {}\nChart SHA-256: {}\nProvenance SHA-256: {}\nExplicit trusted public keyring SHA-256: {}\nTrust is the keys supplied in this keyring; no keyserver, personal keyring, Sigstore or transparency-log lookup.\n",
            self.filename, self.chart.sha256, self.provenance.sha256, self.keyring.sha256
        )
    }
    pub fn check(&self) -> Result<()> {
        for input in [&self.chart, &self.provenance, &self.keyring] {
            for path in [&input.source, &input.sealed] {
                if sha256(&read(path, input.limit)?) != input.sha256 {
                    return Err("chart, provenance or trusted keyring changed after signature review; prepare again".into());
                }
            }
        }
        Ok(())
    }
}
pub fn local_provenance_path(chart: &Path) -> PathBuf {
    let mut path = chart.as_os_str().to_os_string();
    path.push(".prov");
    path.into()
}
/// Only call after supervised Helm verify exits successfully. No signer name,
/// raw verifier diagnostics or arbitrary keyring text is admitted to review.
pub fn verified_fingerprint(stdout: &[u8]) -> Result<String> {
    if stdout.len() > 64 * 1024 {
        return Err("Helm verifier report exceeded 64 KiB".into());
    }
    let text =
        std::str::from_utf8(stdout).map_err(|_| "Helm verifier returned an invalid report")?;
    let fingerprints = text
        .lines()
        .filter_map(|line| line.strip_prefix("Using Key With Fingerprint: "))
        .collect::<Vec<_>>();
    if fingerprints.len() != 1
        || !matches!(fingerprints[0].len(), 40 | 64)
        || !fingerprints[0].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Helm verifier did not return one unambiguous OpenPGP fingerprint".into());
    }
    Ok(fingerprints[0].to_ascii_uppercase())
}
/// Make a private named copy because Helm provenance binds the archive's
/// original basename. `directory` must be a new child of caller-owned workspace.
pub fn prepare(
    chart: &Path,
    filename: &str,
    provenance: &Path,
    keyring: &Path,
    directory: &Path,
) -> Result<Prepared> {
    if !helm_oci::valid_archive_filename(filename) {
        return Err("signed chart needs a safe original .tgz basename".into());
    }
    if !directory.is_absolute() {
        return Err("signature workspace must be absolute".into());
    }
    let chart_bytes = read(chart, CHART_LIMIT)?;
    let provenance_bytes = read(provenance, PROVENANCE_LIMIT)?;
    let keyring_bytes = read(keyring, KEYRING_LIMIT)?;
    if !provenance_bytes.starts_with(b"-----BEGIN PGP SIGNED MESSAGE-----") {
        return Err("selected keyring requires a Helm OpenPGP clearsigned .prov file; unsigned or other signature formats are not accepted".into());
    }
    public_keyring(&keyring_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(keyring)
            .map_err(|_| "cannot inspect trusted public keyring")?
            .permissions()
            .mode()
            & 0o022
            != 0
        {
            return Err("trusted public keyring may not be group/world writable".into());
        }
    }
    safety::checked_path(directory)?;
    let parent = directory
        .parent()
        .ok_or("signature workspace has no parent")?;
    safety::ensure_private_dir(parent)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .map_err(|_| "cannot create new private signature workspace")?;
    let named = directory.join(filename);
    let adjacent = local_provenance_path(&named);
    let trusted = directory.join("trusted-public-keys.pgp");
    let build = (|| {
        sealed(&named, &chart_bytes)?;
        sealed(&adjacent, &provenance_bytes)?;
        sealed(&trusted, &keyring_bytes)?;
        Ok(Prepared {
            chart: Input {
                source: chart.into(),
                sealed: named,
                sha256: sha256(&chart_bytes),
                limit: CHART_LIMIT,
            },
            provenance: Input {
                source: provenance.into(),
                sealed: adjacent,
                sha256: sha256(&provenance_bytes),
                limit: PROVENANCE_LIMIT,
            },
            keyring: Input {
                source: keyring.into(),
                sealed: trusted,
                sha256: sha256(&keyring_bytes),
                limit: KEYRING_LIMIT,
            },
            filename: filename.into(),
        })
    })();
    if build.is_err() {
        let _ = fs::remove_dir_all(directory);
    }
    build
}

/// Inspect public-only packet framing, not signatures, identities or crypto.
/// The trusted Helm verifier still validates the actual OpenPGP/keybox format.
fn public_keyring(bytes: &[u8]) -> Result<()> {
    let error = "trusted keyring must contain public OpenPGP keys only; secret, malformed or unsupported packet framing is rejected";
    let mut primary = 0usize;
    if bytes.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----") {
        let text = std::str::from_utf8(bytes).map_err(|_| error)?;
        let mut lines = text.lines();
        if lines.next() != Some("-----BEGIN PGP PUBLIC KEY BLOCK-----") {
            return Err(error.into());
        }
        let mut blank = false;
        for line in lines.by_ref() {
            if line.is_empty() {
                blank = true;
                break;
            }
            if !(line.starts_with("Version: ") || line.starts_with("Comment: ")) {
                return Err(error.into());
            }
        }
        if !blank {
            return Err(error.into());
        }
        let mut encoded = String::new();
        let mut ended = false;
        let mut checksum = false;
        for line in lines.by_ref() {
            if line == "-----END PGP PUBLIC KEY BLOCK-----" {
                ended = true;
                break;
            }
            if line.starts_with('=') {
                if checksum || line.len() != 5 {
                    return Err(error.into());
                }
                checksum = true;
            } else if checksum
                || line.is_empty()
                || !line
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
            {
                return Err(error.into());
            } else {
                encoded.push_str(line);
            }
        }
        if !ended || lines.any(|line| !line.trim().is_empty()) {
            return Err(error.into());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| error)?;
        primary += public_packets(&decoded)?;
    } else if bytes.get(8..12) == Some(b"KBXf") {
        let mut offset = 0usize;
        let mut blobs = 0usize;
        while offset < bytes.len() {
            blobs += 1;
            let remaining = &bytes[offset..];
            if blobs > 4096 || remaining.len() < 8 {
                return Err(error.into());
            }
            let size = u32::from_be_bytes(remaining[..4].try_into().map_err(|_| error)?) as usize;
            if size < 8 || size > remaining.len() {
                return Err(error.into());
            }
            let blob = &remaining[..size];
            match blob[4] {
                1 if offset == 0
                    && size >= 32
                    && blob[5] == 1
                    && blob.get(8..12) == Some(b"KBXf") => {}
                2 if offset != 0 && size >= 36 && matches!(blob[5], 1 | 2) => {
                    // GnuPG defines bit 0 as "secret" on an OpenPGP key blob.
                    if u16::from_be_bytes([blob[6], blob[7]]) & 1 != 0 {
                        return Err(error.into());
                    }
                    let start =
                        u32::from_be_bytes(blob[8..12].try_into().map_err(|_| error)?) as usize;
                    let len =
                        u32::from_be_bytes(blob[12..16].try_into().map_err(|_| error)?) as usize;
                    let end = start.checked_add(len).ok_or(error)?;
                    if start < 16 || len == 0 || end > size - 20 {
                        return Err(error.into());
                    }
                    primary += public_packets(&blob[start..end])?;
                }
                _ => return Err(error.into()),
            }
            offset += size;
        }
    } else {
        primary += public_packets(bytes)?;
    }
    if primary == 0 {
        return Err(error.into());
    }
    Ok(())
}
fn public_packets(mut bytes: &[u8]) -> Result<usize> {
    let error = "trusted keyring must contain public OpenPGP keys only; secret, malformed or unsupported packet framing is rejected";
    let mut primary = 0usize;
    let mut packets = 0usize;
    while !bytes.is_empty() {
        packets += 1;
        let first = bytes[0];
        if packets > 4096 || first & 0x80 == 0 {
            return Err(error.into());
        }
        bytes = &bytes[1..];
        let (tag, length_bytes) = if first & 0x40 != 0 {
            (first & 0x3f, None)
        } else {
            (
                first >> 2 & 0x0f,
                Some(match first & 3 {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    _ => return Err(error.into()),
                }),
            )
        };
        // Never admit a secret key/subkey, compressed nested packet, encrypted
        // packet or literal data as a nominal public trust root.
        if !matches!(tag, 2 | 6 | 10 | 12 | 13 | 14 | 17 | 21) {
            return Err(error.into());
        }
        let length;
        if let Some(count) = length_bytes {
            if bytes.len() < count {
                return Err(error.into());
            }
            length = bytes[..count]
                .iter()
                .fold(0usize, |value, byte| value * 256 + usize::from(*byte));
            bytes = &bytes[count..];
        } else {
            let first = *bytes.first().ok_or(error)?;
            bytes = &bytes[1..];
            length = match first {
                0..=191 => usize::from(first),
                192..=223 => {
                    let second = *bytes.first().ok_or(error)?;
                    bytes = &bytes[1..];
                    (usize::from(first) - 192) * 256 + usize::from(second) + 192
                }
                255 => {
                    if bytes.len() < 4 {
                        return Err(error.into());
                    }
                    let value =
                        u32::from_be_bytes(bytes[..4].try_into().map_err(|_| error)?) as usize;
                    bytes = &bytes[4..];
                    value
                }
                _ => return Err(error.into()),
            };
        }
        if length == 0 || length > bytes.len() {
            return Err(error.into());
        }
        if tag == 6 {
            primary += 1;
        }
        bytes = &bytes[length..];
    }
    Ok(primary)
}
fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.len() == b.len()
            && a.modified().ok() == b.modified().ok()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len() && a.modified().ok() == b.modified().ok()
    }
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    if cfg!(all(
        unix,
        not(any(
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )
        ))
    )) {
        return Err(
            "nonblocking signature file validation supports macOS and Linux x86-64/ARM64".into(),
        );
    }
    safety::checked_path(path)?;
    let before = fs::symlink_metadata(path)
        .map_err(|_| "required chart/provenance/public-keyring file is missing or unreadable")?;
    if !before.is_file() || before.len() == 0 || before.len() > limit {
        return Err("signature input must be a nonempty bounded regular file".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0004);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0800);
    }
    let file = options
        .open(path)
        .map_err(|_| "cannot open signature input")?;
    let opened = file
        .metadata()
        .map_err(|_| "cannot inspect opened signature input")?;
    if !opened.is_file() || !same_metadata(&before, &opened) {
        return Err("signature input changed while opening; prepare again".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read bounded signature input")?;
    if bytes.len() as u64 > limit
        || !same_metadata(
            &opened,
            &file
                .metadata()
                .map_err(|_| "cannot recheck signature input")?,
        )
    {
        return Err("signature input changed or exceeded its bound; prepare again".into());
    }
    Ok(bytes)
}
fn sealed(path: &Path, bytes: &[u8]) -> Result<()> {
    safety::write_new(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o400))
            .map_err(|_| "cannot seal signature verification input")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Result<Self> {
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "temp")?
                .join(format!(
                    "kborg-prov-{}-{}",
                    std::process::id(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                ));
            safety::ensure_private_dir(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn verification_inputs_keep_signed_basename_are_immutable_and_bind_sources() -> Result<()> {
        let root = Temp::new()?;
        let chart = root.0.join("chart.tgz");
        let provenance = local_provenance_path(&chart);
        let keyring = root.0.join("trusted.gpg");
        safety::write_new(&chart, b"packaged bytes")?;
        safety::write_new(&provenance, b"-----BEGIN PGP SIGNED MESSAGE-----\nfixture")?;
        safety::write_new(&keyring, &[0xc6, 1, 4])?;
        let prepared = prepare(
            &chart,
            "chart-1.2.3+build.tgz",
            &provenance,
            &keyring,
            &root.0.join("verify"),
        )?;
        prepared.check()?;
        let argv = prepared.argv();
        assert_eq!(argv[0], "verify");
        assert!(argv[1].ends_with("chart-1.2.3+build.tgz"));
        assert_eq!(argv[2], "--keyring");
        assert!(!prepared.summary().contains("public fixture keyring"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&prepared.chart.sealed)
                    .map_err(|_| "stat")?
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
        }
        fs::write(&keyring, b"different trusted keys").map_err(|_| "replace")?;
        assert!(prepared.check().is_err());
        Ok(())
    }
    #[test]
    fn missing_unbounded_or_unsafe_signature_inputs_fail_closed() -> Result<()> {
        let root = Temp::new()?;
        let chart = root.0.join("chart.tgz");
        let provenance = local_provenance_path(&chart);
        let keyring = root.0.join("keys");
        safety::write_new(&chart, b"chart")?;
        safety::write_new(&keyring, b"keys")?;
        assert!(
            prepare(
                &chart,
                "chart.tgz",
                &provenance,
                &keyring,
                &root.0.join("verify")
            )
            .is_err()
        );
        safety::write_new(&provenance, b"not a clearsigned provenance")?;
        assert!(
            prepare(
                &chart,
                "chart.tgz",
                &provenance,
                &keyring,
                &root.0.join("verify")
            )
            .is_err()
        );
        assert!(
            prepare(
                &chart,
                "../chart.tgz",
                &provenance,
                &keyring,
                &root.0.join("verify")
            )
            .is_err()
        );
        assert!(!root.0.join("verify").exists());
        Ok(())
    }
    #[test]
    fn public_only_keyring_framing_rejects_binary_secret_material() -> Result<()> {
        let public = [0xc6, 1, 4]; // Framing only; Helm validates key contents.
        public_keyring(&public)?;
        public_keyring(&[0x98, 1, 4])?; // Legacy public-key packet framing.
        for secret in [[0xc5, 1, 4], [0xc7, 1, 4], [0x94, 1, 4], [0x9c, 1, 4]] {
            let mut mixed = public.to_vec();
            mixed.extend(secret);
            assert!(public_keyring(&mixed).is_err());
            let encoded = base64::engine::general_purpose::STANDARD.encode(&mixed);
            assert!(public_keyring(format!("-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n{encoded}\n-----END PGP PUBLIC KEY BLOCK-----\n").as_bytes()).is_err());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(public);
        public_keyring(format!("-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n{encoded}\n-----END PGP PUBLIC KEY BLOCK-----\n").as_bytes())?;
        for bad in [
            &[0xc6, 224, 4][..],
            &[0xc6, 255, 255, 255, 255, 255][..],
            &[0xc8, 1, 4][..],
        ] {
            assert!(public_keyring(bad).is_err());
        }
        let mut keybox = vec![0; 32];
        keybox[..4].copy_from_slice(&32u32.to_be_bytes());
        keybox[4] = 1;
        keybox[5] = 1;
        keybox[8..12].copy_from_slice(b"KBXf");
        let mut blob = vec![0; 39];
        blob[..4].copy_from_slice(&39u32.to_be_bytes());
        blob[4] = 2;
        blob[5] = 1;
        blob[8..12].copy_from_slice(&16u32.to_be_bytes());
        blob[12..16].copy_from_slice(&3u32.to_be_bytes());
        blob[16..19].copy_from_slice(&public);
        keybox.extend(&blob);
        public_keyring(&keybox)?;
        keybox[32 + 7] = 1; // Keybox secret-key flag, even with public packet.
        assert!(public_keyring(&keybox).is_err());
        keybox[32 + 7] = 0;
        keybox[32 + 16] = 0xc5;
        assert!(public_keyring(&keybox).is_err());
        Ok(())
    }
    #[test]
    fn verifier_report_exposes_only_one_strict_fingerprint() -> Result<()> {
        let hex = "AB".repeat(20);
        assert_eq!(
            verified_fingerprint(
                format!("Signed by: PRIVATE_NAME\nUsing Key With Fingerprint: {hex}\n").as_bytes()
            )?,
            hex
        );
        assert!(verified_fingerprint(b"Using Key With Fingerprint: credential CANARY\n").is_err());
        assert!(
            verified_fingerprint(
                format!("Using Key With Fingerprint: {hex}\nUsing Key With Fingerprint: {hex}\n")
                    .as_bytes()
            )
            .is_err()
        );
        Ok(())
    }
}
