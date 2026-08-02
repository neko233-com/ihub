//! Verified, on-demand FFmpeg integration used only by `utools.runFFmpeg`.
//!
//! Windows is the current runtime acceptance platform. The integration is a
//! pinned Gyan FFmpeg 8.1.2 essentials ZIP, whose archive hash is compiled into
//! iHub. Only `ffmpeg.exe` and its GPL notice are extracted into app-data, and
//! a proof records the executable hash for every subsequent launch.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Cursor, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::ZipArchive;

use crate::background_process::background_command;

pub(crate) const FFMPEG_VERSION: &str = "8.1.2";
pub(crate) const FFMPEG_ARCHIVE_URL: &str =
    "https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-8.1.2-essentials_build.zip";
pub(crate) const FFMPEG_ARCHIVE_SHA256: &str =
    "db580001caa24ac104c8cb856cd113a87b0a443f7bdf47d8c12b1d740584a2ec";
const MAX_ARCHIVE_BYTES: usize = 140 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES: u64 = 220 * 1024 * 1024;
const MAX_LICENSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct UtoolsFfmpegIntegration {
    root: Arc<PathBuf>,
    install_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FfmpegProof {
    version: String,
    archive_sha256: String,
    executable_sha256: String,
    source: String,
}

impl UtoolsFfmpegIntegration {
    pub(crate) fn new(app_data_dir: PathBuf) -> Self {
        Self {
            root: Arc::new(
                app_data_dir
                    .join("integrations")
                    .join("ffmpeg")
                    .join(FFMPEG_VERSION),
            ),
            install_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn installed_executable(&self) -> Result<Option<PathBuf>, String> {
        #[cfg(not(target_os = "windows"))]
        {
            return Ok(None);
        }
        #[cfg(target_os = "windows")]
        {
            let executable = self.root.join("ffmpeg.exe");
            let proof_path = self.root.join("proof.json");
            if !executable.exists() && !proof_path.exists() {
                return Ok(None);
            }
            ensure_regular_file(&executable, MAX_EXECUTABLE_BYTES)?;
            ensure_regular_file(&proof_path, 16 * 1024)?;
            let proof = serde_json::from_slice::<FfmpegProof>(
                &fs::read(&proof_path)
                    .map_err(|error| format!("Could not read the FFmpeg proof: {error}"))?,
            )
            .map_err(|error| format!("The FFmpeg proof is invalid: {error}"))?;
            if proof.version != FFMPEG_VERSION
                || proof.archive_sha256 != FFMPEG_ARCHIVE_SHA256
                || proof.source != FFMPEG_ARCHIVE_URL
            {
                return Err("The installed FFmpeg proof does not match this iHub build.".to_owned());
            }
            let current = sha256_file(&executable)?;
            if current != proof.executable_sha256 {
                return Err("The installed FFmpeg executable failed its SHA-256 proof.".to_owned());
            }
            Ok(Some(executable))
        }
    }

    pub(crate) async fn ensure_installed(&self) -> Result<PathBuf, String> {
        if let Some(executable) = self.installed_executable()? {
            return Ok(executable);
        }
        let _install_guard = self.install_lock.lock().await;
        if let Some(executable) = self.installed_executable()? {
            return Ok(executable);
        }
        #[cfg(not(target_os = "windows"))]
        {
            return Err(
                "The managed FFmpeg integration has been runtime-verified on Windows x64 only."
                    .to_owned(),
            );
        }
        #[cfg(target_os = "windows")]
        {
            let client = reqwest::Client::builder()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(15 * 60))
                .https_only(true)
                .build()
                .map_err(|error| format!("Could not initialize the FFmpeg downloader: {error}"))?;
            let mut response = client
                .get(FFMPEG_ARCHIVE_URL)
                .send()
                .await
                .map_err(|error| format!("Could not download the FFmpeg integration: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "FFmpeg integration download returned HTTP {}.",
                    response.status()
                ));
            }
            if response
                .content_length()
                .is_some_and(|length| length == 0 || length > MAX_ARCHIVE_BYTES as u64)
            {
                return Err("The FFmpeg integration archive has an invalid size.".to_owned());
            }
            let mut archive = Vec::with_capacity(
                response
                    .content_length()
                    .unwrap_or_default()
                    .min(MAX_ARCHIVE_BYTES as u64) as usize,
            );
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                format!("Could not read the FFmpeg integration archive: {error}")
            })? {
                if chunk.len() > MAX_ARCHIVE_BYTES.saturating_sub(archive.len()) {
                    return Err("The FFmpeg integration archive is too large.".to_owned());
                }
                archive.extend_from_slice(&chunk);
            }
            if archive.is_empty() || archive.len() > MAX_ARCHIVE_BYTES {
                return Err("The FFmpeg integration archive is empty or too large.".to_owned());
            }
            let archive_hash = sha256_bytes(&archive);
            if archive_hash != FFMPEG_ARCHIVE_SHA256 {
                return Err(
                    "The FFmpeg integration archive failed SHA-256 verification.".to_owned(),
                );
            }
            let root = self.root.as_ref().clone();
            tauri::async_runtime::spawn_blocking(move || {
                install_verified_archive(&root, archive.as_ref(), &archive_hash)
            })
            .await
            .map_err(|error| format!("FFmpeg extraction task failed: {error}"))??;
            self.installed_executable()?.ok_or_else(|| {
                "FFmpeg installation completed without a valid executable proof.".to_owned()
            })
        }
    }
}

fn install_verified_archive(root: &Path, archive: &[u8], archive_hash: &str) -> Result<(), String> {
    let parent = root
        .parent()
        .ok_or_else(|| "The FFmpeg integration path has no parent.".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create the FFmpeg integration directory: {error}"))?;
    reject_symlink(parent)?;
    if root.exists() {
        return Err(
            "An incomplete FFmpeg integration directory already exists; remove it from iHub preferences before retrying."
                .to_owned(),
        );
    }
    let staging = parent.join(format!(".install-{}", Uuid::new_v4().simple()));
    fs::create_dir(&staging)
        .map_err(|error| format!("Could not create FFmpeg staging: {error}"))?;
    let result = (|| {
        let mut zip = ZipArchive::new(Cursor::new(archive))
            .map_err(|error| format!("The FFmpeg ZIP is invalid: {error}"))?;
        if zip.is_empty() || zip.len() > 512 {
            return Err("The FFmpeg ZIP has an invalid entry count.".to_owned());
        }
        let mut executable = None;
        let mut license = None;
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .map_err(|error| format!("Could not inspect FFmpeg ZIP entry: {error}"))?;
            let normalized = entry.name().replace('\\', "/");
            if normalized.ends_with("/bin/ffmpeg.exe") {
                if executable.is_some() || entry.size() == 0 || entry.size() > MAX_EXECUTABLE_BYTES
                {
                    return Err("The FFmpeg ZIP contains an invalid executable entry.".to_owned());
                }
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry
                    .by_ref()
                    .take(MAX_EXECUTABLE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("Could not extract ffmpeg.exe: {error}"))?;
                if bytes.len() as u64 != entry.size() {
                    return Err("The FFmpeg executable entry changed while extracting.".to_owned());
                }
                executable = Some(bytes);
            } else if (normalized.ends_with("/LICENSE") || normalized.ends_with("/LICENSE.txt"))
                && license.is_none()
                && entry.size() <= MAX_LICENSE_BYTES as u64
            {
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry
                    .by_ref()
                    .take(MAX_LICENSE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("Could not extract the FFmpeg license: {error}"))?;
                if bytes.len() <= MAX_LICENSE_BYTES {
                    license = Some(bytes);
                }
            }
        }
        let executable =
            executable.ok_or_else(|| "The FFmpeg ZIP has no ffmpeg.exe.".to_owned())?;
        let executable_hash = sha256_bytes(&executable);
        write_new_file(&staging.join("ffmpeg.exe"), &executable)?;
        write_new_file(
            &staging.join("LICENSE.txt"),
            license.as_deref().unwrap_or(
                b"FFmpeg essentials build by gyan.dev. FFmpeg is licensed under GPLv3; see https://ffmpeg.org/legal.html\n",
            ),
        )?;
        let proof = serde_json::to_vec_pretty(&FfmpegProof {
            version: FFMPEG_VERSION.to_owned(),
            archive_sha256: archive_hash.to_owned(),
            executable_sha256: executable_hash,
            source: FFMPEG_ARCHIVE_URL.to_owned(),
        })
        .map_err(|error| format!("Could not encode the FFmpeg proof: {error}"))?;
        write_new_file(&staging.join("proof.json"), &proof)?;
        fs::rename(&staging, root)
            .map_err(|error| format!("Could not publish the FFmpeg integration: {error}"))
    })();
    if result.is_err() {
        // This staging directory is created above and contains only these
        // exact host-owned files. Keep cleanup non-recursive so a malformed
        // archive can never broaden a destructive filesystem target.
        for name in ["ffmpeg.exe", "LICENSE.txt", "proof.json"] {
            let _ = fs::remove_file(staging.join(name));
        }
        let _ = fs::remove_dir(&staging);
    }
    result
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Could not create '{}': {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("Could not persist '{}': {error}", path.display()))
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect '{}': {error}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "FFmpeg integration directory '{}' is not a regular directory.",
            path.display()
        ));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path, maximum: u64) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect '{}': {error}", path.display()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err(format!(
            "'{}' is not a bounded regular file.",
            path.display()
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not open '{}': {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash '{}': {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
pub(crate) struct FfmpegControl {
    action: AtomicU8,
}

impl FfmpegControl {
    pub(crate) fn quit(&self) {
        let _ = self
            .action
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn kill(&self) {
        self.action.store(2, Ordering::Release);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FfmpegProgress {
    bitrate: String,
    fps: f64,
    frame: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<f64>,
    q: ValueNumberOrString,
    size: String,
    speed: String,
    time: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum ValueNumberOrString {
    Number(f64),
    String(String),
}

pub(crate) fn run<F>(
    executable: &Path,
    args: &[String],
    duration_seconds: Option<f64>,
    control: &FfmpegControl,
    mut on_progress: F,
) -> Result<(), String>
where
    F: FnMut(FfmpegProgress) + Send + 'static,
{
    ensure_regular_file(executable, MAX_EXECUTABLE_BYTES)?;
    if control.action.load(Ordering::Acquire) != 0 {
        return Err("uTools FFmpeg run was cancelled before the process started.".to_owned());
    }
    let mut command = background_command(executable);
    command
        .args(args)
        .args(["-nostats", "-progress", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start the verified FFmpeg integration: {error}"))?;
    let mut stdin = child.stdin.take();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "FFmpeg stdout was unavailable.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "FFmpeg stderr was unavailable.".to_owned())?;
    let progress_thread = thread::spawn(move || {
        let mut block = BTreeMap::<String, String>::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.len() > 16 * 1024 {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key == "progress" {
                if let Some(progress) = progress_from_block(&block, duration_seconds) {
                    on_progress(progress);
                }
                block.clear();
            } else if block.len() < 32 {
                block.insert(key.to_owned(), value.to_owned());
            }
        }
    });
    let error_thread = thread::spawn(move || {
        let mut tail = Vec::new();
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0_u8; 8 * 1024];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            tail.extend_from_slice(&buffer[..count]);
            if tail.len() > MAX_ERROR_BYTES {
                tail.drain(..tail.len() - MAX_ERROR_BYTES);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    });
    let mut quit_sent = false;
    let status = loop {
        match control.action.load(Ordering::Acquire) {
            2 => {
                let _ = child.kill();
            }
            1 if !quit_sent => {
                if let Some(writer) = stdin.as_mut() {
                    let _ = writer.write_all(b"q\n");
                    let _ = writer.flush();
                }
                quit_sent = true;
            }
            _ => {}
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Could not wait for FFmpeg: {error}"))?
        {
            break status;
        }
        thread::sleep(Duration::from_millis(50));
    };
    drop(stdin);
    let _ = progress_thread.join();
    let error = error_thread.join().unwrap_or_default();
    if control.action.load(Ordering::Acquire) == 2 {
        return Err("uTools FFmpeg run was killed.".to_owned());
    }
    if !status.success() {
        let error = error.trim();
        let summary = if error.is_empty() {
            "FFmpeg did not provide an error message.".to_owned()
        } else {
            error.chars().take(4_000).collect()
        };
        return Err(format!(
            "FFmpeg exited with code {}: {summary}",
            status
                .code()
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        ));
    }
    Ok(())
}

fn progress_from_block(
    block: &BTreeMap<String, String>,
    duration_seconds: Option<f64>,
) -> Option<FfmpegProgress> {
    let frame = block
        .get("frame")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let fps = block
        .get("fps")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0);
    let seconds = block
        .get("out_time_us")
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value / 1_000_000.0)
        .unwrap_or(0.0);
    let percent = duration_seconds
        .filter(|duration| duration.is_finite() && *duration > 0.0)
        .map(|duration| (seconds / duration * 100.0).clamp(0.0, 100.0));
    Some(FfmpegProgress {
        bitrate: block
            .get("bitrate")
            .cloned()
            .unwrap_or_else(|| "0kbits/s".to_owned()),
        fps,
        frame,
        percent,
        q: block
            .get("stream_0_0_q")
            .and_then(|value| value.parse::<f64>().ok())
            .map(ValueNumberOrString::Number)
            .unwrap_or_else(|| ValueNumberOrString::String("N/A".to_owned())),
        size: block
            .get("total_size")
            .and_then(|value| value.parse::<u64>().ok())
            .map(human_size)
            .unwrap_or_else(|| "0B".to_owned()),
        speed: block
            .get("speed")
            .cloned()
            .unwrap_or_else(|| "0x".to_owned()),
        time: block
            .get("out_time")
            .cloned()
            .unwrap_or_else(|| "00:00:00.000000".to_owned()),
    })
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::{progress_from_block, sha256_bytes, FFMPEG_ARCHIVE_SHA256};
    use std::collections::BTreeMap;

    #[test]
    fn pinned_archive_hash_is_exact_sha256() {
        assert_eq!(FFMPEG_ARCHIVE_SHA256.len(), 64);
        assert!(FFMPEG_ARCHIVE_SHA256
            .chars()
            .all(|value| value.is_ascii_hexdigit()));
        assert_eq!(
            sha256_bytes(b"iHub"),
            "9eee78f578b5b2e7a577f25f10e6383a2225cf73e8341ee83ba4c32b9111a19d"
        );
    }

    #[test]
    fn progress_projection_is_bounded_and_computes_optional_percent() {
        let block = BTreeMap::from([
            ("frame".to_owned(), "90".to_owned()),
            ("fps".to_owned(), "30.0".to_owned()),
            ("out_time_us".to_owned(), "3000000".to_owned()),
            ("out_time".to_owned(), "00:00:03.000000".to_owned()),
            ("total_size".to_owned(), "2048".to_owned()),
            ("speed".to_owned(), "1.5x".to_owned()),
        ]);
        let progress = progress_from_block(&block, Some(10.0)).unwrap();
        assert_eq!(progress.frame, 90);
        assert_eq!(progress.percent, Some(30.0));
        assert_eq!(progress.size, "2.0KiB");
    }
}
