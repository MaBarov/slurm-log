use anyhow::{Context, Result, bail};
use ed25519_compact::{PublicKey, Signature};

pub const MAX_MANIFEST_BYTES: usize = 4 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = Signature::BYTES;
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const HEADER: &str = "slurm-log-release-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseManifest {
    pub version: String,
    pub target: String,
    pub archive: String,
    pub sha256: String,
    pub size: u64,
}

impl ReleaseManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES || !bytes.is_ascii() {
            bail!("release manifest must be non-empty, ASCII, and at most 4 KiB");
        }
        let text = std::str::from_utf8(bytes).context("release manifest is not UTF-8")?;
        if !text.ends_with('\n') {
            bail!("release manifest must end with a newline");
        }
        let mut lines = text.split_terminator('\n');
        if lines.next() != Some(HEADER) {
            bail!("invalid release manifest header");
        }
        let version = manifest_field(&mut lines, "version")?;
        let target = manifest_field(&mut lines, "target")?;
        let archive = manifest_field(&mut lines, "archive")?;
        let sha256 = manifest_field(&mut lines, "sha256")?;
        let size = manifest_field(&mut lines, "size")?;
        if lines.next().is_some() {
            bail!("release manifest has unexpected fields");
        }
        validate_version(&version)?;
        validate_token(&target, 128, "release target")?;
        if archive.len() > 128
            || !archive.ends_with(".tar.gz")
            || archive.contains('/')
            || archive.contains('\\')
            || !archive
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            bail!("invalid release archive name");
        }
        if sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || sha256.bytes().any(|byte| byte.is_ascii_uppercase())
        {
            bail!("invalid release archive digest");
        }
        let size: u64 = size.parse().context("invalid release archive size")?;
        if size == 0 || size > MAX_ARCHIVE_BYTES {
            bail!("release archive exceeds the safety limit");
        }
        let manifest = Self {
            version,
            target,
            archive,
            sha256,
            size,
        };
        if manifest.canonical_bytes() != bytes {
            bail!("release manifest is not canonical");
        }
        Ok(manifest)
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        format!(
            "{HEADER}\nversion={}\ntarget={}\narchive={}\nsha256={}\nsize={}\n",
            self.version, self.target, self.archive, self.sha256, self.size
        )
        .into_bytes()
    }
}

/// Returns the trust anchor compiled into a production binary. Production
/// builds read only the reviewed source PEM; a separately named build cfg is
/// permitted solely for hermetic fixture binaries.
pub fn compiled_public_key() -> Result<PublicKey> {
    let value = configured_public_key();
    if value == "UNCONFIGURED" {
        bail!(
            "this binary was built without a configured immutable release-authentication public key"
        );
    }
    #[cfg(slurm_log_test_build)]
    {
        return public_key_from_hex(value);
    }
    #[cfg(not(slurm_log_test_build))]
    {
        public_key_from_pem(value)
    }
}

fn configured_public_key() -> &'static str {
    #[cfg(slurm_log_test_build)]
    {
        return option_env!("SLURM_LOG_TEST_RELEASE_PUBLIC_KEY").unwrap_or("UNCONFIGURED");
    }
    #[cfg(not(slurm_log_test_build))]
    {
        include_str!("../release-public-key.pem").trim()
    }
}

/// Parses only the standard, canonical Ed25519 SubjectPublicKeyInfo PEM form.
/// It is a compiled source input, not a field accepted from a release root.
#[cfg(any(test, not(slurm_log_test_build)))]
pub fn public_key_from_pem(value: &str) -> Result<PublicKey> {
    const PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    if value.contains('\r') {
        bail!("release public key PEM is not canonical");
    }
    let mut lines = value.lines();
    if lines.next() != Some("-----BEGIN PUBLIC KEY-----") {
        bail!("release public key is not a PEM public key");
    }
    let body = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("release public key PEM is missing its body"))?;
    if lines.next() != Some("-----END PUBLIC KEY-----") || lines.next().is_some() {
        bail!("release public key PEM is not canonical");
    }
    let der = decode_base64(body)?;
    if der.len() != PREFIX.len() + PublicKey::BYTES || !der.starts_with(&PREFIX) {
        bail!("release public key is not an Ed25519 SubjectPublicKeyInfo key");
    }
    PublicKey::from_slice(&der[PREFIX.len()..])
        .map_err(|error| anyhow::anyhow!("invalid release public key: {error}"))
}

#[cfg(any(test, slurm_log_test_build))]
pub fn public_key_from_hex(value: &str) -> Result<PublicKey> {
    PublicKey::from_slice(&decode_hex(value, PublicKey::BYTES)?)
        .map_err(|error| anyhow::anyhow!("invalid release public key: {error}"))
}

pub fn verify_manifest(
    manifest: &[u8],
    signature: &[u8],
    key: &PublicKey,
) -> Result<ReleaseManifest> {
    if signature.len() != MAX_SIGNATURE_BYTES {
        bail!("release manifest signature has an invalid size");
    }
    let signature = Signature::from_slice(signature)
        .map_err(|error| anyhow::anyhow!("invalid release manifest signature: {error}"))?;
    key.verify(manifest, &signature)
        .map_err(|_| anyhow::anyhow!("release manifest signature verification failed"))?;
    ReleaseManifest::parse(manifest)
}

fn manifest_field<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<String> {
    let line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("release manifest is missing {name}"))?;
    line.strip_prefix(&format!("{name}="))
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("release manifest field order is invalid"))
}

fn validate_version(value: &str) -> Result<()> {
    let mut parts = value.split('.');
    for _ in 0..3 {
        let part = parts.next().unwrap_or_default();
        if part.is_empty()
            || part.len() > 20
            || !part.bytes().all(|byte| byte.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            bail!("invalid release version");
        }
    }
    if parts.next().is_some() {
        bail!("invalid release version");
    }
    Ok(())
}

fn validate_token(value: &str, maximum: usize, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        bail!("invalid {label}");
    }
    Ok(())
}

#[cfg(any(test, slurm_log_test_build))]
fn decode_hex(value: &str, bytes: usize) -> Result<Vec<u8>> {
    if value.len() != bytes * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("hex value has an invalid length or character");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .expect("hex is ASCII")
                .chars()
                .try_fold(0_u8, |value, digit| {
                    digit
                        .to_digit(16)
                        .map(|digit| value * 16 + digit as u8)
                        .ok_or_else(|| anyhow::anyhow!("invalid hex digit"))
                })
        })
        .collect()
}

#[cfg(any(test, not(slurm_log_test_build)))]
fn decode_base64(value: &str) -> Result<Vec<u8>> {
    // An Ed25519 SPKI DER value is 44 bytes and uses exactly 60 base64 bytes.
    if value.len() != 60 || !value.ends_with('=') {
        bail!("release public key PEM has an invalid base64 body");
    }
    let mut output = Vec::with_capacity(44);
    let bytes = value.as_bytes();
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let first = base64_value(chunk[0])?;
        let second = base64_value(chunk[1])?;
        let third = if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' {
                bail!("release public key PEM has invalid base64 padding");
            }
            None
        } else {
            Some(base64_value(chunk[2])?)
        };
        let fourth = if chunk[3] == b'=' {
            if !last {
                bail!("release public key PEM has invalid base64 padding");
            }
            None
        } else {
            Some(base64_value(chunk[3])?)
        };
        output.push((first << 2) | (second >> 4));
        if let Some(third) = third {
            output.push((second << 4) | (third >> 2));
            if let Some(fourth) = fourth {
                output.push((third << 6) | fourth);
            } else if third & 0x03 != 0 {
                bail!("release public key PEM has non-canonical base64 padding");
            }
        } else {
            if fourth.is_some() {
                bail!("release public key PEM has invalid base64 padding");
            }
            if second & 0x0f != 0 {
                bail!("release public key PEM has non-canonical base64 padding");
            }
        }
    }
    Ok(output)
}

#[cfg(any(test, not(slurm_log_test_build)))]
fn base64_value(value: u8) -> Result<u8> {
    match value {
        b'A'..=b'Z' => Ok(value - b'A'),
        b'a'..=b'z' => Ok(value - b'a' + 26),
        b'0'..=b'9' => Ok(value - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => bail!("release public key PEM has an invalid base64 character"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    fn fixture() -> (Vec<u8>, String) {
        let manifest = ReleaseManifest {
            version: "1.2.3".into(),
            target: "x86_64-unknown-linux-musl".into(),
            archive: "slurm-log-linux-x86_64.tar.gz".into(),
            sha256: "a".repeat(64),
            size: 123,
        }
        .canonical_bytes();
        (manifest, "7".repeat(64))
    }

    #[test]
    fn signed_canonical_manifest_verifies_and_rejects_tampering() {
        let (manifest, seed) = fixture();
        let seed = Seed::from_slice(&decode_hex(&seed, Seed::BYTES).unwrap()).unwrap();
        let pair = KeyPair::from_seed(seed);
        let signature = pair.sk.sign(&manifest, None);
        assert_eq!(
            verify_manifest(&manifest, signature.as_ref(), &pair.pk)
                .unwrap()
                .version,
            "1.2.3"
        );
        let mut tampered = manifest.clone();
        tampered[10] ^= 1;
        assert!(verify_manifest(&tampered, signature.as_ref(), &pair.pk).is_err());
        assert!(verify_manifest(&manifest, &signature.as_ref()[..63], &pair.pk).is_err());
    }

    #[test]
    fn manifest_and_pem_are_strict() {
        let (manifest, seed) = fixture();
        assert!(ReleaseManifest::parse(&manifest).is_ok());
        let mut noncanonical = manifest.clone();
        noncanonical.extend_from_slice(b"extra=x\n");
        assert!(ReleaseManifest::parse(&noncanonical).is_err());
        let seed = Seed::from_slice(&decode_hex(&seed, Seed::BYTES).unwrap()).unwrap();
        let key = KeyPair::from_seed(seed).pk;
        let key_hex: String = key
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(public_key_from_hex(&key_hex).unwrap(), key);
        let pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA4y2wXrBPeUh7ErGI3vss1HwdZ1/Bu1Wx00O3m2cvDvQ=\n-----END PUBLIC KEY-----";
        assert!(public_key_from_pem(pem).is_ok());
        assert!(public_key_from_pem("UNCONFIGURED").is_err());
        assert!(public_key_from_pem(&pem.replace('\n', "\r\n")).is_err());
    }

    #[test]
    fn validation_helpers_cover_edge_cases() {
        assert!(validate_version("0.1.2").is_ok());
        assert!(validate_version("10.20.30").is_ok());
        assert!(validate_version("01.1.2").is_err());
        assert!(validate_version("1.2").is_err());
        assert!(validate_version("1.2.3.4").is_err());
        assert!(validate_version("1.2.abc").is_err());

        assert!(validate_token("valid-token_1.0", 20, "test").is_ok());
        assert!(validate_token("", 20, "test").is_err());
        assert!(validate_token("invalid$char", 20, "test").is_err());
        assert!(validate_token("toolong", 4, "test").is_err());

        assert!(decode_hex("bad", 2).is_err());
        assert!(decode_hex("zzzz", 2).is_err());
        assert!(decode_base64("bad").is_err());
        assert!(decode_base64(&"A".repeat(60)).is_err());

        assert!(ReleaseManifest::parse(b"").is_err());
        assert!(ReleaseManifest::parse(b"not-header\n").is_err());
        assert!(ReleaseManifest::parse(b"slurm-log-release-v1").is_err());

        let _ = compiled_public_key();
    }
}
