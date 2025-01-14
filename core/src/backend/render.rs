pub use crate::{transform::Transform, Color};
use std::io::Read;
pub use swf;

pub trait RenderBackend {
    fn set_viewport_dimensions(&mut self, width: u32, height: u32);
    fn register_shape(&mut self, shape: &swf::Shape) -> ShapeHandle;
    fn register_glyph_shape(&mut self, shape: &swf::Glyph) -> ShapeHandle;
    fn register_bitmap_jpeg(
        &mut self,
        id: swf::CharacterId,
        data: &[u8],
        jpeg_tables: &[u8],
    ) -> BitmapHandle;
    fn register_bitmap_jpeg_2(&mut self, id: swf::CharacterId, data: &[u8]) -> BitmapHandle;
    fn register_bitmap_jpeg_3(
        &mut self,
        id: swf::CharacterId,
        jpeg_data: &[u8],
        alpha_data: &[u8],
    ) -> BitmapHandle;
    fn register_bitmap_png(&mut self, swf_tag: &swf::DefineBitsLossless) -> BitmapHandle;

    fn begin_frame(&mut self);
    fn clear(&mut self, color: Color);
    fn render_shape(&mut self, shape: ShapeHandle, transform: &Transform);
    fn end_frame(&mut self);
    fn draw_pause_overlay(&mut self);
    fn draw_letterbox(&mut self, letterbox: Letterbox);
}

#[derive(Copy, Clone, Debug)]
pub struct ShapeHandle(pub usize);

#[derive(Copy, Clone, Debug)]
pub struct BitmapHandle(pub usize);

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Letterbox {
    None,
    Letterbox(f32),
    Pillarbox(f32)
}


pub struct NullRenderer;

impl RenderBackend for NullRenderer {
    fn set_viewport_dimensions(&mut self, _width: u32, _height: u32) {}
    fn register_shape(&mut self, _shape: &swf::Shape) -> ShapeHandle {
        ShapeHandle(0)
    }
    fn register_glyph_shape(&mut self, _shape: &swf::Glyph) -> ShapeHandle {
        ShapeHandle(0)
    }
    fn register_bitmap_jpeg(
        &mut self,
        _id: swf::CharacterId,
        _data: &[u8],
        _jpeg_tables: &[u8],
    ) -> BitmapHandle {
        BitmapHandle(0)
    }
    fn register_bitmap_jpeg_2(&mut self, _id: swf::CharacterId, _data: &[u8]) -> BitmapHandle {
        BitmapHandle(0)
    }
    fn register_bitmap_jpeg_3(
        &mut self,
        _id: swf::CharacterId,
        _data: &[u8],
        _alpha_data: &[u8],
    ) -> BitmapHandle {
        BitmapHandle(0)
    }
    fn register_bitmap_png(&mut self, _swf_tag: &swf::DefineBitsLossless) -> BitmapHandle {
        BitmapHandle(0)
    }
    fn begin_frame(&mut self) {}
    fn end_frame(&mut self) {}
    fn clear(&mut self, _color: Color) {}
    fn render_shape(&mut self, _shape: ShapeHandle, _transform: &Transform) {}
    fn draw_pause_overlay(&mut self) {}
    fn draw_letterbox(&mut self, _letterbox: Letterbox) {}
}

pub fn glue_swf_jpeg_to_tables(jpeg_tables: &[u8], jpeg_data: &[u8]) -> Vec<u8> {
    let mut full_jpeg = Vec::with_capacity(jpeg_tables.len() + jpeg_data.len() - 4);
    full_jpeg.extend_from_slice(&jpeg_tables[..jpeg_tables.len() - 2]);
    full_jpeg.extend_from_slice(&jpeg_data[2..]);
    full_jpeg
}

/// Removes potential invalid JPEG data from SWF DefineBitsJPEG tags.
///
/// SWF19 p.138:
/// "Before version 8 of the SWF file format, SWF files could contain an erroneous header of 0xFF, 0xD9, 0xFF, 0xD8 before the JPEG SOI marker."
/// These bytes need to be removed for the JPEG to decode properly.
pub fn remove_invalid_jpeg_data(mut data: &[u8]) -> std::borrow::Cow<[u8]> {
    // TODO: Might be better to return an Box<Iterator<Item=u8>> instead of a Cow here,
    // where the spliced iter is a data[..n].chain(data[n+4..])?
    if data[..4] == [0xFF, 0xD9, 0xFF, 0xD8] {
        data = &data[4..];
    }
    if let Some(pos) = (0..data.len() - 4).find(|&n| data[n..n + 4] == [0xFF, 0xD9, 0xFF, 0xD8]) {
        let mut out_data = Vec::with_capacity(data.len() - 4);
        out_data.extend_from_slice(&data[..pos]);
        out_data.extend_from_slice(&data[pos + 4..]);
        std::borrow::Cow::from(out_data)
    } else {
        std::borrow::Cow::Borrowed(data)
    }
}

/// Decodes a JPEG with optional alpha data.
///
pub fn define_bits_jpeg_to_rgba(
    jpeg_data: &[u8],
    alpha_data: &[u8],
) -> Result<(u32, u32, Vec<u8>), Box<dyn std::error::Error>> {
    let jpeg_data = remove_invalid_jpeg_data(jpeg_data);

    let mut decoder = jpeg_decoder::Decoder::new(&jpeg_data[..]);
    decoder.read_info().unwrap();
    let metadata = decoder.info().unwrap();
    let decoded_data = decoder.decode().expect("failed to decode image");

    // Decompress the alpha data (DEFLATE compression).
    let alpha_data = {
        let mut data = vec![];
        let mut decoder = libflate::zlib::Decoder::new(alpha_data)?;
        decoder.read_to_end(&mut data)?;
        data
    };

    let mut rgba = Vec::with_capacity((decoded_data.len() / 3) * 4);
    let mut i = 0;
    let mut a = 0;
    while i < decoded_data.len() {
        rgba.push(decoded_data[i]);
        rgba.push(decoded_data[i + 1]);
        rgba.push(decoded_data[i + 2]);
        rgba.push(alpha_data[a]);
        i += 3;
        a += 1;
    }

    Ok((metadata.width.into(), metadata.height.into(), rgba))
}

/// Decodes the bitmap data in DefineBitsLossless tag into RGBA.
/// DefineBitsLossless is Zlib encoded pixel data (similar to PNG), possibly
/// palletized.
pub fn define_bits_lossless_to_rgba(
    swf_tag: &swf::DefineBitsLossless,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Decompress the image data (DEFLATE compression).
    let mut decoded_data = {
        let mut data = vec![];
        let mut decoder = libflate::zlib::Decoder::new(&swf_tag.data[..])?;
        decoder.read_to_end(&mut data)?;
        data
    };

    // Swizzle/de-palettize the bitmap.
    let out_data = match (swf_tag.version, swf_tag.format) {
        (1, swf::BitmapFormat::Rgb15) => unimplemented!("15-bit PNG"),
        (1, swf::BitmapFormat::Rgb32) => {
            let mut i = 0;
            while i < decoded_data.len() {
                decoded_data[i] = decoded_data[i + 1];
                decoded_data[i + 1] = decoded_data[i + 2];
                decoded_data[i + 2] = decoded_data[i + 3];
                decoded_data[i + 3] = 0xff;
                i += 4;
            }
            decoded_data
        }
        (2, swf::BitmapFormat::Rgb32) => {
            let mut i = 0;
            while i < decoded_data.len() {
                let alpha = decoded_data[i];
                decoded_data[i] = decoded_data[i + 1];
                decoded_data[i + 1] = decoded_data[i + 2];
                decoded_data[i + 2] = decoded_data[i + 3];
                decoded_data[i + 3] = alpha;
                i += 4;
            }
            decoded_data
        }
        (1, swf::BitmapFormat::ColorMap8) => {
            let mut i = 0;
            let padded_width = (swf_tag.width + 0b11) & !0b11;

            let mut palette = Vec::with_capacity(swf_tag.num_colors as usize + 1);
            for _ in 0..=swf_tag.num_colors {
                palette.push(Color {
                    r: decoded_data[i],
                    g: decoded_data[i + 1],
                    b: decoded_data[i + 2],
                    a: 255,
                });
                i += 3;
            }
            let mut out_data = vec![];
            for _ in 0..swf_tag.height {
                for _ in 0..swf_tag.width {
                    let entry = decoded_data[i] as usize;
                    if entry < palette.len() {
                        let color = &palette[entry];
                        out_data.push(color.r);
                        out_data.push(color.g);
                        out_data.push(color.b);
                        out_data.push(color.a);
                    } else {
                        out_data.push(0);
                        out_data.push(0);
                        out_data.push(0);
                        out_data.push(255);
                    }
                    i += 1;
                }
                i += (padded_width - swf_tag.width) as usize;
            }
            out_data
        }
        (2, swf::BitmapFormat::ColorMap8) => {
            let mut i = 0;
            let padded_width = (swf_tag.width + 0b11) & !0b11;

            let mut palette = Vec::with_capacity(swf_tag.num_colors as usize + 1);
            for _ in 0..=swf_tag.num_colors {
                palette.push(Color {
                    r: decoded_data[i],
                    g: decoded_data[i + 1],
                    b: decoded_data[i + 2],
                    a: decoded_data[i + 3],
                });
                i += 4;
            }
            let mut out_data = vec![];
            for _ in 0..swf_tag.height {
                for _ in 0..swf_tag.width {
                    let entry = decoded_data[i] as usize;
                    if entry < palette.len() {
                        let color = &palette[entry];
                        out_data.push(color.r);
                        out_data.push(color.g);
                        out_data.push(color.b);
                        out_data.push(color.a);
                    } else {
                        out_data.push(0);
                        out_data.push(0);
                        out_data.push(0);
                        out_data.push(0);
                    }
                    i += 1;
                }
                i += (padded_width - swf_tag.width) as usize;
            }
            out_data
        }
        _ => unimplemented!("{:?} {:?}", swf_tag.version, swf_tag.format),
    };

    Ok(out_data)
}
