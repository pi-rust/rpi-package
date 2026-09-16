pub struct FileUploadOptions {
    pub file_name: String,
    pub content_type: Option<String>,
    pub block_size: usize,
}

impl FileUploadOptions {
    pub fn new(file_name: impl Into<String>) -> Self {
        Self {
            file_name: file_name.into(),
            content_type: None,
            block_size: 4 * 1024 * 1024,
        }
    }

    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    pub fn block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }
}

impl Default for FileUploadOptions {
    fn default() -> Self {
        Self {
            file_name: "file".to_string(),
            content_type: None,
            block_size: 4 * 1024 * 1024,
        }
    }
}

pub struct FileDownloadOptions {
    pub block_size: usize,
}

impl FileDownloadOptions {
    pub fn new() -> Self {
        Self {
            block_size: 4 * 1024 * 1024,
        }
    }

    pub fn block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }
}

impl Default for FileDownloadOptions {
    fn default() -> Self {
        Self::new()
    }
}

pub fn guess_content_type(file_name: &str) -> Option<&'static str> {
    let ext = file_name.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "txt" => Some("text/plain"),
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "js" => Some("application/javascript"),
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "pdf" => Some("application/pdf"),
        "zip" => Some("application/zip"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "svg" => Some("image/svg+xml"),
        "mp3" => Some("audio/mpeg"),
        "mp4" => Some("video/mp4"),
        "xlsx" | "xls" => Some("application/vnd.ms-excel"),
        "docx" | "doc" => Some("application/msword"),
        "pptx" | "ppt" => Some("application/vnd.ms-powerpoint"),
        _ => None,
    }
}

pub fn format_file_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.2} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.2} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.2} KB", size as f64 / KB as f64)
    } else {
        format!("{} B", size)
    }
}

pub fn parse_file_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = if s.ends_with("GB") || s.ends_with("Gb") || s.ends_with("gb") {
        (s.trim_end_matches(['G', 'B', 'b']), 3)
    } else if s.ends_with("MB") || s.ends_with("Mb") || s.ends_with("mb") {
        (s.trim_end_matches(['M', 'B', 'b']), 2)
    } else if s.ends_with("KB") || s.ends_with("Kb") || s.ends_with("kb") {
        (s.trim_end_matches(['K', 'B', 'b']), 1)
    } else {
        (s, 0)
    };

    let value: u64 = num.parse().ok()?;
    match unit {
        3 => Some(value * 1024 * 1024 * 1024),
        2 => Some(value * 1024 * 1024),
        1 => Some(value * 1024),
        _ => Some(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guess_content_type() {
        assert_eq!(guess_content_type("file.txt"), Some("text/plain"));
        assert_eq!(guess_content_type("file.json"), Some("application/json"));
        assert_eq!(guess_content_type("file.png"), Some("image/png"));
        assert_eq!(guess_content_type("file.unknown"), None);
        assert_eq!(guess_content_type("noextension"), None);
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1024), "1.00 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_file_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_parse_file_size() {
        assert_eq!(parse_file_size("1024"), Some(1024));
        assert_eq!(parse_file_size("1KB"), Some(1024));
        assert_eq!(parse_file_size("1MB"), Some(1024 * 1024));
        assert_eq!(parse_file_size("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_file_size("invalid"), None);
    }

    #[test]
    fn test_file_upload_options() {
        let opts = FileUploadOptions::new("test.txt")
            .content_type("text/plain")
            .block_size(1024);

        assert_eq!(opts.file_name, "test.txt");
        assert_eq!(opts.content_type, Some("text/plain".to_string()));
        assert_eq!(opts.block_size, 1024);
    }
}
