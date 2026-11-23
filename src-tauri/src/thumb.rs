#[cfg(windows)]
mod win {
    use std::path::Path;

    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        SHCreateItemFromParsingName, IShellItemImageFactory, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY,
    };
    use windows::core::PCWSTR;

    const EDGE: i32 = 128;

    pub fn data_url(file: &str) -> Option<String> {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let png = shell_png(Path::new(file))?;
        Some(format!("data:image/png;base64,{}", base64(&png)))
    }

    fn shell_png(file: &Path) -> Option<Vec<u8>> {
        let wide: Vec<u16> = file
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let factory: IShellItemImageFactory =
            unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()? };
        let bitmap = unsafe {
            factory
                .GetImage(
                    SIZE { cx: EDGE, cy: EDGE },
                    SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK,
                )
                .ok()?
        };
        let pixels = bitmap_rgba(bitmap);
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        let (width, height, rgba) = pixels?;
        encode_png(width, height, &rgba)
    }

    fn bitmap_rgba(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        let mut info = BITMAP::default();
        let written = unsafe {
            GetObjectW(
                HGDIOBJ(bitmap.0),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut info as *mut BITMAP as *mut _),
            )
        };
        if written == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
            return None;
        }
        let width = info.bmWidth as u32;
        let height = info.bmHeight as u32;
        if width > 1024 || height > 1024 {
            return None;
        }
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: info.bmWidth,
                biHeight: -info.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let dc = unsafe { GetDC(None) };
        let rows = unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                height,
                Some(bgra.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            )
        };
        unsafe { ReleaseDC(None, dc) };
        if rows == 0 {
            return None;
        }
        let mut rgba = Vec::with_capacity(bgra.len());
        let mut visible = false;
        for chunk in bgra.chunks_exact(4) {
            let alpha = if chunk[3] == 0 { 255 } else { chunk[3] };
            if chunk[0] > 8 || chunk[1] > 8 || chunk[2] > 8 {
                visible = true;
            }
            rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], alpha]);
        }
        if !visible {
            return None;
        }
        Some((width, height, rgba))
    }

    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        drop(writer);
        Some(out)
    }

    fn base64(data: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut index = 0;
        while index + 3 <= data.len() {
            let n = (u32::from(data[index]) << 16)
                | (u32::from(data[index + 1]) << 8)
                | u32::from(data[index + 2]);
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
            out.push(TABLE[(n & 63) as usize] as char);
            index += 3;
        }
        let rest = data.len() - index;
        if rest == 1 {
            let n = u32::from(data[index]) << 16;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        } else if rest == 2 {
            let n = (u32::from(data[index]) << 16) | (u32::from(data[index + 1]) << 8);
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        out
    }
}

#[cfg(windows)]
pub use win::data_url;

#[cfg(not(windows))]
pub fn data_url(_file: &str) -> Option<String> {
    None
}