//! Bounded, host-owned image pipeline for the public `utools.sharp` surface.
//!
//! The compatibility iframe never receives filesystem authority or a native
//! library handle. It submits a declarative pipeline; the app layer resolves
//! picker-authorized paths to bytes and publishes an optional picker-approved
//! output after this module has completed the in-memory transformation.

use std::io::Cursor;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use image::{
    imageops::{self, FilterType},
    DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage,
};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_OPERATIONS: usize = 48;
const MAX_DIMENSION: u32 = 16_384;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SharpRequest {
    input: SharpInput,
    #[serde(default)]
    operations: Vec<SharpOperation>,
    output: SharpOutput,
}

impl SharpRequest {
    pub(crate) fn picker_path(&self) -> Option<&str> {
        self.input.picker_path()
    }

    pub(crate) fn replace_picker_path(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.input.replace_with_bytes(bytes)
    }
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum SharpInput {
    Bytes {
        data_base64: String,
    },
    Raw {
        data_base64: String,
        width: u32,
        height: u32,
        channels: u8,
    },
    Create {
        width: u32,
        height: u32,
        channels: u8,
        #[serde(default)]
        background: Value,
    },
    Path {
        path: String,
    },
}

impl SharpInput {
    pub(crate) fn picker_path(&self) -> Option<&str> {
        match self {
            Self::Path { path } => Some(path),
            _ => None,
        }
    }

    pub(crate) fn replace_with_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() || bytes.len() > MAX_SOURCE_BYTES {
            return Err(format!(
                "uTools Sharp inputs must contain 1-{MAX_SOURCE_BYTES} bytes."
            ));
        }
        *self = Self::Bytes {
            data_base64: BASE64.encode(bytes),
        };
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SharpOperation {
    method: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum SharpOutput {
    Buffer,
    File { path: String },
    Metadata,
}

#[derive(Debug)]
pub(crate) struct SharpExecution {
    pub(crate) response: Value,
    pub(crate) output_file: Option<(String, Vec<u8>)>,
}

pub(crate) fn execute(request: SharpRequest) -> Result<SharpExecution, String> {
    if request.operations.len() > MAX_OPERATIONS {
        return Err(format!(
            "uTools Sharp accepts at most {MAX_OPERATIONS} chained operations."
        ));
    }
    let (mut image, input_format) = decode_input(request.input)?;
    ensure_image_bounds(&image)?;
    if matches!(request.output, SharpOutput::Metadata) {
        return Ok(SharpExecution {
            response: metadata(&image, input_format),
            output_file: None,
        });
    }

    let mut output_format = input_format.unwrap_or(ImageFormat::Png);
    let mut jpeg_quality = 80_u8;
    for operation in request.operations {
        apply_operation(&mut image, operation, &mut output_format, &mut jpeg_quality)?;
        ensure_image_bounds(&image)?;
    }
    let bytes = encode_image(&image, output_format, jpeg_quality)?;
    let info = json!({
        "format": format_name(output_format),
        "width": image.width(),
        "height": image.height(),
        "channels": 4,
        "size": bytes.len(),
    });
    match request.output {
        SharpOutput::Buffer => Ok(SharpExecution {
            response: json!({ "dataBase64": BASE64.encode(bytes), "info": info }),
            output_file: None,
        }),
        SharpOutput::File { path } => Ok(SharpExecution {
            response: info,
            output_file: Some((path, bytes)),
        }),
        SharpOutput::Metadata => unreachable!(),
    }
}

fn decode_input(input: SharpInput) -> Result<(DynamicImage, Option<ImageFormat>), String> {
    match input {
        SharpInput::Bytes { data_base64 } => {
            let bytes = decode_base64(&data_base64)?;
            let format = image::guess_format(&bytes).map_err(|error| {
                format!("uTools Sharp could not identify the input image: {error}")
            })?;
            let image = image::load_from_memory_with_format(&bytes, format).map_err(|error| {
                format!("uTools Sharp could not decode the input image: {error}")
            })?;
            Ok((image, Some(format)))
        }
        SharpInput::Raw {
            data_base64,
            width,
            height,
            channels,
        } => {
            validate_dimensions(width, height)?;
            if !matches!(channels, 3 | 4) {
                return Err("uTools Sharp raw input supports 3 or 4 channels.".to_owned());
            }
            let bytes = decode_base64(&data_base64)?;
            let expected =
                usize::try_from(u64::from(width) * u64::from(height) * u64::from(channels))
                    .map_err(|_| "uTools Sharp raw input size overflowed.".to_owned())?;
            if bytes.len() != expected {
                return Err(
                    "uTools Sharp raw input byte length does not match its dimensions.".to_owned(),
                );
            }
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            if channels == 4 {
                rgba = bytes;
            } else {
                for pixel in bytes.chunks_exact(3) {
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                }
            }
            RgbaImage::from_raw(width, height, rgba)
                .map(DynamicImage::ImageRgba8)
                .map(|image| (image, None))
                .ok_or_else(|| "uTools Sharp raw pixels are malformed.".to_owned())
        }
        SharpInput::Create {
            width,
            height,
            channels,
            background,
        } => {
            validate_dimensions(width, height)?;
            if !matches!(channels, 3 | 4) {
                return Err("uTools Sharp create supports 3 or 4 channels.".to_owned());
            }
            let mut color = parse_color(&background)?;
            if channels == 3 {
                color[3] = 255;
            }
            Ok((
                DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, color)),
                None,
            ))
        }
        SharpInput::Path { .. } => Err(
            "uTools Sharp filesystem input was not resolved through the native picker.".to_owned(),
        ),
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() || value.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 + 8 {
        return Err("uTools Sharp input is empty or exceeds 16 MiB.".to_owned());
    }
    let bytes = BASE64
        .decode(value)
        .map_err(|_| "uTools Sharp input is not valid base64.".to_owned())?;
    if bytes.is_empty() || bytes.len() > MAX_SOURCE_BYTES {
        return Err("uTools Sharp input is empty or exceeds 16 MiB.".to_owned());
    }
    Ok(bytes)
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || pixels > MAX_PIXELS
    {
        return Err(format!(
            "uTools Sharp dimensions must be 1-{MAX_DIMENSION} per edge and at most {MAX_PIXELS} pixels."
        ));
    }
    Ok(())
}

fn ensure_image_bounds(image: &DynamicImage) -> Result<(), String> {
    validate_dimensions(image.width(), image.height())
}

fn metadata(image: &DynamicImage, format: Option<ImageFormat>) -> Value {
    json!({
        "format": format.map(format_name).unwrap_or("raw"),
        "width": image.width(),
        "height": image.height(),
        "space": "srgb",
        "channels": image.color().channel_count(),
        "depth": "uchar",
        "hasAlpha": image.color().has_alpha(),
    })
}

fn apply_operation(
    image: &mut DynamicImage,
    operation: SharpOperation,
    output_format: &mut ImageFormat,
    jpeg_quality: &mut u8,
) -> Result<(), String> {
    if operation.method.is_empty()
        || operation.method.chars().count() > 48
        || operation
            .method
            .chars()
            .any(|character| character != '_' && !character.is_ascii_alphanumeric())
    {
        return Err("uTools Sharp contains an invalid operation name.".to_owned());
    }
    match operation.method.as_str() {
        "resize" => resize(image, &operation.args),
        "rotate" => rotate(image, &operation.args),
        "flip" => {
            expect_args(&operation.args, 0, 0, "flip")?;
            *image = image.flipv();
            Ok(())
        }
        "flop" => {
            expect_args(&operation.args, 0, 0, "flop")?;
            *image = image.fliph();
            Ok(())
        }
        "grayscale" | "greyscale" => {
            expect_args(&operation.args, 0, 0, "grayscale")?;
            *image = image.grayscale();
            Ok(())
        }
        "negate" => {
            expect_args(&operation.args, 0, 1, "negate")?;
            image.invert();
            Ok(())
        }
        "blur" => {
            expect_args(&operation.args, 0, 1, "blur")?;
            let sigma = optional_f32(&operation.args, 0, 1.0, 0.3, 100.0, "blur sigma")?;
            *image = image.blur(sigma);
            Ok(())
        }
        "sharpen" => {
            expect_args(&operation.args, 0, 3, "sharpen")?;
            let sigma = optional_f32(&operation.args, 0, 1.0, 0.01, 100.0, "sharpen sigma")?;
            *image = image.unsharpen(sigma, 1);
            Ok(())
        }
        "threshold" => {
            expect_args(&operation.args, 0, 2, "threshold")?;
            threshold(image, optional_u8(&operation.args, 0, 128, "threshold")?);
            Ok(())
        }
        "normalize" | "normalise" => {
            expect_args(&operation.args, 0, 1, "normalize")?;
            normalize(image);
            Ok(())
        }
        "gamma" => {
            expect_args(&operation.args, 1, 2, "gamma")?;
            gamma(image, required_f32(&operation.args, 0, 1.0, 3.0, "gamma")?);
            Ok(())
        }
        "median" => {
            expect_args(&operation.args, 0, 1, "median")?;
            median(
                image,
                optional_u32(&operation.args, 0, 3, 1, 7, "median size")?,
            );
            Ok(())
        }
        "tint" => {
            expect_args(&operation.args, 1, 1, "tint")?;
            tint(image, parse_color(&operation.args[0])?);
            Ok(())
        }
        "flatten" => {
            expect_args(&operation.args, 0, 1, "flatten")?;
            let background = operation
                .args
                .first()
                .map(parse_color)
                .transpose()?
                .unwrap_or(Rgba([0, 0, 0, 255]));
            flatten(image, background);
            Ok(())
        }
        "extend" => extend(image, &operation.args),
        "trim" => {
            expect_args(&operation.args, 0, 2, "trim")?;
            trim(
                image,
                optional_u8(&operation.args, 0, 10, "trim tolerance")?,
            );
            Ok(())
        }
        "extract" => extract(image, &operation.args),
        "composite" => composite(image, &operation.args),
        "jpeg" | "jpg" => {
            expect_args(&operation.args, 0, 1, "jpeg")?;
            *output_format = ImageFormat::Jpeg;
            if let Some(options) = operation.args.first().and_then(Value::as_object) {
                if let Some(quality) = options.get("quality") {
                    *jpeg_quality = value_u8(quality, "JPEG quality")?;
                    if *jpeg_quality == 0 {
                        return Err("uTools Sharp JPEG quality must be 1-100.".to_owned());
                    }
                }
            }
            Ok(())
        }
        "png" => set_format(&operation.args, output_format, ImageFormat::Png, "png"),
        "webp" => set_format(&operation.args, output_format, ImageFormat::WebP, "webp"),
        "gif" => set_format(&operation.args, output_format, ImageFormat::Gif, "gif"),
        "tiff" => set_format(&operation.args, output_format, ImageFormat::Tiff, "tiff"),
        "clone" => Ok(()),
        other => Err(format!(
            "uTools Sharp operation '{other}' is not supported by the bounded iHub image pipeline."
        )),
    }
}

fn set_format(
    args: &[Value],
    output: &mut ImageFormat,
    format: ImageFormat,
    name: &str,
) -> Result<(), String> {
    expect_args(args, 0, 1, name)?;
    *output = format;
    Ok(())
}

fn resize(image: &mut DynamicImage, args: &[Value]) -> Result<(), String> {
    expect_args(args, 1, 3, "resize")?;
    let current = image.dimensions();
    let width = optional_dimension(args.first(), "resize width")?;
    let height = optional_dimension(args.get(1), "resize height")?;
    let (width, height) = match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, scaled_dimension(current.1, width, current.0)?),
        (None, Some(height)) => (scaled_dimension(current.0, height, current.1)?, height),
        (None, None) => return Err("uTools Sharp resize requires a width or height.".to_owned()),
    };
    validate_dimensions(width, height)?;
    *image = image.resize_exact(width, height, FilterType::Lanczos3);
    Ok(())
}

fn scaled_dimension(value: u32, numerator: u32, denominator: u32) -> Result<u32, String> {
    let scaled = (u64::from(value) * u64::from(numerator) + u64::from(denominator) / 2)
        / u64::from(denominator);
    u32::try_from(scaled.max(1)).map_err(|_| "uTools Sharp resize overflowed.".to_owned())
}

fn rotate(image: &mut DynamicImage, args: &[Value]) -> Result<(), String> {
    expect_args(args, 0, 2, "rotate")?;
    let angle = args
        .first()
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .rem_euclid(360);
    *image = match angle {
        0 => image.clone(),
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => {
            return Err(
                "The bounded uTools Sharp pipeline supports 90-degree rotations.".to_owned(),
            )
        }
    };
    Ok(())
}

fn threshold(image: &mut DynamicImage, threshold: u8) {
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let luminance =
            (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19) / 256;
        let value = if luminance >= u16::from(threshold) {
            255
        } else {
            0
        };
        pixel[0] = value;
        pixel[1] = value;
        pixel[2] = value;
    }
    *image = DynamicImage::ImageRgba8(rgba);
}

fn normalize(image: &mut DynamicImage) {
    let mut rgba = image.to_rgba8();
    let mut low = [255_u8; 3];
    let mut high = [0_u8; 3];
    for pixel in rgba.pixels() {
        for channel in 0..3 {
            low[channel] = low[channel].min(pixel[channel]);
            high[channel] = high[channel].max(pixel[channel]);
        }
    }
    for pixel in rgba.pixels_mut() {
        for channel in 0..3 {
            let span = u16::from(high[channel].saturating_sub(low[channel]));
            let numerator = u16::from(pixel[channel].saturating_sub(low[channel])) * 255;
            if let Some(value) = numerator.checked_div(span) {
                pixel[channel] = value as u8;
            }
        }
    }
    *image = DynamicImage::ImageRgba8(rgba);
}

fn gamma(image: &mut DynamicImage, gamma: f32) {
    let mut table = [0_u8; 256];
    for (index, value) in table.iter_mut().enumerate() {
        *value = ((index as f32 / 255.0).powf(1.0 / gamma) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] = table[pixel[channel] as usize];
        }
    }
    *image = DynamicImage::ImageRgba8(rgba);
}

fn median(image: &mut DynamicImage, size: u32) {
    let source = image.to_rgba8();
    let (width, height) = source.dimensions();
    let radius = (size / 2) as i32;
    let mut output = source.clone();
    let mut samples = Vec::with_capacity((size * size) as usize);
    for y in 0..height {
        for x in 0..width {
            let mut result = [0_u8; 4];
            for (channel, result_channel) in result.iter_mut().enumerate() {
                samples.clear();
                for offset_y in -radius..=radius {
                    for offset_x in -radius..=radius {
                        let px = (x as i32 + offset_x).clamp(0, width as i32 - 1) as u32;
                        let py = (y as i32 + offset_y).clamp(0, height as i32 - 1) as u32;
                        samples.push(source.get_pixel(px, py)[channel]);
                    }
                }
                samples.sort_unstable();
                *result_channel = samples[samples.len() / 2];
            }
            output.put_pixel(x, y, Rgba(result));
        }
    }
    *image = DynamicImage::ImageRgba8(output);
}

fn tint(image: &mut DynamicImage, color: Rgba<u8>) {
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let luminance =
            (u32::from(pixel[0]) * 54 + u32::from(pixel[1]) * 183 + u32::from(pixel[2]) * 19) / 255;
        for channel in 0..3 {
            pixel[channel] = ((luminance * u32::from(color[channel])) / 255) as u8;
        }
    }
    *image = DynamicImage::ImageRgba8(rgba);
}

fn flatten(image: &mut DynamicImage, background: Rgba<u8>) {
    let mut canvas = RgbaImage::from_pixel(image.width(), image.height(), background);
    imageops::overlay(&mut canvas, &image.to_rgba8(), 0, 0);
    for pixel in canvas.pixels_mut() {
        pixel[3] = 255;
    }
    *image = DynamicImage::ImageRgba8(canvas);
}

fn extend(image: &mut DynamicImage, args: &[Value]) -> Result<(), String> {
    expect_args(args, 1, 1, "extend")?;
    let options = args[0]
        .as_object()
        .ok_or_else(|| "uTools Sharp extend requires an options object.".to_owned())?;
    let top = object_u32(options, "top", 0, MAX_DIMENSION)?;
    let bottom = object_u32(options, "bottom", 0, MAX_DIMENSION)?;
    let left = object_u32(options, "left", 0, MAX_DIMENSION)?;
    let right = object_u32(options, "right", 0, MAX_DIMENSION)?;
    let width = image
        .width()
        .checked_add(left)
        .and_then(|value| value.checked_add(right))
        .ok_or_else(|| "uTools Sharp extend width overflowed.".to_owned())?;
    let height = image
        .height()
        .checked_add(top)
        .and_then(|value| value.checked_add(bottom))
        .ok_or_else(|| "uTools Sharp extend height overflowed.".to_owned())?;
    validate_dimensions(width, height)?;
    let background = options
        .get("background")
        .map(parse_color)
        .transpose()?
        .unwrap_or(Rgba([0, 0, 0, 0]));
    let mut canvas = RgbaImage::from_pixel(width, height, background);
    imageops::overlay(
        &mut canvas,
        &image.to_rgba8(),
        i64::from(left),
        i64::from(top),
    );
    *image = DynamicImage::ImageRgba8(canvas);
    Ok(())
}

fn trim(image: &mut DynamicImage, tolerance: u8) {
    let rgba = image.to_rgba8();
    let reference = *rgba.get_pixel(0, 0);
    let differs = |pixel: &Rgba<u8>| {
        (0..4).any(|channel| pixel[channel].abs_diff(reference[channel]) > tolerance)
    };
    let mut left = rgba.width();
    let mut top = rgba.height();
    let mut right = 0_u32;
    let mut bottom = 0_u32;
    for (x, y, pixel) in rgba.enumerate_pixels() {
        if differs(pixel) {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    if left <= right && top <= bottom {
        *image = DynamicImage::ImageRgba8(
            imageops::crop_imm(&rgba, left, top, right - left + 1, bottom - top + 1).to_image(),
        );
    }
}

fn extract(image: &mut DynamicImage, args: &[Value]) -> Result<(), String> {
    expect_args(args, 1, 1, "extract")?;
    let options = args[0]
        .as_object()
        .ok_or_else(|| "uTools Sharp extract requires an options object.".to_owned())?;
    let left = object_u32(options, "left", 0, MAX_DIMENSION)?;
    let top = object_u32(options, "top", 0, MAX_DIMENSION)?;
    let width = object_u32(options, "width", 1, MAX_DIMENSION)?;
    let height = object_u32(options, "height", 1, MAX_DIMENSION)?;
    if left
        .checked_add(width)
        .map_or(true, |value| value > image.width())
        || top
            .checked_add(height)
            .map_or(true, |value| value > image.height())
    {
        return Err("uTools Sharp extract lies outside the current image.".to_owned());
    }
    *image = DynamicImage::ImageRgba8(
        imageops::crop_imm(&image.to_rgba8(), left, top, width, height).to_image(),
    );
    Ok(())
}

fn composite(image: &mut DynamicImage, args: &[Value]) -> Result<(), String> {
    expect_args(args, 1, 1, "composite")?;
    let overlays = args[0]
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 16)
        .ok_or_else(|| "uTools Sharp composite requires 1-16 overlays.".to_owned())?;
    let mut canvas = image.to_rgba8();
    for overlay in overlays {
        let overlay = overlay
            .as_object()
            .ok_or_else(|| "Every uTools Sharp composite entry must be an object.".to_owned())?;
        if overlay
            .get("blend")
            .and_then(Value::as_str)
            .is_some_and(|blend| blend != "over")
        {
            return Err(
                "The bounded uTools Sharp pipeline supports composite blend 'over'.".to_owned(),
            );
        }
        let data = overlay
            .get("input")
            .and_then(Value::as_object)
            .and_then(|input| input.get("dataBase64"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "uTools Sharp composite input must be a bounded byte source.".to_owned()
            })?;
        let bytes = decode_base64(data)?;
        let decoded = image::load_from_memory(&bytes)
            .map_err(|error| format!("uTools Sharp could not decode a composite input: {error}"))?;
        ensure_image_bounds(&decoded)?;
        let left = overlay.get("left").and_then(Value::as_i64).unwrap_or(0);
        let top = overlay.get("top").and_then(Value::as_i64).unwrap_or(0);
        imageops::overlay(&mut canvas, &decoded.to_rgba8(), left, top);
    }
    *image = DynamicImage::ImageRgba8(canvas);
    Ok(())
}

fn encode_image(
    image: &DynamicImage,
    format: ImageFormat,
    jpeg_quality: u8,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    if format == ImageFormat::Jpeg {
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, jpeg_quality);
        encoder
            .encode_image(image)
            .map_err(|error| format!("uTools Sharp could not encode JPEG: {error}"))?;
    } else {
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .map_err(|error| format!("uTools Sharp could not encode output: {error}"))?;
    }
    if bytes.is_empty() || bytes.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "uTools Sharp output exceeds the {MAX_OUTPUT_BYTES}-byte limit."
        ));
    }
    Ok(bytes)
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Tiff => "tiff",
        _ => "image",
    }
}

fn parse_color(value: &Value) -> Result<Rgba<u8>, String> {
    if value.is_null() {
        return Ok(Rgba([0, 0, 0, 0]));
    }
    if let Some(value) = value.as_str() {
        let hex = value.strip_prefix('#').unwrap_or(value);
        let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
        return match hex.len() {
            6 => match (parse(0..2), parse(2..4), parse(4..6)) {
                (Some(r), Some(g), Some(b)) => Ok(Rgba([r, g, b, 255])),
                _ => Err("uTools Sharp color must be hexadecimal or RGBA.".to_owned()),
            },
            8 => match (parse(0..2), parse(2..4), parse(4..6), parse(6..8)) {
                (Some(r), Some(g), Some(b), Some(a)) => Ok(Rgba([r, g, b, a])),
                _ => Err("uTools Sharp color must be hexadecimal or RGBA.".to_owned()),
            },
            _ => Err("uTools Sharp color must be #RRGGBB or #RRGGBBAA.".to_owned()),
        };
    }
    let object = value
        .as_object()
        .ok_or_else(|| "uTools Sharp color must be hexadecimal or RGBA.".to_owned())?;
    let r = object
        .get("r")
        .map(|value| value_u8(value, "red"))
        .transpose()?
        .unwrap_or(0);
    let g = object
        .get("g")
        .map(|value| value_u8(value, "green"))
        .transpose()?
        .unwrap_or(0);
    let b = object
        .get("b")
        .map(|value| value_u8(value, "blue"))
        .transpose()?
        .unwrap_or(0);
    let alpha = match object.get("alpha") {
        None => 255,
        Some(value) => {
            let alpha = value
                .as_f64()
                .filter(|alpha| alpha.is_finite() && *alpha >= 0.0 && *alpha <= 1.0)
                .ok_or_else(|| "uTools Sharp alpha must be between 0 and 1.".to_owned())?;
            (alpha * 255.0).round() as u8
        }
    };
    Ok(Rgba([r, g, b, alpha]))
}

fn expect_args(args: &[Value], minimum: usize, maximum: usize, name: &str) -> Result<(), String> {
    if args.len() < minimum || args.len() > maximum {
        return Err(format!(
            "uTools Sharp {name} expects {minimum}-{maximum} arguments."
        ));
    }
    Ok(())
}

fn optional_dimension(value: Option<&Value>, label: &str) -> Result<Option<u32>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(value_u32(value, 1, MAX_DIMENSION, label)?)),
    }
}

fn value_u8(value: &Value, label: &str) -> Result<u8, String> {
    value
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| format!("uTools Sharp {label} must be an integer from 0 to 255."))
}

fn value_u32(value: &Value, minimum: u32, maximum: u32, label: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value >= minimum && *value <= maximum)
        .ok_or_else(|| {
            format!("uTools Sharp {label} must be an integer from {minimum} to {maximum}.")
        })
}

fn object_u32(
    object: &serde_json::Map<String, Value>,
    key: &str,
    minimum: u32,
    maximum: u32,
) -> Result<u32, String> {
    object
        .get(key)
        .map(|value| value_u32(value, minimum, maximum, key))
        .transpose()
        .map(|value| value.unwrap_or(minimum))
}

fn optional_u8(args: &[Value], index: usize, fallback: u8, label: &str) -> Result<u8, String> {
    args.get(index)
        .map(|value| value_u8(value, label))
        .transpose()
        .map(|value| value.unwrap_or(fallback))
}

fn optional_u32(
    args: &[Value],
    index: usize,
    fallback: u32,
    minimum: u32,
    maximum: u32,
    label: &str,
) -> Result<u32, String> {
    args.get(index)
        .map(|value| value_u32(value, minimum, maximum, label))
        .transpose()
        .map(|value| value.unwrap_or(fallback))
}

fn required_f32(
    args: &[Value],
    index: usize,
    minimum: f32,
    maximum: f32,
    label: &str,
) -> Result<f32, String> {
    args.get(index)
        .and_then(Value::as_f64)
        .filter(|value| {
            value.is_finite() && *value >= f64::from(minimum) && *value <= f64::from(maximum)
        })
        .map(|value| value as f32)
        .ok_or_else(|| format!("uTools Sharp {label} must be between {minimum} and {maximum}."))
}

fn optional_f32(
    args: &[Value],
    index: usize,
    fallback: f32,
    minimum: f32,
    maximum: f32,
    label: &str,
) -> Result<f32, String> {
    if args.get(index).map_or(true, Value::is_null) {
        Ok(fallback)
    } else {
        required_f32(args, index, minimum, maximum, label)
    }
}

#[cfg(test)]
mod tests {
    use super::{execute, SharpRequest};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use serde_json::json;
    use std::io::Cursor;

    fn fixture() -> String {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        }));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        BASE64.encode(bytes)
    }

    #[test]
    fn resize_rotate_and_png_buffer_are_bounded() {
        let request: SharpRequest = serde_json::from_value(json!({
            "input": { "kind": "bytes", "dataBase64": fixture() },
            "operations": [
                { "method": "resize", "args": [4, 2] },
                { "method": "rotate", "args": [90] },
                { "method": "png", "args": [] }
            ],
            "output": { "kind": "buffer" }
        }))
        .unwrap();
        let result = execute(request).unwrap();
        assert_eq!(result.response.pointer("/info/width"), Some(&json!(2)));
        assert_eq!(result.response.pointer("/info/height"), Some(&json!(4)));
        assert!(result.response["dataBase64"]
            .as_str()
            .unwrap()
            .starts_with("iVBOR"));
    }

    #[test]
    fn raw_input_metadata_and_unsafe_pipeline_shapes_fail_closed() {
        let metadata: SharpRequest = serde_json::from_value(json!({
            "input": {
                "kind": "raw",
                "dataBase64": BASE64.encode([255_u8, 0, 0, 0, 255, 0]),
                "width": 2,
                "height": 1,
                "channels": 3
            },
            "output": { "kind": "metadata" }
        }))
        .unwrap();
        assert_eq!(execute(metadata).unwrap().response["width"], 2);

        let unknown: SharpRequest = serde_json::from_value(json!({
            "input": { "kind": "bytes", "dataBase64": fixture() },
            "operations": [{ "method": "arbitraryNativeCall", "args": [] }],
            "output": { "kind": "buffer" }
        }))
        .unwrap();
        assert!(execute(unknown).unwrap_err().contains("not supported"));
    }
}
