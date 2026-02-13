// Audio Processing Module
//
// Provides audio file format detection, validation, and encoding
// for the audio transcription handler.
//
// Requirements covered:
// - 7.3: Audio transcription via /v1/audio/transcriptions

use base64::{engine::general_purpose, Engine as _};
use std::path::Path;

/// Maximum audio file size: 15 MB
pub const MAX_AUDIO_SIZE: usize = 15 * 1024 * 1024;

/// Supported audio format descriptor
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFormat {
    /// File extension (lowercase, without dot)
    pub extension: &'static str,
    /// MIME type string
    pub mime_type: &'static str,
}

/// All supported audio formats
const SUPPORTED_FORMATS: &[AudioFormat] = &[
    AudioFormat { extension: "mp3", mime_type: "audio/mp3" },
    AudioFormat { extension: "wav", mime_type: "audio/wav" },
    AudioFormat { extension: "m4a", mime_type: "audio/aac" },
    AudioFormat { extension: "ogg", mime_type: "audio/ogg" },
    AudioFormat { extension: "flac", mime_type: "audio/flac" },
    AudioFormat { extension: "aiff", mime_type: "audio/aiff" },
    AudioFormat { extension: "aif", mime_type: "audio/aiff" },
    AudioFormat { extension: "webm", mime_type: "audio/webm" },
];

/// Audio processing utilities for transcription requests.
pub struct AudioProcessor;

impl AudioProcessor {
    /// Detect the MIME type from a filename's extension.
    ///
    /// Returns the MIME type string on success, or an error message
    /// if the extension is missing or unsupported.
    pub fn detect_mime_type(filename: &str) -> Result<String, String> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("Cannot determine file extension: {}", filename))?;

        let ext_lower = ext.to_lowercase();
        SUPPORTED_FORMATS
            .iter()
            .find(|f| f.extension == ext_lower)
            .map(|f| f.mime_type.to_string())
            .ok_or_else(|| format!("Unsupported audio format: {}", ext))
    }

    /// Encode raw audio bytes to base64.
    pub fn encode_to_base64(audio_data: &[u8]) -> String {
        general_purpose::STANDARD.encode(audio_data)
    }

    /// Check if the given byte count exceeds the maximum allowed audio file size.
    pub fn exceeds_size_limit(size_bytes: usize) -> bool {
        size_bytes > MAX_AUDIO_SIZE
    }

    /// Return a list of all supported file extensions.
    pub fn supported_extensions() -> Vec<&'static str> {
        SUPPORTED_FORMATS.iter().map(|f| f.extension).collect()
    }

    /// Build the Gemini-compatible inline data payload for an audio file.
    ///
    /// This validates the file, detects the MIME type, and encodes the data.
    pub fn prepare_inline_data(
        filename: &str,
        audio_bytes: &[u8],
    ) -> Result<AudioInlineData, String> {
        let mime_type = Self::detect_mime_type(filename)?;

        if Self::exceeds_size_limit(audio_bytes.len()) {
            let size_mb = audio_bytes.len() as f64 / (1024.0 * 1024.0);
            return Err(format!(
                "Audio file too large ({:.1} MB). Max: {} MB",
                size_mb,
                MAX_AUDIO_SIZE / (1024 * 1024)
            ));
        }

        let base64_data = Self::encode_to_base64(audio_bytes);

        Ok(AudioInlineData {
            mime_type,
            data: base64_data,
        })
    }
}

/// Prepared audio data ready for Gemini API inline data format.
#[derive(Debug, Clone)]
pub struct AudioInlineData {
    /// The detected MIME type
    pub mime_type: String,
    /// Base64-encoded audio data
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- detect_mime_type tests ---

    #[test]
    fn test_detect_mime_type_mp3() {
        assert_eq!(AudioProcessor::detect_mime_type("song.mp3").unwrap(), "audio/mp3");
    }

    #[test]
    fn test_detect_mime_type_wav() {
        assert_eq!(AudioProcessor::detect_mime_type("recording.wav").unwrap(), "audio/wav");
    }

    #[test]
    fn test_detect_mime_type_m4a() {
        assert_eq!(AudioProcessor::detect_mime_type("voice.m4a").unwrap(), "audio/aac");
    }

    #[test]
    fn test_detect_mime_type_ogg() {
        assert_eq!(AudioProcessor::detect_mime_type("clip.ogg").unwrap(), "audio/ogg");
    }

    #[test]
    fn test_detect_mime_type_flac() {
        assert_eq!(AudioProcessor::detect_mime_type("music.flac").unwrap(), "audio/flac");
    }

    #[test]
    fn test_detect_mime_type_aiff() {
        assert_eq!(AudioProcessor::detect_mime_type("sample.aiff").unwrap(), "audio/aiff");
    }

    #[test]
    fn test_detect_mime_type_aif() {
        assert_eq!(AudioProcessor::detect_mime_type("sample.aif").unwrap(), "audio/aiff");
    }

    #[test]
    fn test_detect_mime_type_webm() {
        assert_eq!(AudioProcessor::detect_mime_type("video.webm").unwrap(), "audio/webm");
    }

    #[test]
    fn test_detect_mime_type_case_insensitive() {
        assert_eq!(AudioProcessor::detect_mime_type("TEST.MP3").unwrap(), "audio/mp3");
        assert_eq!(AudioProcessor::detect_mime_type("Audio.WAV").unwrap(), "audio/wav");
        assert_eq!(AudioProcessor::detect_mime_type("file.FLAC").unwrap(), "audio/flac");
    }

    #[test]
    fn test_detect_mime_type_unsupported() {
        assert!(AudioProcessor::detect_mime_type("doc.txt").is_err());
        assert!(AudioProcessor::detect_mime_type("image.png").is_err());
        assert!(AudioProcessor::detect_mime_type("video.mp4").is_err());
    }

    #[test]
    fn test_detect_mime_type_no_extension() {
        assert!(AudioProcessor::detect_mime_type("noext").is_err());
    }

    #[test]
    fn test_detect_mime_type_path_with_dirs() {
        assert_eq!(
            AudioProcessor::detect_mime_type("/path/to/audio.mp3").unwrap(),
            "audio/mp3"
        );
    }

    // --- exceeds_size_limit tests ---

    #[test]
    fn test_exceeds_size_limit_under() {
        assert!(!AudioProcessor::exceeds_size_limit(10 * 1024 * 1024));
    }

    #[test]
    fn test_exceeds_size_limit_exact() {
        assert!(!AudioProcessor::exceeds_size_limit(MAX_AUDIO_SIZE));
    }

    #[test]
    fn test_exceeds_size_limit_over_by_one() {
        assert!(AudioProcessor::exceeds_size_limit(MAX_AUDIO_SIZE + 1));
    }

    #[test]
    fn test_exceeds_size_limit_way_over() {
        assert!(AudioProcessor::exceeds_size_limit(20 * 1024 * 1024));
    }

    #[test]
    fn test_exceeds_size_limit_zero() {
        assert!(!AudioProcessor::exceeds_size_limit(0));
    }

    // --- encode_to_base64 tests ---

    #[test]
    fn test_encode_to_base64_basic() {
        let data = b"test audio data";
        let encoded = AudioProcessor::encode_to_base64(data);
        assert!(!encoded.is_empty());
        // Verify round-trip
        let decoded = general_purpose::STANDARD.decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_encode_to_base64_empty() {
        let encoded = AudioProcessor::encode_to_base64(b"");
        assert_eq!(encoded, "");
    }

    // --- supported_extensions tests ---

    #[test]
    fn test_supported_extensions_contains_common_formats() {
        let exts = AudioProcessor::supported_extensions();
        assert!(exts.contains(&"mp3"));
        assert!(exts.contains(&"wav"));
        assert!(exts.contains(&"ogg"));
        assert!(exts.contains(&"flac"));
        assert!(exts.contains(&"m4a"));
        assert!(exts.contains(&"webm"));
    }

    // --- prepare_inline_data tests ---

    #[test]
    fn test_prepare_inline_data_success() {
        let data = vec![0u8; 1024]; // 1KB
        let result = AudioProcessor::prepare_inline_data("test.mp3", &data);
        assert!(result.is_ok());
        let inline = result.unwrap();
        assert_eq!(inline.mime_type, "audio/mp3");
        assert!(!inline.data.is_empty());
    }

    #[test]
    fn test_prepare_inline_data_unsupported_format() {
        let data = vec![0u8; 100];
        let result = AudioProcessor::prepare_inline_data("test.txt", &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_prepare_inline_data_too_large() {
        let data = vec![0u8; MAX_AUDIO_SIZE + 1];
        let result = AudioProcessor::prepare_inline_data("test.mp3", &data);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too large"));
    }

    #[test]
    fn test_prepare_inline_data_at_limit() {
        let data = vec![0u8; MAX_AUDIO_SIZE];
        let result = AudioProcessor::prepare_inline_data("test.wav", &data);
        assert!(result.is_ok());
    }
}
