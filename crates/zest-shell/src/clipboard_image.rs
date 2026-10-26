//! "Copy as PNG/JPG": files plus pixels on the clipboard.
//!
//! The clipboard gets the files (`CF_HDROP` with `Preferred DropEffect` = COPY, so
//! Paste in Explorer copies them). For one image it also gets the pixels, so that
//! Paste works in chat programs and image editors:
//! - `CF_DIBV5` (32-bit BGRA with alpha), decoded from the output file with WIC.
//!   Windows adds `CF_DIB` and `CF_BITMAP` from it.
//! - the registered format `PNG` with the bytes of the file, if the file is a PNG.
//!   Programs that keep transparency (Slack, Discord, Chrome, Office) read this one.

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, HWND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{EmptyClipboard, RegisterClipboardFormatW};
use windows::Win32::System::Ole::{CF_DIBV5, CF_HDROP, DROPEFFECT_COPY};

use crate::actions::{build_dropfiles, open_clipboard, set_clipboard, Clip};
use crate::{wide_path, wide_z, ComScope, Result, ShellError};

/// Largest image that is put on the clipboard as pixels (width x height).
const MAX_PIXELS: u64 = 120_000_000;
const BITMAPV5HEADER_SIZE: usize = 124;
const BI_BITFIELDS: u32 = 3;
/// `LCS_sRGB` ("sRGB" as a little-endian tag).
const LCS_SRGB: u32 = 0x7352_4742;

/// The bytes of a `CF_DIBV5` block: a `BITMAPV5HEADER` (124 bytes) and the pixels
/// bottom-up. `bgra` holds `width * height` pixels, top row first, 4 bytes each.
pub fn build_dibv5(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    debug_assert_eq!(bgra.len(), stride * height as usize);
    let mut out = Vec::with_capacity(BITMAPV5HEADER_SIZE + bgra.len());
    let put32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    put32(&mut out, BITMAPV5HEADER_SIZE as u32); // bV5Size
    put32(&mut out, width); // bV5Width
    put32(&mut out, height); // bV5Height (positive = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // bV5Planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bV5BitCount
    put32(&mut out, BI_BITFIELDS); // bV5Compression
    put32(&mut out, bgra.len() as u32); // bV5SizeImage
    put32(&mut out, 0); // bV5XPelsPerMeter
    put32(&mut out, 0); // bV5YPelsPerMeter
    put32(&mut out, 0); // bV5ClrUsed
    put32(&mut out, 0); // bV5ClrImportant
    put32(&mut out, 0x00FF_0000); // bV5RedMask
    put32(&mut out, 0x0000_FF00); // bV5GreenMask
    put32(&mut out, 0x0000_00FF); // bV5BlueMask
    put32(&mut out, 0xFF00_0000); // bV5AlphaMask
    put32(&mut out, LCS_SRGB); // bV5CSType
    out.extend_from_slice(&[0u8; 36]); // bV5Endpoints (CIEXYZTRIPLE)
    put32(&mut out, 0); // bV5GammaRed
    put32(&mut out, 0); // bV5GammaGreen
    put32(&mut out, 0); // bV5GammaBlue
    put32(&mut out, 4); // bV5Intent = LCS_GM_IMAGES
    put32(&mut out, 0); // bV5ProfileData
    put32(&mut out, 0); // bV5ProfileSize
    put32(&mut out, 0); // bV5Reserved
    debug_assert_eq!(out.len(), BITMAPV5HEADER_SIZE);
    for row in (0..height as usize).rev() {
        out.extend_from_slice(&bgra[row * stride..(row + 1) * stride]);
    }
    out
}

/// Decode an image file to 32-bit BGRA, top row first (WIC: PNG, JPEG, ...).
/// Starts COM on the calling thread if needed.
pub fn load_bgra(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let _com = ComScope::new();
    let name = wide_path(path);
    // SAFETY: COM calls with valid arguments; the NUL-terminated name outlives them.
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let decoder = factory.CreateDecoderFromFilename(
            PCWSTR(name.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(ShellError::Invalid(format!("image size {w} x {h} is not supported")));
        }
        let mut buf = vec![0u8; w as usize * h as usize * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
        Ok((w, h, buf))
    }
}

/// Put `files` on the clipboard as a file list. If `image` is given, add the pixels
/// of that image too (see the module text). Decoding runs on the calling thread
/// before the clipboard is opened. `owner` is any window of this process.
pub fn copy_files_and_image(owner: HWND, files: &[PathBuf], image: Option<&Path>) -> Result<()> {
    if files.is_empty() {
        return Err(ShellError::Invalid("nothing to copy".into()));
    }
    let drop = build_dropfiles(files);
    let effect = DROPEFFECT_COPY.0.to_le_bytes();
    let pixels = match image {
        Some(p) => {
            let (w, h, bgra) = load_bgra(p)?;
            let png = if p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            {
                std::fs::read(p).ok()
            } else {
                None
            };
            Some((build_dibv5(w, h, &bgra), png))
        }
        None => None,
    };
    let effect_name = wide_z("Preferred DropEffect");
    let png_name = wide_z("PNG");
    // SAFETY: NUL-terminated names.
    let (effect_format, png_format) = unsafe {
        (
            RegisterClipboardFormatW(PCWSTR(effect_name.as_ptr())),
            RegisterClipboardFormatW(PCWSTR(png_name.as_ptr())),
        )
    };
    if effect_format == 0 || png_format == 0 {
        return Err(ShellError::Win32(windows::core::Error::from_win32()));
    }
    open_clipboard(owner)?;
    let _clip = Clip;
    // SAFETY: the clipboard is open.
    unsafe {
        EmptyClipboard()?;
        set_clipboard(u32::from(CF_HDROP.0), &drop)?;
        set_clipboard(effect_format, &effect)?;
        if let Some((dib, png)) = &pixels {
            set_clipboard(u32::from(CF_DIBV5.0), dib)?;
            if let Some(png) = png {
                set_clipboard(png_format, png)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dibv5_layout_is_bottom_up_bgra() {
        // 2 x 2 image, top row: red, green; bottom row: blue, white (B, G, R, A).
        let px: Vec<u8> = vec![
            0, 0, 255, 255, 0, 255, 0, 255, // top
            255, 0, 0, 255, 255, 255, 255, 255, // bottom
        ];
        let d = build_dibv5(2, 2, &px);
        assert_eq!(d.len(), BITMAPV5HEADER_SIZE + 16);
        let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        assert_eq!(u32_at(0), 124);
        assert_eq!(u32_at(4), 2);
        assert_eq!(u32_at(8), 2);
        assert_eq!(u16::from_le_bytes([d[12], d[13]]), 1);
        assert_eq!(u16::from_le_bytes([d[14], d[15]]), 32);
        assert_eq!(u32_at(16), BI_BITFIELDS);
        assert_eq!(u32_at(20), 16);
        assert_eq!(u32_at(40), 0x00FF_0000);
        assert_eq!(u32_at(44), 0x0000_FF00);
        assert_eq!(u32_at(48), 0x0000_00FF);
        assert_eq!(u32_at(52), 0xFF00_0000);
        assert_eq!(u32_at(56), LCS_SRGB);
        // The bottom row comes first.
        assert_eq!(&d[124..128], &[255, 0, 0, 255]);
        assert_eq!(&d[132..136], &[0, 0, 255, 255]);
    }

    #[test]
    fn wic_decodes_png_and_jpeg_fixtures() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        for name in ["sample.png", "sample.jpg"] {
            let p = dir.join(name);
            if !p.is_file() {
                eprintln!("SKIPPED: {name} not found");
                continue;
            }
            let (w, h, px) = load_bgra(&p).unwrap();
            assert!(w > 0 && h > 0, "{name}");
            assert_eq!(px.len(), w as usize * h as usize * 4);
            let dib = build_dibv5(w, h, &px);
            assert_eq!(dib.len(), BITMAPV5HEADER_SIZE + px.len());
        }
        assert!(load_bgra(Path::new("C:/no/such/file.png")).is_err());
    }

    #[test]
    fn empty_file_list_is_an_error() {
        assert!(copy_files_and_image(HWND::default(), &[], None).is_err());
    }
}
