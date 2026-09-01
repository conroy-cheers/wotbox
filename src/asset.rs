use std::{
    io::Cursor,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result as AnyResult};
use image::{GenericImageView, ImageFormat, ImageReader, Limits};
use reqwest::{StatusCode, Url, redirect::Policy};
use sha2::{Digest, Sha256};

const MAX_ENCODED_BYTES: usize = 20 * 1024 * 1024;
const MAX_DIMENSION: u32 = 12_000;
const MAX_PIXELS: u64 = 64_000_000;
pub const MATERIALIZER_VERSION: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFailureKind {
    Retryable,
    Absent,
    Unsupported,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AssetMaterializationError {
    kind: AssetFailureKind,
    code: &'static str,
    message: String,
}

impl AssetMaterializationError {
    fn new(kind: AssetFailureKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> AssetFailureKind {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

type AssetResult<T> = std::result::Result<T, AssetMaterializationError>;

#[derive(Debug, Clone)]
pub struct MaterializedAsset {
    pub source_hash: String,
    pub blob_hash: String,
    pub original_extension: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: usize,
}

#[derive(Clone)]
pub struct AssetStore {
    root: PathBuf,
}

impl AssetStore {
    pub fn new(root: PathBuf) -> AnyResult<Self> {
        std::fs::create_dir_all(root.join("blobs"))
            .with_context(|| format!("create Library asset store {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn source_hash(source_url: &str) -> String {
        hex::encode(Sha256::digest(source_url.as_bytes()))
    }

    pub fn path(&self, blob_hash: &str, variant: &str, extension: &str) -> Option<PathBuf> {
        if blob_hash.len() != 64
            || !blob_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !matches!(variant, "original" | "128" | "512")
            || !matches!(extension, "jpg" | "png" | "gif" | "webp")
        {
            return None;
        }
        let filename = if variant == "original" {
            format!("{blob_hash}.{extension}")
        } else {
            format!("{blob_hash}-{variant}.webp")
        };
        Some(self.root.join("blobs").join(&blob_hash[..2]).join(filename))
    }

    pub async fn materialize(&self, source_url: &str) -> AssetResult<MaterializedAsset> {
        let source_hash = Self::source_hash(source_url);
        let bytes = fetch_public_image(source_url).await?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || materialize_bytes(&root, source_hash, bytes))
            .await
            .map_err(|error| {
                AssetMaterializationError::new(
                    AssetFailureKind::Retryable,
                    "materializer_join_failed",
                    format!("join image materialization task: {error}"),
                )
            })?
    }

    pub async fn remove_blob(&self, blob_hash: &str, extension: &str) -> AnyResult<()> {
        let paths = [
            self.path(blob_hash, "original", extension),
            self.path(blob_hash, "128", extension),
            self.path(blob_hash, "512", extension),
        ];
        if paths.iter().any(Option::is_none) {
            anyhow::bail!("invalid Library asset blob identity");
        }
        let paths = paths.map(Option::unwrap);
        tokio::task::spawn_blocking(move || {
            for path in paths {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })
        .await
        .context("join Library asset removal task")?
    }
}

async fn fetch_public_image(source_url: &str) -> AssetResult<Vec<u8>> {
    let mut url = Url::parse(source_url).map_err(|error| {
        AssetMaterializationError::new(
            AssetFailureKind::Unsupported,
            "invalid_url",
            format!("parse artwork URL: {error}"),
        )
    })?;
    for redirect_count in 0..=3 {
        validate_url(&url)?;
        let host = url
            .host_str()
            .ok_or_else(|| {
                AssetMaterializationError::new(
                    AssetFailureKind::Unsupported,
                    "invalid_url",
                    "artwork URL has no host",
                )
            })?
            .to_owned();
        let addresses = tokio::net::lookup_host((host.as_str(), 443))
            .await
            .map_err(|error| {
                AssetMaterializationError::new(
                    AssetFailureKind::Retryable,
                    "dns_failed",
                    format!("resolve artwork host {host}: {error}"),
                )
            })?
            .filter(|address| public_ip(address.ip()))
            .collect::<Vec<_>>();
        let address = addresses.first().copied().ok_or_else(|| {
            AssetMaterializationError::new(
                AssetFailureKind::Unsupported,
                "unsafe_address",
                "artwork host did not resolve to a public address",
            )
        })?;
        let client = reqwest::Client::builder()
            .user_agent(format!("wotbox/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none())
            .resolve(&host, SocketAddr::new(address.ip(), 443))
            .build()
            .map_err(|error| retryable("http_client_failed", error))?;
        let mut response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| retryable("request_failed", error))?;
        if response.status().is_redirection() {
            if redirect_count == 3 {
                return Err(AssetMaterializationError::new(
                    AssetFailureKind::Unsupported,
                    "redirect_limit",
                    "artwork redirect limit exceeded",
                ));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| {
                    AssetMaterializationError::new(
                        AssetFailureKind::Unsupported,
                        "invalid_redirect",
                        "artwork redirect has no Location header",
                    )
                })?
                .to_str()
                .map_err(|error| {
                    AssetMaterializationError::new(
                        AssetFailureKind::Unsupported,
                        "invalid_redirect",
                        format!("artwork redirect Location is not valid text: {error}"),
                    )
                })?;
            url = url.join(location).map_err(|error| {
                AssetMaterializationError::new(
                    AssetFailureKind::Unsupported,
                    "invalid_redirect",
                    format!("resolve artwork redirect: {error}"),
                )
            })?;
            continue;
        }
        if matches!(response.status(), StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Err(AssetMaterializationError::new(
                AssetFailureKind::Absent,
                "not_found",
                format!("artwork is definitively absent ({})", response.status()),
            ));
        }
        response
            .error_for_status_ref()
            .map_err(|error| retryable("http_status", error))?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ENCODED_BYTES as u64)
        {
            return Err(unsupported(
                "encoded_too_large",
                "artwork exceeds the 20 MiB encoded limit",
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| retryable("response_body_failed", error))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_ENCODED_BYTES {
                return Err(unsupported(
                    "encoded_too_large",
                    "artwork exceeds the 20 MiB encoded limit",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    unreachable!()
}

fn validate_url(url: &Url) -> AssetResult<()> {
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return Err(unsupported(
            "invalid_url",
            "artwork URL must use HTTPS on the default port",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(unsupported(
            "invalid_url",
            "artwork URL must not contain credentials",
        ));
    }
    Ok(())
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_ipv4(ip.octets()),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ipv4(mapped.octets());
            }
            !(ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || (ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8))
        }
    }
}

fn public_ipv4([a, b, c, _d]: [u8; 4]) -> bool {
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn materialize_bytes(
    root: &Path,
    source_hash: String,
    bytes: Vec<u8>,
) -> AssetResult<MaterializedAsset> {
    let format = image::guess_format(&bytes).map_err(|error| {
        AssetMaterializationError::new(
            AssetFailureKind::Unsupported,
            "unrecognized_format",
            format!("recognize artwork format: {error}"),
        )
    })?;
    let (extension, mime_type) = match format {
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::Png => ("png", "image/png"),
        ImageFormat::Gif => ("gif", "image/gif"),
        ImageFormat::WebP => ("webp", "image/webp"),
        _ => {
            return Err(unsupported(
                "unsupported_format",
                format!("unsupported static artwork format {format:?}"),
            ));
        }
    };
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| {
        AssetMaterializationError::new(
            AssetFailureKind::Unsupported,
            "decode_failed",
            format!("decode artwork: {error}"),
        )
    })?;
    let (width, height) = image.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(unsupported(
            "decoded_too_large",
            "artwork exceeds the 64 megapixel decoded limit",
        ));
    }
    let blob_hash = hex::encode(Sha256::digest(&bytes));
    let directory = root.join("blobs").join(&blob_hash[..2]);
    std::fs::create_dir_all(&directory).map_err(|error| retryable("store_write_failed", error))?;
    atomic_write(&directory.join(format!("{blob_hash}.{extension}")), &bytes)
        .map_err(|error| retryable("store_write_failed", error))?;
    for size in [128_u32, 512_u32] {
        let variant = image.thumbnail(size, size);
        let mut output = Cursor::new(Vec::new());
        variant
            .write_to(&mut output, ImageFormat::WebP)
            .map_err(|error| retryable("variant_encode_failed", error))?;
        atomic_write(
            &directory.join(format!("{blob_hash}-{size}.webp")),
            output.get_ref(),
        )
        .map_err(|error| retryable("store_write_failed", error))?;
    }
    Ok(MaterializedAsset {
        source_hash,
        blob_hash,
        original_extension: extension.into(),
        mime_type: mime_type.into(),
        width,
        height,
        byte_size: bytes.len(),
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> AnyResult<()> {
    if path.exists() {
        return Ok(());
    }
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("asset path has no parent")?)?;
    std::io::Write::write_all(&mut file, bytes)?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error.into()),
    }
}

fn retryable(code: &'static str, error: impl std::fmt::Display) -> AssetMaterializationError {
    AssetMaterializationError::new(AssetFailureKind::Retryable, code, error.to_string())
}

fn unsupported(code: &'static str, message: impl Into<String>) -> AssetMaterializationError {
    AssetMaterializationError::new(AssetFailureKind::Unsupported, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_identity_is_stable() {
        assert_eq!(
            AssetStore::source_hash("https://example.test/a"),
            AssetStore::source_hash("https://example.test/a")
        );
        assert_ne!(
            AssetStore::source_hash("https://example.test/a"),
            AssetStore::source_hash("https://example.test/b")
        );
    }

    #[test]
    fn rejects_unsafe_addresses_and_urls() {
        assert!(!public_ip("127.0.0.1".parse().unwrap()));
        assert!(!public_ip("10.0.0.1".parse().unwrap()));
        assert!(!public_ip("100.64.0.1".parse().unwrap()));
        assert!(!public_ip("198.18.0.1".parse().unwrap()));
        assert!(!public_ip("::1".parse().unwrap()));
        assert!(!public_ip("2001:db8::1".parse().unwrap()));
        assert!(!public_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(validate_url(&Url::parse("http://example.com/x").unwrap()).is_err());
        assert!(validate_url(&Url::parse("https://user@example.com/x").unwrap()).is_err());
        let directory = tempfile::tempdir().unwrap();
        let store = AssetStore::new(directory.path().to_path_buf()).unwrap();
        assert!(
            store
                .path(&"a".repeat(64), "original", "../sqlite")
                .is_none()
        );
    }

    #[test]
    fn corrupt_and_unsupported_artwork_are_terminal() {
        let directory = tempfile::tempdir().unwrap();
        let error = materialize_bytes(
            directory.path(),
            AssetStore::source_hash("https://example.test/not-an-image"),
            b"not an image".to_vec(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), AssetFailureKind::Unsupported);
        assert_eq!(error.code(), "unrecognized_format");
    }

    #[test]
    fn materialization_retains_original_and_browsing_variants() {
        let directory = tempfile::tempdir().unwrap();
        let image = image::DynamicImage::new_rgb8(16, 8);
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Png).unwrap();
        let asset = materialize_bytes(
            directory.path(),
            AssetStore::source_hash("https://example.test/cover.png"),
            encoded.into_inner(),
        )
        .unwrap();
        let store = AssetStore::new(directory.path().to_path_buf()).unwrap();
        assert_eq!((asset.width, asset.height), (16, 8));
        assert!(
            store
                .path(&asset.blob_hash, "original", &asset.original_extension)
                .unwrap()
                .is_file()
        );
        assert!(
            store
                .path(&asset.blob_hash, "128", &asset.original_extension)
                .unwrap()
                .is_file()
        );
        assert!(
            store
                .path(&asset.blob_hash, "512", &asset.original_extension)
                .unwrap()
                .is_file()
        );
    }
}
