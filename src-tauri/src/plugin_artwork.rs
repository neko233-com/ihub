use std::{
    fs,
    io::{self, BufReader, Cursor, Read, Write},
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use image::{
    codecs::png::PngEncoder, imageops::FilterType, ColorType, GenericImageView, ImageEncoder,
    ImageFormat, ImageReader, Limits,
};

/// Artwork is display metadata, not an ambient filesystem capability. Keep
/// every stage bounded before bytes are decoded or sent across Tauri IPC.
const MAX_ARTWORK_INPUT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARTWORK_SOURCE_EDGE: u32 = 1_024;
const MAX_ARTWORK_SOURCE_PIXELS: u64 =
    MAX_ARTWORK_SOURCE_EDGE as u64 * MAX_ARTWORK_SOURCE_EDGE as u64;
const MAX_ARTWORK_DECODE_ALLOC_BYTES: u64 = 16 * 1024 * 1024;
const NORMALIZED_ARTWORK_EDGE: u32 = 128;
/// Base64 expansion plus the `data:image/png;base64,` prefix remains below
/// the renderer's existing 128 KiB native-icon source ceiling.
const MAX_NORMALIZED_PNG_BYTES: usize = 95 * 1024;

#[derive(Debug)]
pub(crate) struct PluginArtwork {
    pub(crate) canonical_path: PathBuf,
    pub(crate) data_url: String,
}

/// Rejects absolute, drive-prefixed, empty, and traversal paths on every host
/// platform. Both slash styles are treated as separators so a manifest cannot
/// become unsafe merely by moving between Windows and macOS.
pub(crate) fn validate_artwork_relative_path(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let has_drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if value.trim().is_empty()
        || value.contains('\0')
        || value.contains(':')
        || value.chars().any(char::is_control)
        || value.starts_with(['/', '\\'])
        || has_drive_prefix
        || Path::new(value).is_absolute()
        || value.split(['/', '\\']).any(is_unsafe_artwork_component)
    {
        return Err(
            "Plugin artwork paths must be normal relative paths inside the plugin package."
                .to_owned(),
        );
    }
    Ok(())
}

fn is_unsafe_artwork_component(component: &str) -> bool {
    if component.is_empty() || matches!(component, "." | "..") || component.ends_with(['.', ' ']) {
        return true;
    }
    let device_stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device_stem
            .strip_prefix("COM")
            .or_else(|| device_stem.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'))
}

fn is_symlink_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        // FILE_ATTRIBUTE_REPARSE_POINT also catches directory junctions and
        // other redirecting filesystem objects that `is_symlink` may not
        // classify as a symbolic link.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}

/// Resolves one declared image beneath the canonical package root, rejecting
/// symlinks in every path component. The original path is inspected before
/// canonicalization so even a symlink that points back inside the package is
/// not accepted as artwork.
fn canonical_artwork_path(
    package_root: &Path,
    declared_path: &str,
    label: &str,
) -> Result<PathBuf, String> {
    validate_artwork_relative_path(declared_path)?;
    let package_root = package_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve the plugin package for {label}: {error}"))?;
    if !package_root.is_dir() {
        return Err(format!(
            "The plugin package for {label} is not a directory."
        ));
    }

    let mut inspected = package_root.clone();
    // Manifest paths use a portable package namespace. Treat both historical
    // Windows separators and `/` as components on every host instead of
    // letting Unix interpret a backslash as a literal filename character.
    for component in declared_path.split(['/', '\\']) {
        inspected.push(component);
        let metadata = fs::symlink_metadata(&inspected).map_err(|error| {
            format!("Could not inspect declared {label} '{declared_path}': {error}")
        })?;
        if is_symlink_or_reparse_point(&metadata) {
            return Err(format!(
                "Declared {label} '{declared_path}' must not contain symbolic links."
            ));
        }
    }

    let canonical = inspected.canonicalize().map_err(|error| {
        format!("Could not resolve declared {label} '{declared_path}': {error}")
    })?;
    if !canonical.starts_with(&package_root) {
        return Err(format!(
            "Declared {label} '{declared_path}' escapes the plugin package."
        ));
    }
    let metadata = canonical.metadata().map_err(|error| {
        format!("Could not inspect declared {label} '{declared_path}': {error}")
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "Declared {label} '{declared_path}' is not a regular file."
        ));
    }
    if metadata.len() > MAX_ARTWORK_INPUT_BYTES {
        return Err(format!(
            "Declared {label} '{declared_path}' exceeds the {} MiB input limit.",
            MAX_ARTWORK_INPUT_BYTES / (1024 * 1024)
        ));
    }
    Ok(canonical)
}

pub(crate) fn load_plugin_artwork(
    package_root: &Path,
    declared_path: &str,
    label: &str,
) -> Result<PluginArtwork, String> {
    let canonical_path = canonical_artwork_path(package_root, declared_path, label)?;
    let file = fs::File::open(&canonical_path)
        .map_err(|error| format!("Could not open declared {label} '{declared_path}': {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_ARTWORK_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read declared {label} '{declared_path}': {error}"))?;
    if bytes.len() as u64 > MAX_ARTWORK_INPUT_BYTES {
        return Err(format!(
            "Declared {label} '{declared_path}' exceeds the {} MiB input limit.",
            MAX_ARTWORK_INPUT_BYTES / (1024 * 1024)
        ));
    }

    let format = image::guess_format(&bytes).map_err(|_| {
        format!("Declared {label} '{declared_path}' is not a supported PNG, JPEG, or WebP image.")
    })?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(format!(
            "Declared {label} '{declared_path}' is not a supported PNG, JPEG, or WebP image."
        ));
    }

    let mut reader = ImageReader::with_format(BufReader::new(Cursor::new(bytes)), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_ARTWORK_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_ARTWORK_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_ARTWORK_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|error| {
        format!("Declared {label} '{declared_path}' is not a valid bounded raster image: {error}")
    })?;
    let (width, height) = decoded.dimensions();
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("Declared {label} '{declared_path}' has invalid dimensions."))?;
    if width == 0
        || height == 0
        || width > MAX_ARTWORK_SOURCE_EDGE
        || height > MAX_ARTWORK_SOURCE_EDGE
        || pixels > MAX_ARTWORK_SOURCE_PIXELS
    {
        return Err(format!(
            "Declared {label} '{declared_path}' exceeds the {}×{} source dimension limit.",
            MAX_ARTWORK_SOURCE_EDGE, MAX_ARTWORK_SOURCE_EDGE
        ));
    }

    let rgba = decoded.to_rgba8();
    let (normalized_width, normalized_height) =
        normalized_dimensions(width, height, NORMALIZED_ARTWORK_EDGE);
    let normalized = if (normalized_width, normalized_height) == (width, height) {
        rgba
    } else {
        image::imageops::resize(
            &rgba,
            normalized_width,
            normalized_height,
            FilterType::Lanczos3,
        )
    };

    let mut png = LimitedPngBuffer::new(MAX_NORMALIZED_PNG_BYTES);
    let encode_result = PngEncoder::new(&mut png).write_image(
        normalized.as_raw(),
        normalized.width(),
        normalized.height(),
        ColorType::Rgba8.into(),
    );
    if let Err(error) = encode_result {
        if png.limit_exceeded {
            return Err(format!(
                "Declared {label} '{declared_path}' cannot be normalized within the {} KiB PNG limit.",
                MAX_NORMALIZED_PNG_BYTES / 1024
            ));
        }
        return Err(format!(
            "Declared {label} '{declared_path}' could not be normalized as PNG: {error}"
        ));
    }

    Ok(PluginArtwork {
        canonical_path,
        data_url: format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(png.bytes)
        ),
    })
}

fn normalized_dimensions(width: u32, height: u32, edge: u32) -> (u32, u32) {
    if width <= edge && height <= edge {
        return (width, height);
    }
    if width >= height {
        let scaled_height = (u64::from(height) * u64::from(edge) / u64::from(width)).max(1);
        (edge, scaled_height as u32)
    } else {
        let scaled_width = (u64::from(width) * u64::from(edge) / u64::from(height)).max(1);
        (scaled_width as u32, edge)
    }
}

struct LimitedPngBuffer {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl LimitedPngBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_exceeded: false,
        }
    }
}

impl Write for LimitedPngBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "normalized plugin artwork exceeds its PNG limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

    use super::{
        load_plugin_artwork, normalized_dimensions, validate_artwork_relative_path,
        MAX_ARTWORK_INPUT_BYTES, MAX_ARTWORK_SOURCE_EDGE,
    };

    fn temporary_directory(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("ihub-plugin-artwork-{label}-{suffix}"));
        fs::create_dir(&path).expect("temporary artwork directory");
        path
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        let pixels = usize::try_from(u64::from(width) * u64::from(height) * 4)
            .expect("test image allocation");
        let mut rgba = vec![0_u8; pixels];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[32, 180, 140, 255]);
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&rgba, width, height, ColorType::Rgba8.into())
            .expect("test PNG encoding");
        fs::write(path, png).expect("test PNG");
    }

    #[test]
    fn declared_artwork_paths_are_cross_platform_relative() {
        for safe in ["icon.png", "assets/icon.webp", r"assets\icon.jpg"] {
            validate_artwork_relative_path(safe).expect("safe relative artwork path");
        }
        for unsafe_path in [
            "",
            ".",
            "../icon.png",
            r"..\icon.png",
            "/tmp/icon.png",
            r"\server\share\icon.png",
            r"C:\icon.png",
            "assets/icon.png:secret",
            "assets//icon.png",
            "assets/NUL.png",
            "assets/icon.png.",
            "assets/icon.png ",
        ] {
            assert!(
                validate_artwork_relative_path(unsafe_path).is_err(),
                "{unsafe_path} must be rejected"
            );
        }
    }

    #[test]
    fn normalization_preserves_aspect_ratio_and_never_returns_zero() {
        assert_eq!(normalized_dimensions(64, 32, 128), (64, 32));
        assert_eq!(normalized_dimensions(1_024, 512, 128), (128, 64));
        assert_eq!(normalized_dimensions(1, 1_024, 128), (1, 128));
    }

    #[test]
    fn valid_raster_is_normalized_to_a_bounded_png_data_url() {
        let package = temporary_directory("valid");
        fs::create_dir(package.join("assets")).expect("assets directory");
        write_png(&package.join("assets/icon.png"), 512, 256);

        let artwork =
            load_plugin_artwork(&package, "assets/icon.png", "plugin icon").expect("valid artwork");
        let legacy_windows_separator =
            load_plugin_artwork(&package, r"assets\icon.png", "plugin icon")
                .expect("backslash package paths must resolve portably");
        assert_eq!(legacy_windows_separator.data_url, artwork.data_url);
        assert!(artwork
            .canonical_path
            .starts_with(package.canonicalize().expect("canonical temporary package")));
        let encoded = artwork
            .data_url
            .strip_prefix("data:image/png;base64,")
            .expect("normalized PNG data URL");
        let png = BASE64_STANDARD.decode(encoded).expect("base64 PNG");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("normalized PNG");
        assert_eq!((decoded.width(), decoded.height()), (128, 64));
        assert!(artwork.data_url.len() < 128 * 1024);

        fs::remove_dir_all(package).expect("remove valid artwork fixture");
    }

    #[test]
    fn traversal_malformed_svg_and_oversized_artwork_are_rejected() {
        let package = temporary_directory("invalid");
        let outside = package
            .parent()
            .expect("temporary parent")
            .join(format!("outside-{}.png", std::process::id()));
        write_png(&outside, 8, 8);
        fs::write(package.join("malformed.png"), b"not an image").expect("malformed fixture");
        fs::write(
            package.join("script.svg"),
            br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
        )
        .expect("SVG fixture");
        fs::write(
            package.join("too-large.png"),
            vec![0_u8; (MAX_ARTWORK_INPUT_BYTES + 1) as usize],
        )
        .expect("oversized fixture");
        write_png(
            &package.join("too-wide.png"),
            MAX_ARTWORK_SOURCE_EDGE + 1,
            1,
        );

        for (path, expected) in [
            ("../outside.png", "relative paths"),
            ("malformed.png", "supported PNG"),
            ("script.svg", "supported PNG"),
            ("too-large.png", "input limit"),
            ("too-wide.png", "valid bounded raster"),
        ] {
            let error = load_plugin_artwork(&package, path, "plugin icon")
                .expect_err("unsafe artwork must fail");
            assert!(error.contains(expected), "{path}: {error}");
        }

        fs::remove_file(outside).expect("remove traversal fixture");
        fs::remove_dir_all(package).expect("remove invalid artwork fixture");
    }

    #[test]
    fn artwork_symlinks_are_rejected_before_canonicalization() {
        let package = temporary_directory("symlink");
        let outside = package
            .parent()
            .expect("temporary parent")
            .join(format!("outside-symlink-{}.png", std::process::id()));
        write_png(&outside, 8, 8);
        let link = package.join("icon.png");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).expect("test artwork symlink");
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&outside, &link) {
            // Windows returns ERROR_PRIVILEGE_NOT_HELD as `Other` on hosts
            // without Developer Mode. The same test executes normally on
            // Windows CI/dev machines that can create a symlink.
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(1_314)
            {
                fs::remove_file(outside).expect("remove skipped symlink fixture");
                fs::remove_dir_all(package).expect("remove skipped symlink package");
                return;
            }
            panic!("could not create test artwork symlink: {error}");
        }

        let error = load_plugin_artwork(&package, "icon.png", "plugin icon")
            .expect_err("artwork symlink must fail");
        assert!(error.contains("symbolic links"), "{error}");

        fs::remove_file(link).expect("remove artwork symlink");
        fs::remove_file(outside).expect("remove symlink target");
        fs::remove_dir_all(package).expect("remove symlink package");
    }
}
