//! Download + decode images off the main thread.

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};

use crate::feeds;

/// Download and decode an image, downscaling (keeping aspect) to fit `max_w` × `max_h`.
/// Blocking; call from a worker thread (GdkTextures are thread-safe).
pub fn decode(url: &str, max_w: i32, max_h: i32) -> Result<gdk::Texture, String> {
    let bytes = feeds::download(url)?;
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
    let mut pixbuf = gdk_pixbuf::Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).map_err(|e| e.to_string())?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    if w > max_w || h > max_h {
        let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
        let (nw, nh) = (((w as f64 * scale).round() as i32).max(1), ((h as f64 * scale).round() as i32).max(1));
        pixbuf = pixbuf.scale_simple(nw, nh, gdk_pixbuf::InterpType::Bilinear).ok_or("scale failed")?;
    }
    let format = if pixbuf.has_alpha() { gdk::MemoryFormat::R8g8b8a8 } else { gdk::MemoryFormat::R8g8b8 };
    let texture = gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    );
    Ok(texture.upcast())
}
