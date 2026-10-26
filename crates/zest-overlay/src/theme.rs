//! Colors: light/dark from `AppsUseLightTheme`, accent from
//! `DwmGetColorizationColor`. The color maths is pure and unit-tested.

use std::ffi::c_void;

use windows::core::w;
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

impl Rgba {
    pub fn with_alpha(self, a: f32) -> Rgba {
        Rgba { a, ..self }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub light: bool,
    pub disc: Rgba,
    pub sub_disc: Rgba,
    pub dead: Rgba,
    pub text: Rgba,
    pub text_on_accent: Rgba,
    pub line: Rgba,
    pub accent: Rgba,
    pub track: Rgba,
    pub lossy: Rgba,
    pub ok: Rgba,
    pub fail: Rgba,
    pub muted: Rgba,
    /// Soft drop under sectors / progress.
    pub shadow: Rgba,
    /// Specular rim on glass slices.
    pub glass_hi: Rgba,
    /// Inner emboss edge.
    pub glass_lo: Rgba,
    /// Slice body (translucent glass over the disc).
    pub slice: Rgba,
    /// Hovered slice glass wash (multiplied with accent).
    pub slice_hot: Rgba,
}

/// Default accent when DWM gives none (Windows blue).
pub const DEFAULT_ACCENT: Rgba = rgba(0.0, 0.47, 0.84, 0.95);

impl Theme {
    /// Build a theme. `accent_argb` is the DWM colorization value (0xAARRGGBB).
    pub fn new(light: bool, accent_argb: Option<u32>) -> Theme {
        let accent = match accent_argb {
            Some(argb) => rgba(
                ((argb >> 16) & 0xFF) as f32 / 255.0,
                ((argb >> 8) & 0xFF) as f32 / 255.0,
                (argb & 0xFF) as f32 / 255.0,
                0.95,
            ),
            None => DEFAULT_ACCENT,
        };
        if light {
            Theme {
                light,
                disc: rgba(0.94, 0.95, 0.97, 0.78),
                sub_disc: rgba(0.92, 0.93, 0.95, 0.82),
                dead: rgba(1.0, 1.0, 1.0, 0.38),
                text: rgba(0.10, 0.11, 0.14, 0.96),
                text_on_accent: rgba(1.0, 1.0, 1.0, 0.98),
                line: rgba(0.0, 0.0, 0.0, 0.06),
                accent,
                track: rgba(0.0, 0.0, 0.0, 0.14),
                lossy: rgba(0.85, 0.50, 0.0, 0.95),
                ok: rgba(0.10, 0.62, 0.30, 0.95),
                fail: rgba(0.84, 0.18, 0.18, 0.95),
                muted: rgba(0.40, 0.40, 0.44, 0.95),
                shadow: rgba(0.05, 0.08, 0.14, 0.28),
                glass_hi: rgba(1.0, 1.0, 1.0, 0.55),
                glass_lo: rgba(0.20, 0.25, 0.35, 0.18),
                slice: rgba(1.0, 1.0, 1.0, 0.22),
                slice_hot: rgba(1.0, 1.0, 1.0, 0.16),
            }
        } else {
            Theme {
                light,
                // Frosted dark glass — denser than a flat panel, still see-through.
                disc: rgba(0.10, 0.11, 0.14, 0.72),
                sub_disc: rgba(0.14, 0.15, 0.19, 0.78),
                dead: rgba(0.06, 0.07, 0.09, 0.35),
                text: rgba(0.96, 0.97, 0.99, 0.96),
                text_on_accent: rgba(1.0, 1.0, 1.0, 0.98),
                line: rgba(1.0, 1.0, 1.0, 0.05),
                accent,
                track: rgba(1.0, 1.0, 1.0, 0.16),
                lossy: rgba(1.0, 0.72, 0.20, 0.95),
                ok: rgba(0.30, 0.80, 0.45, 0.95),
                fail: rgba(0.95, 0.35, 0.35, 0.95),
                muted: rgba(0.65, 0.66, 0.70, 0.95),
                shadow: rgba(0.0, 0.0, 0.0, 0.45),
                glass_hi: rgba(1.0, 1.0, 1.0, 0.28),
                glass_lo: rgba(0.0, 0.0, 0.0, 0.35),
                slice: rgba(1.0, 1.0, 1.0, 0.07),
                slice_hot: rgba(1.0, 1.0, 1.0, 0.10),
            }
        }
    }

    /// Read the system settings now (one registry read and one DWM call).
    pub fn from_system() -> Theme {
        Theme::new(apps_use_light_theme(), accent_color())
    }
}

fn apps_use_light_theme() -> bool {
    let mut data = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: out buffers are valid locals of the stated size.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    // Missing value: Windows default is dark for this wheel (as in the spike).
    r.is_ok() && data != 0
}

fn accent_color() -> Option<u32> {
    let (mut argb, mut opaque) = (0u32, Default::default());
    // SAFETY: out-pointers to locals.
    unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) }.ok().map(|_| argb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_is_decoded_from_argb() {
        let t = Theme::new(false, Some(0xFF00_80FF));
        assert!((t.accent.r - 0.0).abs() < 1e-6);
        assert!((t.accent.g - 128.0 / 255.0).abs() < 1e-6);
        assert!((t.accent.b - 1.0).abs() < 1e-6);
        assert_eq!(t.accent.a, 0.95);
        assert_eq!(Theme::new(true, None).accent, DEFAULT_ACCENT);
    }

    #[test]
    fn light_and_dark_differ_in_text_contrast() {
        let (l, d) = (Theme::new(true, None), Theme::new(false, None));
        assert!(l.light && !d.light);
        assert!(l.text.r < 0.5 && d.text.r > 0.5);
        assert!(l.disc.r > 0.5 && d.disc.r < 0.5);
    }

    #[test]
    fn dark_theme_disc_stays_dark() {
        let d = Theme::new(false, None);
        assert!(d.disc.r < 0.2 && d.disc.g < 0.2);
        assert!(d.disc.a < 0.9 && d.disc.a > 0.55);
        assert!(d.dead.a < 0.5);
        assert!(d.slice.a < 0.2);
        assert!(d.glass_hi.a > 0.15);
    }
}
