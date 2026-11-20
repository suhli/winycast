use std::{marker::PhantomData, rc::Rc};

use anyhow::{Context, Result, ensure};
use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};

use crate::clipboard::{decode_png, private_text};

pub struct ImageRecognizer {
    engines: Vec<OcrEngine>,
    max_side: u32,
    _apartment: WinRtApartment,
}

struct WinRtApartment(PhantomData<Rc<()>>);

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

impl ImageRecognizer {
    pub fn new() -> Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.context("无法初始化 Windows OCR")?;
        let apartment = WinRtApartment(PhantomData);
        let max_side = OcrEngine::MaxImageDimension().context("Windows OCR 不可用")?;
        ensure!(max_side > 0, "Windows OCR 未报告受支持的图片尺寸");

        let mut engines: Vec<_> = OcrEngine::TryCreateFromUserProfileLanguages()
            .into_iter()
            .collect();
        let primary_tag = engines
            .first()
            .and_then(|engine| engine.RecognizerLanguage().ok())
            .and_then(|language| language.LanguageTag().ok())
            .map(|tag| tag.to_string_lossy());
        if !primary_tag
            .as_deref()
            .is_some_and(|tag| tag.eq_ignore_ascii_case("zh-Hans-CN"))
            && let Ok(language) = Language::CreateLanguage(&HSTRING::from("zh-Hans-CN"))
            && let Ok(chinese) = OcrEngine::TryCreateFromLanguage(&language)
        {
            engines.push(chinese);
        }
        if engines.is_empty() {
            let language = OcrEngine::AvailableRecognizerLanguages()?
                .GetAt(0)
                .context("Windows 未安装可用的 OCR 语言包")?;
            engines
                .push(OcrEngine::TryCreateFromLanguage(&language).context("无法启动 Windows OCR")?);
        }

        Ok(Self {
            engines,
            max_side,
            _apartment: apartment,
        })
    }

    pub fn recognize_png(&self, png: &[u8], cancelled: impl Fn() -> bool) -> Result<String> {
        if cancelled() {
            return Ok(String::new());
        }
        let image = decode_png(png)?;
        let image = if image.width().max(image.height()) > self.max_side {
            image.thumbnail(self.max_side, self.max_side)
        } else {
            image
        };
        let (width, height) = (image.width() as i32, image.height() as i32);
        let mut pixels = image.into_rgba8().into_raw();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        let writer = DataWriter::new()?;
        writer.WriteBytes(&pixels)?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
            &writer.DetachBuffer()?,
            BitmapPixelFormat::Bgra8,
            width,
            height,
            BitmapAlphaMode::Ignore,
        )?;
        let mut texts = Vec::new();
        for engine in &self.engines {
            if cancelled() {
                return Ok(String::new());
            }
            let result = engine.RecognizeAsync(&bitmap)?.get()?;
            let text = result.Text()?.to_string_lossy();
            let text = text.trim();
            let lines = result.Lines()?;
            let mut line_text = Vec::new();
            for line in lines {
                line_text.push(line.Text()?.to_string_lossy());
            }
            if private_text(text) || private_text(&line_text.join("\n")) {
                return Ok(String::new());
            }
            if !text.is_empty() && !texts.iter().any(|seen| seen == text) {
                texts.push(text.to_owned());
            }
        }
        Ok(texts.join("\n"))
    }
}
