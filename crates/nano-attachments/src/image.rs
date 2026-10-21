//! Local image checks before a source reaches the official admission path.
//!
//! These checks are deliberately shallow: format magic, dimensions, declared
//! type, and deployment limits. The official attachment service still fully
//! decodes, normalizes, and persists every image at submit time, and its
//! refusal remains authoritative.

use nano_runtime::Hello;

/// Raster source formats accepted by the version-one attachment path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMediaType {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    WebP,
    /// GIF (first frame only after normalization).
    Gif,
}

impl ImageMediaType {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    /// Parse one wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/webp" => Some(Self::WebP),
            "image/gif" => Some(Self::Gif),
            _ => None,
        }
    }

    /// Guess a media type from a file extension, for the attachment picker.
    pub fn from_extension(path: &std::path::Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }
}

/// What a source's bytes claim to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageProbe {
    /// Detected media type.
    pub media_type: ImageMediaType,
    /// Intrinsic width in pixels.
    pub width: u32,
    /// Intrinsic height in pixels.
    pub height: u32,
}

/// Why local image inspection failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageProbeError {
    /// The bytes are not one of the four supported source formats.
    Unsupported,
    /// The bytes carry the expected signature but the header is malformed.
    Corrupt(&'static str),
}

/// Why an image cannot be attached under the deployment limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageLimitError {
    /// The declared type or extension is not accepted.
    UnsupportedType(String),
    /// The bytes do not match the declared type.
    TypeMismatch {
        /// Type from the attachment's declared extension or metadata.
        declared: String,
        /// Type proven by the bytes.
        detected: String,
    },
    /// Source bytes exceed the per-image limit.
    TooLarge {
        /// Actual bytes.
        bytes: usize,
        /// Deployment limit.
        limit: u64,
    },
    /// Width multiplied by height exceeds the pixel limit.
    TooManyPixels {
        /// Actual pixels.
        pixels: u64,
        /// Deployment limit.
        limit: u64,
    },
    /// One side exceeds the dimension limit.
    DimensionTooLarge {
        /// Actual width or height.
        dimension: u32,
        /// Deployment limit.
        limit: u64,
    },
    /// The route selected by the user explicitly rejects image input.
    ModelDoesNotSupportImages,
}

impl std::fmt::Display for ImageLimitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedType(kind) => write!(formatter, "不支持的图片格式：{kind}"),
            Self::TypeMismatch { declared, detected } => {
                write!(
                    formatter,
                    "图片类型与声明不符（声明 {declared}，实际 {detected}）"
                )
            }
            Self::TooLarge { bytes, limit } => {
                write!(formatter, "图片 {bytes} 字节，超过 {limit} 字节上限")
            }
            Self::TooManyPixels { pixels, limit } => {
                write!(formatter, "图片 {pixels} 像素，超过 {limit} 像素上限")
            }
            Self::DimensionTooLarge { dimension, limit } => {
                write!(formatter, "图片边长 {dimension}，超过 {limit} 上限")
            }
            Self::ModelDoesNotSupportImages => write!(formatter, "当前模型不支持图片输入"),
        }
    }
}

/// Deployment image limits reported in `nano.hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageLimits {
    /// Maximum encoded source bytes per image.
    pub max_image_bytes: u64,
    /// Maximum images in one submitted message.
    pub max_images_per_message: u64,
    /// Maximum aggregate encoded source bytes in one message.
    pub max_message_image_bytes: u64,
    /// Maximum source width multiplied by height.
    pub max_image_pixels: u64,
    /// Maximum source width or height.
    pub max_image_dimension: u64,
    /// Accepted source media types.
    pub media_types: Vec<ImageMediaType>,
    /// Maximum raw bytes in one chunk.
    pub max_chunk_bytes: u64,
    /// Whether generic files are stored verbatim.
    pub file_admission: String,
}

impl ImageLimits {
    /// Read the optional S06 block from a handshake.
    pub fn from_hello(hello: &Hello) -> Option<Self> {
        let limits = hello.attachment_limits.as_ref()?;
        let media_types = limits
            .media_types
            .iter()
            .filter_map(|value| ImageMediaType::parse(value))
            .collect();
        Some(Self {
            max_image_bytes: limits.max_image_bytes,
            max_images_per_message: limits.max_images_per_message,
            max_message_image_bytes: limits.max_message_image_bytes,
            max_image_pixels: limits.max_image_pixels,
            max_image_dimension: limits.max_image_dimension,
            media_types,
            max_chunk_bytes: limits.max_chunk_bytes,
            file_admission: limits.file_admission.clone(),
        })
    }

    /// Whether this deployment accepts the given source type.
    pub fn accepts(&self, media_type: ImageMediaType) -> bool {
        self.media_types.contains(&media_type)
    }
}

/// Probe a source image's format and intrinsic dimensions.
pub fn probe(bytes: &[u8]) -> Result<ImageProbe, ImageProbeError> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        return png(bytes);
    }
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return jpeg(bytes);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return webp(bytes);
    }
    if bytes.len() >= 10 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return gif(bytes);
    }
    Err(ImageProbeError::Unsupported)
}

/// Check one probed image against the deployment limits and declared type.
pub fn check(
    declared: Option<ImageMediaType>,
    probe: &ImageProbe,
    bytes: usize,
    limits: &ImageLimits,
) -> Result<(), ImageLimitError> {
    if let Some(declared) = declared {
        if declared != probe.media_type {
            return Err(ImageLimitError::TypeMismatch {
                declared: declared.as_str().to_string(),
                detected: probe.media_type.as_str().to_string(),
            });
        }
    }
    if !limits.accepts(probe.media_type) {
        return Err(ImageLimitError::UnsupportedType(
            probe.media_type.as_str().to_string(),
        ));
    }
    if bytes as u64 > limits.max_image_bytes {
        return Err(ImageLimitError::TooLarge {
            bytes,
            limit: limits.max_image_bytes,
        });
    }
    let pixels = u64::from(probe.width) * u64::from(probe.height);
    if pixels > limits.max_image_pixels {
        return Err(ImageLimitError::TooManyPixels {
            pixels,
            limit: limits.max_image_pixels,
        });
    }
    let dimension = probe.width.max(probe.height) as u64;
    if dimension > limits.max_image_dimension {
        return Err(ImageLimitError::DimensionTooLarge {
            dimension: dimension.min(u64::from(u32::MAX)) as u32,
            limit: limits.max_image_dimension,
        });
    }
    Ok(())
}

/// Standard base64 encoding for the attachment chunk wire field.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0b1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0b111111) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn png(bytes: &[u8]) -> Result<ImageProbe, ImageProbeError> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err(ImageProbeError::Corrupt("png header is truncated"));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        return Err(ImageProbeError::Corrupt("png dimensions are zero"));
    }
    Ok(ImageProbe {
        media_type: ImageMediaType::Png,
        width,
        height,
    })
}

fn jpeg(bytes: &[u8]) -> Result<ImageProbe, ImageProbeError> {
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if marker == 0x00 || (0xd0..=0xd9).contains(&marker) {
            offset += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            return Err(ImageProbeError::Corrupt("jpeg segment is truncated"));
        }
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if length < 7 {
                return Err(ImageProbeError::Corrupt("jpeg SOF is truncated"));
            }
            let height = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]);
            let width = u16::from_be_bytes([bytes[offset + 7], bytes[offset + 8]]);
            if width == 0 || height == 0 {
                return Err(ImageProbeError::Corrupt("jpeg dimensions are zero"));
            }
            return Ok(ImageProbe {
                media_type: ImageMediaType::Jpeg,
                width: u32::from(width),
                height: u32::from(height),
            });
        }
        offset += 2 + length;
    }
    Err(ImageProbeError::Corrupt("jpeg has no frame header"))
}

fn webp(bytes: &[u8]) -> Result<ImageProbe, ImageProbeError> {
    if bytes.len() < 20 {
        return Err(ImageProbeError::Corrupt("webp header is truncated"));
    }
    let kind = &bytes[12..16];
    match kind {
        b"VP8 " => {
            if bytes.len() < 30 {
                return Err(ImageProbeError::Corrupt("webp VP8 frame is truncated"));
            }
            let data = 20usize;
            if bytes[data + 3..data + 6] != [0x9d, 0x01, 0x2a] {
                return Err(ImageProbeError::Corrupt("webp VP8 start code is invalid"));
            }
            let width = u16::from_le_bytes([bytes[data + 6], bytes[data + 7]]) & 0x3fff;
            let height = u16::from_le_bytes([bytes[data + 8], bytes[data + 9]]) & 0x3fff;
            Ok(webp_probe(width, height))
        }
        b"VP8L" => {
            if bytes.len() < 25 || bytes[20] != 0x2f {
                return Err(ImageProbeError::Corrupt("webp VP8L header is invalid"));
            }
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Ok(webp_probe(width as u16, height as u16))
        }
        b"VP8X" => {
            if bytes.len() < 30 {
                return Err(ImageProbeError::Corrupt("webp VP8X header is truncated"));
            }
            let width = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
            let height = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
            if width == 0 || height == 0 {
                return Err(ImageProbeError::Corrupt("webp dimensions are zero"));
            }
            Ok(ImageProbe {
                media_type: ImageMediaType::WebP,
                width,
                height,
            })
        }
        _ => Err(ImageProbeError::Corrupt("webp chunk type is unknown")),
    }
}

fn webp_probe(width: u16, height: u16) -> ImageProbe {
    ImageProbe {
        media_type: ImageMediaType::WebP,
        width: u32::from(width),
        height: u32::from(height),
    }
}

fn gif(bytes: &[u8]) -> Result<ImageProbe, ImageProbeError> {
    if bytes.len() < 10 {
        return Err(ImageProbeError::Corrupt("gif header is truncated"));
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]);
    let height = u16::from_le_bytes([bytes[8], bytes[9]]);
    if width == 0 || height == 0 {
        return Err(ImageProbeError::Corrupt("gif dimensions are zero"));
    }
    Ok(ImageProbe {
        media_type: ImageMediaType::Gif,
        width: u32::from(width),
        height: u32::from(height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes
    }

    fn gif_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes
    }

    fn webp_vp8x(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"WEBPVP8X");
        bytes.extend_from_slice(&[10, 0, 0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        bytes.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        bytes
    }

    fn jpeg_sof(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff];
        bytes.extend_from_slice(&[0xc0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
        bytes
    }

    fn limits() -> ImageLimits {
        ImageLimits {
            max_image_bytes: 20 * 1024 * 1024,
            max_images_per_message: 20,
            max_message_image_bytes: 200 * 1024 * 1024,
            max_image_pixels: 64_000_000,
            max_image_dimension: 8192,
            media_types: vec![
                ImageMediaType::Png,
                ImageMediaType::Jpeg,
                ImageMediaType::WebP,
                ImageMediaType::Gif,
            ],
            max_chunk_bytes: 49_152,
            file_admission: "verbatim".to_string(),
        }
    }

    #[test]
    fn probes_all_four_formats() {
        assert_eq!(probe(&png_bytes(640, 480)).unwrap().width, 640);
        assert_eq!(probe(&jpeg_sof(320, 200)).unwrap().height, 200);
        assert_eq!(probe(&gif_bytes(16, 9)).unwrap().width, 16);
        assert_eq!(probe(&webp_vp8x(1280, 720)).unwrap().height, 720);
    }

    #[test]
    fn unsupported_and_corrupt_bytes_are_distinct() {
        assert_eq!(probe(b"not an image"), Err(ImageProbeError::Unsupported));
        assert!(matches!(
            probe(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Err(ImageProbeError::Corrupt(_))
        ));
    }

    #[test]
    fn declared_type_must_match_bytes() {
        let probe = probe(&png_bytes(10, 10)).unwrap();
        let error = check(Some(ImageMediaType::Jpeg), &probe, 100, &limits()).unwrap_err();
        assert!(matches!(error, ImageLimitError::TypeMismatch { .. }));
    }

    #[test]
    fn limits_are_reported_with_real_numbers() {
        let wide = probe(&png_bytes(9000, 10)).unwrap();
        assert!(matches!(
            check(None, &wide, 100, &limits()),
            Err(ImageLimitError::DimensionTooLarge {
                dimension: 9000,
                ..
            })
        ));
        let small = probe(&png_bytes(10, 10)).unwrap();
        assert!(matches!(
            check(None, &small, 21 * 1024 * 1024, &limits()),
            Err(ImageLimitError::TooLarge { .. })
        ));
        let mut tight = limits();
        tight.max_image_pixels = 99;
        assert!(matches!(
            check(None, &small, 100, &tight),
            Err(ImageLimitError::TooManyPixels { pixels: 100, .. })
        ));
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }
}
