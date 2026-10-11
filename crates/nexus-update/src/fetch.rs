//! Downloading, with the manifest's hash as the only thing trusted.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;

use crate::manifest::{Artifact, SignedManifest, UpdateError};

/// Largest manifest accepted.
const MAX_MANIFEST: usize = 256 * 1024;
/// Largest artifact accepted (a sanity cap on top of the signed size).
const MAX_ARTIFACT: u64 = 1 << 30;

fn client() -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(concat!("nexus-update/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| UpdateError::Download(e.to_string()))
}

/// Fetches the signed manifest at `url`. Only the bytes are fetched here; verification is the
/// caller's next step.
pub async fn fetch_manifest(url: &str) -> Result<SignedManifest, UpdateError> {
    let resp = client()?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    if bytes.len() > MAX_MANIFEST {
        return Err(UpdateError::Malformed("manifest is too large".into()));
    }
    serde_json::from_slice(&bytes).map_err(|e| UpdateError::Malformed(e.to_string()))
}

/// Downloads `artifact` into `dir`, checking size and BLAKE3 as it arrives. On any mismatch the
/// partial file is deleted and nothing is left behind. Returns the staged path.
pub async fn download(artifact: &Artifact, dir: &Path) -> Result<PathBuf, UpdateError> {
    download_from(&artifact.url, artifact, dir).await
}

/// Like [`download`] but from an explicit URL (mirrors; tests).
pub async fn download_from(
    url: &str,
    artifact: &Artifact,
    dir: &Path,
) -> Result<PathBuf, UpdateError> {
    if artifact.size > MAX_ARTIFACT {
        return Err(UpdateError::Download(
            "the artifact is implausibly large".into(),
        ));
    }
    let want = parse_hash(&artifact.blake3)?;
    tokio::fs::create_dir_all(dir).await?;
    let name = Path::new(&artifact.url)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && !n.contains(".."))
        .unwrap_or("update.bin");
    let path = dir.join(name);
    let part = dir.join(format!("{name}.part"));

    let mut resp = client()?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| UpdateError::Download(e.to_string()))?;
    let mut file = tokio::fs::File::create(&part).await?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let outcome: Result<(), UpdateError> = async {
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| UpdateError::Download(e.to_string()))?
        {
            total += chunk.len() as u64;
            if total > artifact.size {
                return Err(UpdateError::HashMismatch);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = outcome {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    if total != artifact.size || *hasher.finalize().as_bytes() != want {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(UpdateError::HashMismatch);
    }
    tokio::fs::rename(&part, &path).await?;
    Ok(path)
}

fn parse_hash(hex: &str) -> Result<[u8; 32], UpdateError> {
    if hex.len() != 64 {
        return Err(UpdateError::Malformed(
            "blake3 must be 64 hex digits".into(),
        ));
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| UpdateError::Malformed("blake3 is not hex".into()))?;
    }
    Ok(out)
}
