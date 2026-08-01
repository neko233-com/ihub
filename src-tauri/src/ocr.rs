//! Trusted, in-memory Windows OCR for the first-party screenshot workbench.
//!
//! The command accepts only one bounded PNG data URL from the trusted main
//! window. Pixels never go through a plugin, filesystem path, worker process,
//! temporary file, cloud service, or persistence layer.

use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use image::ImageDecoder;
use serde::{Deserialize, Serialize};

const PNG_DATA_URL_PREFIX: &str = "data:image/png;base64,";
const MAX_OCR_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_OCR_PIXELS: u64 = 24_000_000;
const MAX_OCR_TEXT_BYTES: usize = 256 * 1024;
const MAX_LANGUAGE_TAG_BYTES: usize = 35;
const MAX_OCR_LANGUAGES: usize = 128;

static OCR_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OcrImageRequest {
    data_url: String,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OcrLanguageInfo {
    tag: String,
    display_name: String,
    native_name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OcrCapabilities {
    available: bool,
    engine: String,
    max_image_dimension: u32,
    max_png_bytes: usize,
    max_text_bytes: usize,
    languages: Vec<OcrLanguageInfo>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OcrRecognitionResult {
    text: String,
    language: String,
    line_count: u32,
    width: u32,
    height: u32,
    truncated: bool,
}

struct OcrLease;

impl OcrLease {
    fn acquire() -> Result<Self, String> {
        OCR_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "已有一项 OCR 识别正在运行，请等待它完成。".to_owned())
    }
}

impl Drop for OcrLease {
    fn drop(&mut self) {
        OCR_RUNNING.store(false, Ordering::Release);
    }
}

#[tauri::command]
pub async fn get_ocr_capabilities() -> Result<OcrCapabilities, String> {
    tauri::async_runtime::spawn_blocking(platform_ocr_capabilities)
        .await
        .map_err(|error| format!("OCR 能力检查任务失败：{error}"))?
}

#[tauri::command]
pub async fn recognize_ocr_image(request: OcrImageRequest) -> Result<OcrRecognitionResult, String> {
    let lease = OcrLease::acquire()?;
    let (png, width, height) = decode_ocr_png(&request.data_url)?;
    let language = normalize_language(request.language)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        platform_recognize_png(&png, width, height, language.as_deref())
    })
    .await
    .map_err(|error| format!("OCR 识别任务失败：{error}"))?
}

fn normalize_language(language: Option<String>) -> Result<Option<String>, String> {
    let Some(language) = language else {
        return Ok(None);
    };
    let language = language.trim();
    if language.is_empty() {
        return Ok(None);
    }
    if language.len() > MAX_LANGUAGE_TAG_BYTES
        || !language
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("OCR 语言标签无效。".to_owned());
    }
    Ok(Some(language.to_owned()))
}

fn decode_ocr_png(data_url: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let encoded = data_url
        .strip_prefix(PNG_DATA_URL_PREFIX)
        .ok_or_else(|| "OCR 只接受 PNG data URL。".to_owned())?;
    let maximum_encoded_bytes = MAX_OCR_PNG_BYTES.div_ceil(3) * 4;
    if encoded.is_empty() || encoded.len() > maximum_encoded_bytes {
        return Err(format!(
            "OCR PNG 必须小于或等于 {} MiB。",
            MAX_OCR_PNG_BYTES / (1024 * 1024)
        ));
    }
    let png = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| "OCR PNG 的 Base64 数据无效。".to_owned())?;
    if png.is_empty() || png.len() > MAX_OCR_PNG_BYTES {
        return Err(format!(
            "OCR PNG 必须小于或等于 {} MiB。",
            MAX_OCR_PNG_BYTES / (1024 * 1024)
        ));
    }
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(&png))
        .map_err(|_| "OCR 无法解码这张 PNG 图片。".to_owned())?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    Ok((png, width, height))
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("OCR 图片没有可识别的像素。".to_owned());
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "OCR 图片尺寸溢出。".to_owned())?;
    if pixels > MAX_OCR_PIXELS {
        return Err("OCR 图片不能超过 2400 万像素。".to_owned());
    }
    Ok(())
}

fn truncate_utf8(value: String) -> (String, bool) {
    if value.len() <= MAX_OCR_TEXT_BYTES {
        return (value, false);
    }
    let mut boundary = MAX_OCR_TEXT_BYTES;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (value[..boundary].to_owned(), true)
}

#[cfg(windows)]
struct WinRtApartment;

#[cfg(windows)]
impl WinRtApartment {
    fn initialize() -> Result<Self, String> {
        use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map(|_| Self)
            .map_err(|error| format!("Windows OCR 无法初始化 WinRT：{error}"))
    }
}

#[cfg(windows)]
impl Drop for WinRtApartment {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    }
}

#[cfg(windows)]
fn platform_ocr_capabilities() -> Result<OcrCapabilities, String> {
    use windows::Media::Ocr::OcrEngine;

    let _apartment = WinRtApartment::initialize()?;
    let max_image_dimension = OcrEngine::MaxImageDimension()
        .map_err(|error| format!("无法读取 Windows OCR 图片尺寸上限：{error}"))?;
    let available = OcrEngine::AvailableRecognizerLanguages()
        .map_err(|error| format!("无法读取 Windows OCR 语言包：{error}"))?;
    let count = usize::try_from(
        available
            .Size()
            .map_err(|error| format!("无法读取 Windows OCR 语言数量：{error}"))?,
    )
    .map_err(|_| "Windows OCR 语言数量无效。".to_owned())?
    .min(MAX_OCR_LANGUAGES);
    let mut languages = Vec::with_capacity(count);
    for index in 0..count {
        let language = available
            .GetAt(index as u32)
            .map_err(|error| format!("无法读取 Windows OCR 语言：{error}"))?;
        languages.push(OcrLanguageInfo {
            tag: language
                .LanguageTag()
                .map_err(|error| format!("无法读取 OCR 语言标签：{error}"))?
                .to_string(),
            display_name: language
                .DisplayName()
                .map_err(|error| format!("无法读取 OCR 语言名称：{error}"))?
                .to_string(),
            native_name: language
                .NativeName()
                .map_err(|error| format!("无法读取 OCR 本地语言名称：{error}"))?
                .to_string(),
        });
    }
    Ok(OcrCapabilities {
        available: !languages.is_empty(),
        engine: "Windows.Media.Ocr".to_owned(),
        max_image_dimension,
        max_png_bytes: MAX_OCR_PNG_BYTES,
        max_text_bytes: MAX_OCR_TEXT_BYTES,
        languages,
    })
}

#[cfg(not(windows))]
fn platform_ocr_capabilities() -> Result<OcrCapabilities, String> {
    Err("本地屏幕 OCR 当前只在 Windows 10/11 桌面端提供。".to_owned())
}

#[cfg(windows)]
fn platform_recognize_png(
    png: &[u8],
    width: u32,
    height: u32,
    requested_language: Option<&str>,
) -> Result<OcrRecognitionResult, String> {
    use windows::{
        core::HSTRING,
        Globalization::Language,
        Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat},
        Media::Ocr::OcrEngine,
        Storage::Streams::{DataWriter, InMemoryRandomAccessStream},
    };

    let _apartment = WinRtApartment::initialize()?;
    let engine_max = OcrEngine::MaxImageDimension()
        .map_err(|error| format!("无法读取 Windows OCR 尺寸上限：{error}"))?;
    if width > engine_max || height > engine_max {
        return Err(format!(
            "OCR 选区单边不能超过 {engine_max}px；请缩小选区后重试。"
        ));
    }

    let stream = InMemoryRandomAccessStream::new()
        .map_err(|error| format!("无法创建内存 OCR 图片流：{error}"))?;
    let output = stream
        .GetOutputStreamAt(0)
        .map_err(|error| format!("无法打开内存 OCR 输出流：{error}"))?;
    let writer = DataWriter::CreateDataWriter(&output)
        .map_err(|error| format!("无法创建内存 OCR 写入器：{error}"))?;
    writer
        .WriteBytes(png)
        .map_err(|error| format!("无法写入内存 OCR 图片：{error}"))?;
    writer
        .StoreAsync()
        .and_then(|operation| operation.get())
        .map_err(|error| format!("无法提交内存 OCR 图片：{error}"))?;
    writer
        .FlushAsync()
        .and_then(|operation| operation.get())
        .map_err(|error| format!("无法刷新内存 OCR 图片：{error}"))?;
    writer
        .DetachStream()
        .map_err(|error| format!("无法释放内存 OCR 写入器：{error}"))?;
    stream
        .Seek(0)
        .map_err(|error| format!("无法复位内存 OCR 图片流：{error}"))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows 无法解码 OCR PNG：{error}"))?;
    let decoded_width = decoder
        .PixelWidth()
        .map_err(|error| format!("无法读取 OCR 图片宽度：{error}"))?;
    let decoded_height = decoder
        .PixelHeight()
        .map_err(|error| format!("无法读取 OCR 图片高度：{error}"))?;
    if decoded_width != width || decoded_height != height {
        return Err("OCR 图片尺寸在解码过程中发生变化。".to_owned());
    }
    let bitmap = decoder
        .GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows 无法准备 OCR 位图：{error}"))?;

    let engine = if let Some(tag) = requested_language {
        let tag = HSTRING::from(tag);
        if !Language::IsWellFormed(&tag).unwrap_or(false) {
            return Err("OCR 语言标签不是有效的 BCP 47 标签。".to_owned());
        }
        let language =
            Language::CreateLanguage(&tag).map_err(|_| "无法创建所选 OCR 语言。".to_owned())?;
        if !OcrEngine::IsLanguageSupported(&language).unwrap_or(false) {
            return Err("所选 OCR 语言包尚未安装到 Windows。".to_owned());
        }
        OcrEngine::TryCreateFromLanguage(&language)
            .map_err(|_| "无法启动所选 Windows OCR 语言。".to_owned())?
    } else {
        OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|_| "Windows 没有可用于当前用户语言的 OCR 引擎。".to_owned())?
    };
    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.get())
        .map_err(|_| "Windows 无法识别这个图片选区。".to_owned())?;
    let text = result
        .Text()
        .map_err(|_| "Windows OCR 没有返回有效文字。".to_owned())?
        .to_string();
    let (text, truncated) = truncate_utf8(text);
    let line_count = result
        .Lines()
        .and_then(|lines| lines.Size())
        .map_err(|_| "Windows OCR 没有返回有效行信息。".to_owned())?;
    let language = engine
        .RecognizerLanguage()
        .and_then(|language| language.LanguageTag())
        .map_err(|_| "Windows OCR 没有返回识别语言。".to_owned())?
        .to_string();
    Ok(OcrRecognitionResult {
        text,
        language,
        line_count,
        width,
        height,
        truncated,
    })
}

#[cfg(not(windows))]
fn platform_recognize_png(
    _png: &[u8],
    _width: u32,
    _height: u32,
    _requested_language: Option<&str>,
) -> Result<OcrRecognitionResult, String> {
    Err("本地屏幕 OCR 当前只在 Windows 10/11 桌面端提供。".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

    fn tiny_png_data_url() -> String {
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[255; 4 * 4 * 4], 4, 4, ColorType::Rgba8.into())
            .expect("tiny PNG should encode");
        format!("{PNG_DATA_URL_PREFIX}{}", BASE64_STANDARD.encode(png))
    }

    #[test]
    fn accepts_one_bounded_png_data_url() {
        let (png, width, height) = decode_ocr_png(&tiny_png_data_url()).expect("PNG should parse");
        assert!(!png.is_empty());
        assert_eq!((width, height), (4, 4));
    }

    #[test]
    fn rejects_non_png_and_invalid_language_tags() {
        assert!(decode_ocr_png("data:image/jpeg;base64,AAAA").is_err());
        assert!(normalize_language(Some("zh_CN".to_owned())).is_err());
        assert_eq!(
            normalize_language(Some(" zh-CN ".to_owned())),
            Ok(Some("zh-CN".to_owned()))
        );
    }

    #[test]
    fn truncates_ocr_text_on_utf8_boundary() {
        let value = "字".repeat(MAX_OCR_TEXT_BYTES);
        let (truncated, did_truncate) = truncate_utf8(value);
        assert!(did_truncate);
        assert!(truncated.len() <= MAX_OCR_TEXT_BYTES);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual Windows OCR language-pack smoke test"]
    fn windows_ocr_pipeline_decodes_an_in_memory_png() {
        let (png, width, height) = decode_ocr_png(&tiny_png_data_url()).expect("PNG should parse");
        let result = platform_recognize_png(&png, width, height, None)
            .expect("installed Windows OCR should process a blank PNG");
        assert!(!result.language.is_empty());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "manual Windows OCR text-recognition acceptance test"]
    fn windows_ocr_recognizes_supplied_text_fixture() {
        let path = std::env::var("IHUB_OCR_TEST_PNG")
            .expect("set IHUB_OCR_TEST_PNG to a local PNG containing visible text");
        let png = std::fs::read(path).expect("OCR fixture should be readable");
        let decoder = image::codecs::png::PngDecoder::new(Cursor::new(&png))
            .expect("OCR fixture should be a PNG");
        let (width, height) = decoder.dimensions();
        let result = platform_recognize_png(&png, width, height, None)
            .expect("Windows OCR should recognize the supplied fixture");
        let normalized = result.text.to_ascii_lowercase();
        assert!(
            normalized.contains("ihub") || normalized.contains("ocr"),
            "expected the supplied fixture text in OCR output, got: {}",
            result.text
        );
    }
}
