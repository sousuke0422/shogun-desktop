use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum UploadState {
    Idle,
    InProgress { done: usize, total: usize },
    Done { success: usize, failed: usize },
    Error(String),
}

/// Returns true when `path` has a known image file extension.
pub fn is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "heic")
    )
}

/// Build a timestamped remote filename for screenshot uploads.
pub fn remote_filename(local: &Path, index: usize) -> String {
    let ext = local.extension().and_then(|e| e.to_str()).unwrap_or("png");
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    format!("screenshot_{ts}_{index:02}.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_recognizes_common_extensions() {
        assert!(is_image(Path::new("/tmp/a.PNG")));
        assert!(is_image(Path::new("photo.jpeg")));
        assert!(!is_image(Path::new("readme.txt")));
    }

    #[test]
    fn remote_filename_includes_index_and_extension() {
        let name = remote_filename(Path::new("/tmp/shot.jpg"), 3);
        assert!(name.starts_with("screenshot_"));
        assert!(name.ends_with(".jpg"));
        assert!(name.contains("_03."));
    }
}
