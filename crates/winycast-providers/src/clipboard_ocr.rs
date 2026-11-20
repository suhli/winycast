use anyhow::Result;
use tokio_util::sync::CancellationToken;
use winycast_core::ClipboardData;
use winycast_platform_windows::ImageRecognizer;
use winycast_storage::Storage;

pub fn index_clipboard_images(storage: &Storage, cancel: &CancellationToken) -> Result<usize> {
    if cancel.is_cancelled() {
        return Ok(0);
    }
    let images = storage.pending_image_ocr()?;
    if images.is_empty() || cancel.is_cancelled() {
        return Ok(0);
    }

    let recognizer = ImageRecognizer::new()?;
    let mut indexed = 0;
    let mut first_error = None;
    for id in images {
        if cancel.is_cancelled() {
            break;
        }
        let Some(ClipboardData::Png(png)) = storage.clip(id)? else {
            continue;
        };
        match recognizer.recognize_png(&png, || cancel.is_cancelled()) {
            Ok(text) => {
                if storage.save_image_ocr(id, &text, || cancel.is_cancelled())? {
                    indexed += 1;
                }
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }

    if indexed == 0
        && !cancel.is_cancelled()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    Ok(indexed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use winycast_core::Settings;

    #[test]
    #[ignore = "requires Windows OCR support and the English and Simplified Chinese OCR languages"]
    fn indexes_synthetic_screenshot_with_windows_ocr() -> Result<()> {
        let storage = Storage::open(Path::new(":memory:"))?;
        storage.save_settings(&Settings {
            clipboard_ocr: true,
            ..Settings::default()
        })?;
        let fixture = include_bytes!("../tests/fixtures/ocr-bilingual.png");
        storage.add_clip(ClipboardData::Png(fixture.to_vec()), 1, 30)?;
        assert!(storage.clips("Winycast OCR test", || false)?.is_empty());
        assert!(storage.clips("中文识别", || false)?.is_empty());

        assert_eq!(
            index_clipboard_images(&storage, &CancellationToken::new())?,
            1
        );
        assert_eq!(storage.clips("Winycast OCR test", || false)?.len(), 1);
        assert_eq!(
            storage.clips("中文识别", || false)?.len(),
            1,
            "recognized fixture text: {:?}",
            ImageRecognizer::new()?.recognize_png(fixture, || false)?
        );
        eprintln!("Windows OCR recognized English and Chinese search terms");
        Ok(())
    }
}
