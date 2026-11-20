use std::{io::Cursor, sync::LazyLock, thread, time::Duration};

use anyhow::{Result, anyhow, bail};
use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, codecs::bmp::BmpDecoder,
};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND},
        System::{
            DataExchange::*,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Threading::GetCurrentProcessId,
        },
        UI::WindowsAndMessaging::GetWindowThreadProcessId,
    },
    core::w,
};
use winycast_core::ClipboardData;

use crate::shell::process_name;

const UNICODE_TEXT: u32 = 13;
const DIB: u32 = 8;
const DIB_V5: u32 = 17;
const FILE_LIST: u32 = 15;
const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_ENCODED_BYTES: usize = 12 * 1024 * 1024;
const MAX_DIB_BYTES: usize = 64 * 1024 * 1024 + 512;

struct Formats {
    png: u32,
    excluded: u32,
    os_history: u32,
    password_manager: u32,
}

static FORMATS: LazyLock<Formats> = LazyLock::new(|| unsafe {
    Formats {
        png: RegisterClipboardFormatW(w!("PNG")),
        excluded: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
        os_history: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
        password_manager: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
    }
});

struct Opened;
impl Opened {
    fn open(owner: Option<HWND>) -> Result<Self> {
        for delay in [0, 8, 16, 32, 48] {
            if delay != 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Ok(Self);
            }
        }
        Err(anyhow!("剪贴板正被其他程序占用"))
    }

    fn data(&self, format: u32, maximum: usize) -> Option<Vec<u8>> {
        if !has(format) {
            return None;
        }
        unsafe {
            let handle = HGLOBAL(GetClipboardData(format).ok()?.0);
            let size = GlobalSize(handle);
            if size == 0 || size > maximum {
                return None;
            }
            let pointer = GlobalLock(handle).cast::<u8>();
            if pointer.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
            let _ = GlobalUnlock(handle);
            Some(bytes)
        }
    }

    fn text(&self) -> Option<String> {
        let bytes = self.data(UNICODE_TEXT, MAX_TEXT_BYTES)?;
        let wide: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        String::from_utf16(&wide).ok()
    }
}
impl Drop for Opened {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn has(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format).is_ok() }
}

fn private_source() -> bool {
    unsafe {
        let Ok(owner) = GetClipboardOwner() else {
            return true;
        };
        let mut pid = 0;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return true;
        }
        let Some(name) = process_name(pid) else {
            return true;
        };
        let name = name.to_ascii_lowercase();
        [
            "keepass",
            "1password",
            "bitwarden",
            "lastpass",
            "enpass",
            "dashlane",
            "protonpass",
            "credentialui",
            "authenticator",
        ]
        .iter()
        .any(|pattern| name.contains(pattern))
    }
}

fn private_text(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let digits_only = (4..=8).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
    digits_only
        || lower.contains("-----begin ") && lower.contains("private key-----")
        || lower.split(['\r', '\n']).any(|line| {
            let line =
                line.trim_start_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
            [
                "password",
                "passwd",
                "api_key",
                "api-key",
                "client_secret",
                "access_token",
                "authorization",
                "aws_secret_access_key",
            ]
            .iter()
            .any(|prefix| {
                line.strip_prefix(prefix).is_some_and(|tail| {
                    tail.trim_start_matches([' ', '\t', '"', '\''])
                        .starts_with(['=', ':'])
                })
            })
        })
        || [
            "ghp_",
            "github_pat_",
            "sk-proj-",
            "xoxb-",
            "xoxp-",
            "akai",
            "akia",
        ]
        .iter()
        .any(|prefix| {
            lower.starts_with(prefix) && text.len() >= 20 && !text.contains(char::is_whitespace)
        })
        || text.starts_with("eyJ")
            && text.split('.').count() == 3
            && !text.contains(char::is_whitespace)
}

pub fn capture(sequence: u32) -> Result<Option<ClipboardData>> {
    if unsafe { GetClipboardSequenceNumber() } != sequence {
        return Ok(None);
    }
    let clipboard = Opened::open(None)?;
    if unsafe { GetClipboardSequenceNumber() } != sequence
        || private_source()
        || has(FILE_LIST)
        || has(FORMATS.excluded)
        || has(FORMATS.password_manager)
        || (has(FORMATS.os_history)
            && clipboard
                .data(FORMATS.os_history, 256)
                .is_none_or(|info| info.get(..4) != Some(&1u32.to_le_bytes())))
    {
        return Ok(None);
    }

    let png = clipboard.data(FORMATS.png, MAX_ENCODED_BYTES);
    let dib = if png.is_none() {
        clipboard
            .data(DIB_V5, MAX_DIB_BYTES)
            .or_else(|| clipboard.data(DIB, MAX_DIB_BYTES))
    } else {
        None
    };
    let text = if png.is_none() && dib.is_none() {
        clipboard.text()
    } else {
        None
    };
    drop(clipboard);
    if let Some(png) = png {
        decode_png(&png)?;
        return Ok(Some(ClipboardData::Png(png)));
    }
    if let Some(dib) = dib {
        return Ok(Some(ClipboardData::Png(dib_to_png(dib)?)));
    }
    Ok(text
        .filter(|text| !text.trim().is_empty() && !private_text(text))
        .map(ClipboardData::Text))
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    limits
}

fn decode_png(bytes: &[u8]) -> Result<DynamicImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(limits());
    Ok(reader.decode()?)
}

fn dib_to_png(dib: Vec<u8>) -> Result<Vec<u8>> {
    let mut decoder = BmpDecoder::new_without_file_header(Cursor::new(dib))?;
    decoder.set_limits(limits())?;
    let image = DynamicImage::from_decoder(decoder)?;
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png)?;
    let bytes = encoded.into_inner();
    if bytes.len() > MAX_ENCODED_BYTES {
        bail!("剪贴板图片超过 12 MiB");
    }
    Ok(bytes)
}

pub fn read_text() -> Result<Option<String>> {
    Ok(Opened::open(None)?.text())
}

pub fn thumbnail(png: &[u8]) -> Result<Vec<u8>> {
    let size = decode_png(png)?.thumbnail(390, 300);
    let mut bytes = Cursor::new(Vec::new());
    size.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

#[derive(Clone, Copy)]
pub struct ClipboardWriter(pub(crate) isize);

impl ClipboardWriter {
    pub fn write(self, data: ClipboardData) -> Result<()> {
        let formats = match data {
            ClipboardData::Text(text) => {
                let wide: Vec<_> = text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                vec![(UNICODE_TEXT, wide)]
            }
            ClipboardData::Png(png) => {
                let image = decode_png(&png)?;
                let mut bmp = Cursor::new(Vec::new());
                // The 24-bit DIB is for applications that cannot paste PNG.
                DynamicImage::ImageRgb8(image.into_rgb8()).write_to(&mut bmp, ImageFormat::Bmp)?;
                let mut dib = bmp.into_inner();
                dib.drain(..14);
                vec![(DIB, dib), (FORMATS.png, png)]
            }
        };
        let buffers = formats
            .iter()
            .map(|(format, bytes)| Ok((*format, Buffer::new(bytes)?)))
            .collect::<Result<Vec<_>>>()?;
        let _clipboard = Opened::open(Some(HWND(self.0 as _)))?;
        unsafe {
            EmptyClipboard()?;
        }
        for (format, buffer) in buffers {
            if format != 0 {
                buffer.publish(format)?;
            }
        }
        Ok(())
    }
}

struct Buffer(HGLOBAL);
impl Buffer {
    fn new(bytes: &[u8]) -> Result<Self> {
        unsafe {
            let buffer = Self(GlobalAlloc(GMEM_MOVEABLE, bytes.len())?);
            let dest = GlobalLock(buffer.0).cast::<u8>();
            if dest.is_null() {
                return Err(windows::core::Error::from_win32().into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len());
            let _ = GlobalUnlock(buffer.0);
            Ok(buffer)
        }
    }

    fn publish(self, format: u32) -> Result<()> {
        unsafe {
            SetClipboardData(format, Some(HANDLE(self.0.0)))?;
        }
        std::mem::forget(self); // The clipboard now owns the HGLOBAL.
        Ok(())
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_sensitive_and_ordinary_text() {
        assert!(private_text("123456"));
        assert!(private_text("Authorization: Bearer sample"));
        assert!(private_text("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(private_text("user=x\npassword = sample"));
        assert!(!private_text("会议安排 123456"));
        assert!(!private_text("docs about access_token"));
    }

    #[test]
    fn dib_and_png_round_trip() -> Result<()> {
        let bitmap =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([12, 55, 200])));
        let mut bmp = Cursor::new(Vec::new());
        bitmap.write_to(&mut bmp, ImageFormat::Bmp)?;
        let png = dib_to_png(bmp.into_inner()[14..].to_vec())?;
        assert_eq!(decode_png(&png)?.into_rgb8(), bitmap.into_rgb8());
        Ok(())
    }
}
