use std::path::PathBuf;

pub const ASR_ARCHIVE_NAME: &str =
    "sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2";

pub const ASR_MODEL_DIR_NAME: &str =
    "sherpa-onnx-streaming-zipformer-en-2023-06-26";

const ASR_DOWNLOAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2";

/// Parent directory under `app_data_dir` where ASR models live.
pub fn asr_models_dir() -> PathBuf {
    crate::app_data_dir().join("asr_models")
}

/// On-disk path of the cached tar.bz2 archive.
pub fn archive_path() -> PathBuf {
    asr_models_dir().join(ASR_ARCHIVE_NAME)
}

/// Absolute path to the extracted sherpa-onnx streaming zipformer model.
pub fn sherpa_model_dir() -> PathBuf {
    asr_models_dir().join(ASR_MODEL_DIR_NAME)
}

/// True once the archive has been downloaded and saved to disk.
pub fn archive_cached() -> bool {
    archive_path().is_file()
}

/// True once the model directory exists and contains `tokens.txt`.
pub fn model_present() -> bool {
    let dir = sherpa_model_dir();
    dir.is_dir() && dir.join("tokens.txt").is_file()
}

/// URL the tar.bz2 archive is fetched from (via `cx.http_request`).
pub fn download_url() -> &'static str {
    ASR_DOWNLOAD_URL
}

/// Save the raw archive bytes to disk atomically, then extract the model.
///
/// Writing to `.tmp` first means a crash during write never leaves a corrupt
/// archive at `archive_path()`.  Called from a background thread.
pub fn save_and_extract(bytes: Vec<u8>) -> Result<(), String> {
    let dest_dir = asr_models_dir();
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("create dir {}: {e}", dest_dir.display()))?;

    // Atomically write the archive so it can be re-used on restart.
    let archive = archive_path();
    let tmp = archive.with_extension("tar.bz2.tmp");
    std::fs::write(&tmp, &bytes)
        .map_err(|e| format!("write archive to {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &archive)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), archive.display()))?;

    extract_from_disk()
}

/// Extract the cached archive from disk into `asr_models_dir()`.
///
/// Call this when `archive_cached()` is true but `model_present()` is false
/// (e.g., extraction was interrupted on a previous run).
pub fn extract_from_disk() -> Result<(), String> {
    use bzip2::read::BzDecoder;
    use tar::Archive;

    let archive = archive_path();
    let file = std::fs::File::open(&archive)
        .map_err(|e| format!("open cached archive {}: {e}", archive.display()))?;
    let dest = asr_models_dir();
    let decoder = BzDecoder::new(std::io::BufReader::new(file));
    let mut ar = Archive::new(decoder);
    ar.unpack(&dest)
        .map_err(|e| format!("extract sherpa model archive: {e}"))?;

    if !model_present() {
        return Err(format!(
            "extraction finished but tokens.txt not found in {}",
            sherpa_model_dir().display()
        ));
    }
    Ok(())
}
