//! tinygif - A tiny GIF decoder for embedded systems.

use core::fmt::{self, Debug};
use core::marker::PhantomData;

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::vec;
use embedded_graphics::prelude::{DrawTarget, ImageDrawable, OriginDimensions, Point, RgbColor, Size};
use embedded_graphics::Pixel;
use embedded_graphics::{pixelcolor::Rgb888, prelude::PixelColor};
use parser::eat_len_prefixed_subblocks;

use parser::{le_u16, take, take1, take_slice};

mod bitstream;
pub mod lzw;
mod parser;

/// Len byte prefixed raw bytes, as used in GIFs.
struct LenPrefixRawDataView<'a> {
    remains: &'a [u8],
    current_block: &'a [u8],
    cursor: u8,
}

impl<'a> LenPrefixRawDataView<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let len = data[0] as usize;
        Self {
            remains: &data[1 + len..],
            current_block: &data[1..1 + len],
            cursor: 0,
        }
    }

    #[inline]
    fn shift_cursor(&mut self) {
        if self.current_block.is_empty() {
            // nop
        } else if self.cursor < self.current_block.len() as u8 - 1 {
            self.cursor += 1;
        } else {
            self.cursor = 0;
            self.shift_next_block();
        }
    }

    // leave cursor untouched
    #[inline]
    fn shift_next_block(&mut self) {
        if self.current_block.is_empty() {
            // no more blocks
            return;
        }
        let len = self.remains[0] as usize;
        if len == 0 {
            self.remains = &[];
            self.current_block = &[];
        } else {
            self.current_block = &self.remains[1..1 + len];
            self.remains = &self.remains[1 + len..];
        }
    }
}

impl Iterator for LenPrefixRawDataView<'_> {
    type Item = u8;

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        if self.current_block.is_empty() {
            return None;
        }

        let current = self.current_block[self.cursor as usize];
        self.shift_cursor();
        Some(current)
    }
}

#[non_exhaustive]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum Version {
    V87a,
    V89a,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Header {
    version: Version,
    pub width: u16,
    pub height: u16,
    has_global_color_table: bool,
    color_resolution: u8, // 3 bits
    // _is_sorted: bool,
    pub bg_color_index: u8,
    // _pixel_aspect_ratio: u8
}

impl Header {
    pub fn parse(input: &[u8]) -> Result<(&[u8], (Header, Option<ColorTable<'_>>)), ParseError> {
        let (input, magic) = take::<3>(input)?;

        if &magic != b"GIF" {
            return Err(ParseError::InvalidFileSignature(magic));
        }

        let (input, ver) = take::<3>(input)?;
        let version = if &ver == b"87a" {
            Version::V87a
        } else if &ver == b"89a" {
            Version::V89a
        } else {
            return Err(ParseError::InvalidFileSignature(magic));
        };

        let (intput, screen_width) = le_u16(input)?;
        let (intput, screen_height) = le_u16(intput)?;

        let (input, flags) = take1(intput)?;
        let has_global_color_table = flags & 0b1000_0000 != 0;
        let global_color_table_size = if has_global_color_table {
            2_usize.pow(((flags & 0b0000_0111) + 1) as u32)
        } else {
            0
        };
        let color_resolution = (flags & 0b0111_0000) >> 4;
        let _is_sorted = flags & 0b0000_1000 != 0;

        let (input, bg_color_index) = take1(input)?;
        let (input, _pixel_aspect_ratio) = take1(input)?;

        let (input, color_table) = if global_color_table_size > 0 {
            // Each color table entry is 3 bytes long
            let (input, table) = take_slice(input, global_color_table_size * 3)?;
            (input, Some(ColorTable::new(table)))
        } else {
            (input, None)
        };

        Ok((
            input,
            (
                Header {
                    version,
                    width: screen_width,
                    height: screen_height,
                    has_global_color_table,
                    color_resolution,
                    bg_color_index,
                },
                color_table,
            ),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColorTable<'a> {
    data: &'a [u8],
}

impl<'a> ColorTable<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Returns the number of entries.
    pub const fn len(&self) -> usize {
        self.data.len() / 3
    }

    /// Returns a color table entry.
    ///
    /// `None` is returned if `index` is out of bounds.
    pub fn get(&self, index: u8) -> Option<Rgb888> {
        let base = 3 * (index as usize);
        if base >= self.data.len() {
            return None;
        }

        Some(Rgb888::new(
            self.data[base],
            self.data[base + 1],
            self.data[base + 2],
        ))
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct RawGif<'a> {
    /// Image header.
    header: Header,

    global_color_table: Option<ColorTable<'a>>,

    /// Image data.
    raw_block_data: &'a [u8],
}

impl<'a> RawGif<'a> {
    fn from_slice(bytes: &'a [u8]) -> Result<Self, ParseError> {
        let (remaining, (header, color_table)) = Header::parse(bytes)?;

        Ok(Self {
            header,
            global_color_table: color_table,
            raw_block_data: remaining,
        })
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct GraphicControl {
    pub is_transparent: bool,
    pub transparent_color_index: u8,
    // centisecond
    pub delay_centis: u16,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]

pub struct ImageBlock<'a> {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub is_interlaced: bool,
    pub lzw_min_code_size: u8,
    local_color_table: Option<ColorTable<'a>>,
    image_data: &'a [u8],
}

impl<'a> ImageBlock<'a> {
    // parse after 0x2c separator
    pub fn parse(input: &'a [u8]) -> Result<(&[u8], Self), ParseError> {
        let (input, left) = le_u16(input)?;
        let (input, top) = le_u16(input)?;
        let (input, width) = le_u16(input)?;
        let (input, height) = le_u16(input)?;
        let (input, flags) = take1(input)?;
        let is_interlaced = flags & 0b0100_0000 != 0;
        let has_local_color_table = flags & 0b1000_0000 != 0;
        let local_color_table_size = if has_local_color_table {
            2_usize.pow(((flags & 0b0000_0111) + 1) as u32)
        } else {
            0
        };

        let (input, local_color_table) = if local_color_table_size > 0 {
            // Each color table entry is 4 bytes long
            let (input, table) = take_slice(input, local_color_table_size * 3)?;
            (input, Some(ColorTable::new(table)))
        } else {
            (input, None)
        };

        let (input, lzw_min_code_size) = take1(input)?;

        let mut input0 = input;
        let mut n = 1;
        loop {
            let (input, block_size) = take1(input0)?;
            if block_size == 0 {
                input0 = input;
                break;
            }
            let (input, _) = take_slice(input, block_size as usize)?;
            n += block_size as usize + 1;
            input0 = input;
        }

        Ok((
            input0,
            Self {
                left,
                top,
                width,
                height,
                is_interlaced,
                lzw_min_code_size,
                local_color_table,
                image_data: &input[..n],
            },
        ))
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum ExtensionBlock<'a> {
    GraphicControl(GraphicControl),
    Comment(&'a [u8]),
    // TODO: handle PlainTextExtension
    PlainText,
    NetscapeApplication { repetitions: u16 },
    Application, // ignore content
    Unknown(u8, &'a [u8]),
}

impl<'a> ExtensionBlock<'a> {
    pub fn parse(input: &'a [u8]) -> Result<(&'a [u8], Self), ParseError> {
        let (input, ext_label) = take1(input)?;
        match ext_label {
            0xff => {
                let (input, block_size_1) = take1(input)?;
                let (input, app_id) = take::<8>(input)?;
                let (input, app_auth_code) = take::<3>(input)?;
                if block_size_1 == 11 && &app_id == b"NETSCAPE" && &app_auth_code == b"2.0" {
                    let (input, block_size_2) = take1(input)?;
                    if block_size_2 == 3 {
                        let (input, always_one) = take1(input)?;
                        if always_one == 1 {
                            let (input, repetitions) = le_u16(input)?;
                            let (input, eob) = take1(input)?;
                            if eob == 0 {
                                return Ok((
                                    input,
                                    ExtensionBlock::NetscapeApplication { repetitions },
                                ));
                            }
                        }
                    }
                }
                let mut input0 = input;
                loop {
                    let (input, block_size_2) = take1(input0)?;
                    if block_size_2 == 0 {
                        input0 = input;
                        break;
                    }
                    let (input, _data) = take_slice(input, block_size_2 as usize)?;
                    input0 = input;
                }
                Ok((input0, ExtensionBlock::Application))
            }
            0xfe => {
                // Comment Extension
                let mut input0 = input;
                let mut size = 0;
                loop {
                    let (input, block_size) = take1(input0)?;
                    size += 1;
                    if block_size == 0 {
                        input0 = input;
                        break;
                    }
                    let (input, _data) = take_slice(input, block_size as usize)?;
                    input0 = input;
                    size += block_size as usize;
                }
                Ok((input0, ExtensionBlock::Comment(&input[..size])))
            }
            0xf9 => {
                // Graphic Control Extension
                let (input, block_size) = take1(input)?; // 4
                if block_size != 4 {
                    return Err(ParseError::InvalidConstSizeBytes); // invalid block size
                }
                let (input, flags) = take1(input)?;
                let is_transparent = flags & 0b0000_0001 != 0;
                let (input, delay_centis) = le_u16(input)?;
                let (input, transparent_color_index) = take1(input)?;
                let (input, block_terminator) = take1(input)?;
                if block_terminator != 0 {
                    return Err(ParseError::InvalidByte);
                }

                Ok((
                    input,
                    ExtensionBlock::GraphicControl(GraphicControl {
                        is_transparent,
                        transparent_color_index,
                        delay_centis,
                    }),
                ))
            }
            0x01 => {
                let (input, _) = take::<13>(input)?;
                let input = eat_len_prefixed_subblocks(input)?;
                Ok((input, ExtensionBlock::PlainText))
            }
            _ => unimplemented!(),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub enum Segment<'a> {
    Image(ImageBlock<'a>),
    Extension(ExtensionBlock<'a>),
    // 0x3b
    Trailer,
}

impl<'a> Segment<'a> {
    pub fn parse(input: &'a [u8]) -> Result<(&'a [u8], Box<Self>), ParseError> {
        let (input, ext_magic) = take1(input)?;

        if ext_magic == 0x21 {
            let (input, ext) = ExtensionBlock::parse(input)?;
            Ok((input, Box::new(Segment::Extension(ext))))
        } else if ext_magic == 0x2c {
            // Image Block
            let (input, image_block) = ImageBlock::parse(input)?;
            Ok((input, Box::new(Segment::Image(image_block))))
        } else if ext_magic == 0x3b {
            if input.is_empty() {
                Ok((input, Box::new(Segment::Trailer)))
            } else {
                Err(ParseError::JunkAfterTrailerByte)
            }
        } else {
            return Err(ParseError::InvalidByte);
        }
    }

    fn skip_to_next_graphic_control(mut input: &[u8]) -> Result<&[u8], ParseError> {
        loop {
            let (input0, ext_magic) = take1(input)?;
            if ext_magic == 0x21 {
                let (input1, label) = take1(input0)?;
                if label == 0xF9 {
                    return Ok(input);
                } else if label == 0xFF {
                    // Application Extension
                    input = eat_len_prefixed_subblocks(input1)?;
                } else if label == 0x01 {
                    // Plain Text Extension
                    // Ref: https://www.w3.org/Graphics/GIF/spec-gif89a.txt
                    let (input2, _) = take::<13>(input1)?;
                    input = eat_len_prefixed_subblocks(input2)?;
                } else if label == 0xFE {
                    // Comment Extension
                    input = eat_len_prefixed_subblocks(input1)?;
                } else {
                    return Err(ParseError::InvalidExtensionLabel);
                }
            } else if ext_magic == 0x2c {
                // Image Descriptor
                // parse image, optional local color table
                let (input1, _) = ImageBlock::parse(input0)?;
                input = input1;
            } else if ext_magic == 0x3b {
                // Trailer
                return Ok(&[]);
            } else if ext_magic == 0x00 {
                // Block Terminator
                return Ok(input0);
            } else {
                return Err(ParseError::InvalidByte);
            }
        }
    }

    pub const fn type_name(&self) -> &'static str {
        match self {
            Segment::Image(_) => "Image",
            Segment::Extension(_) => "Extension",
            Segment::Trailer => "Trailer",
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct Gif<'a, C = Rgb888> {
    raw_gif: RawGif<'a>,
    color_type: PhantomData<C>,
}

impl<'a, C> Gif<'a, C> {
    pub fn from_slice(input: &'a [u8]) -> Result<Self, ParseError> {
        let raw_gif = RawGif::from_slice(input)?;
        Ok(Self {
            raw_gif,
            color_type: PhantomData,
        })
    }

    pub fn frames(&'a self) -> FrameIterator<'a, C> {
        FrameIterator::new(self)
    }

    pub fn width(&self) -> u16 {
        self.raw_gif.header.width
    }

    pub fn height(&self) -> u16 {
        self.raw_gif.header.height
    }
}

pub struct FrameIterator<'a, C> {
    gif: &'a Gif<'a, C>,
    frame_index: usize,
    remain_raw_data: &'a [u8],
}

impl<'a, C> FrameIterator<'a, C> {
    fn new(gif: &'a Gif<'a, C>) -> Self {
        Self {
            gif,
            frame_index: 0,
            remain_raw_data: gif.raw_gif.raw_block_data,
        }
    }
}

impl<'a, C: PixelColor> Iterator for FrameIterator<'a, C> {
    type Item = Frame<'a, C>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remain_raw_data.is_empty() {
            return None;
        }

        let input = self.remain_raw_data;
        let input0 = Segment::skip_to_next_graphic_control(input).ok()?;

        let (input00, seg) = Segment::parse(input0).ok()?;
        self.remain_raw_data = input00;

        if let Segment::Extension(ExtensionBlock::GraphicControl(ctrl)) = seg.as_ref() {
            let frame = Frame {
                delay_centis: ctrl.delay_centis,
                is_transparent: ctrl.is_transparent,
                transparent_color_index: ctrl.transparent_color_index,
                global_color_table: self.gif.raw_gif.global_color_table.clone(),
                header: &self.gif.raw_gif.header,
                raw_data: input00,
                frame_index: self.frame_index,
                _marker: PhantomData,
            };
            self.frame_index += 1;
            return Some(frame);
        } else {
            None
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Frame<'a, C> {
    pub delay_centis: u16,
    pub is_transparent: bool,
    pub transparent_color_index: u8,
    global_color_table: Option<ColorTable<'a>>,
    header: &'a Header,
    raw_data: &'a [u8],
    frame_index: usize,
    _marker: PhantomData<C>,
}

impl<'a, C> OriginDimensions for Frame<'a, C> {
    fn size(&self) -> Size {
        Size::new(self.header.width as _, self.header.height as _)
    }
}

impl<'a, C> Frame<'a, C> {
    pub fn decode_to_rgb(&self) -> Result<Vec<u8>, ParseError>{
        let mut rgb_image = vec![0u8; self.header.width as usize*self.header.height as usize*3];
        let mut input = self.raw_data;
        while let Ok((input0, seg)) = Segment::parse(input) {
            input = input0;
            match seg.as_ref() {
                Segment::Extension(ExtensionBlock::GraphicControl(_)) | Segment::Trailer => {
                    // overflows to the next frame
                    break;
                }
                Segment::Image(ImageBlock {
                    left,
                    top,
                    width,
                    lzw_min_code_size,
                    local_color_table,
                    image_data,
                    ..
                }) => {
                    let transparent_color_index = if self.is_transparent {
                        Some(self.transparent_color_index)
                    } else {
                        None
                    };
                    let color_table = local_color_table
                        .or_else(|| self.global_color_table.clone())
                        .unwrap();
                    let raw_image_data = Box::new(LenPrefixRawDataView::new(image_data));
                    let mut decoder = Box::new(lzw::Decoder::new(raw_image_data, *lzw_min_code_size));

                    let mut idx: u32 = 0;

                    while let Ok(Some(decoded)) = decoder.decode_next() {
                        for (x, y, pixel) in decoded.iter().filter_map(|&color_index| {
                            if transparent_color_index == Some(color_index) {
                                // skip drawing transparent color
                                idx += 1;
                                return None;
                            }
                            let x = left + (idx % u32::from(*width)) as u16;
                            let y = top + (idx / u32::from(*width)) as u16;
                            idx += 1;

                            let color = color_table.get(color_index).unwrap();
                            Some((x, y, color))
                        }){
                            if let Some(rgb_pixel) = rgb_image.chunks_mut(3).nth(y as usize*self.header.width as usize + x as usize){
                                rgb_pixel.copy_from_slice(&[pixel.r(), pixel.g(), pixel.b()]);
                            }
                        }
                    }
                }
                _ => (),
            }
        }

        Ok(rgb_image)
    }
}

impl<'a, C> ImageDrawable for Frame<'a, C>
where
    C: PixelColor + From<Rgb888>,
{
    type Color = C;

    fn draw<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        let mut input = self.raw_data;
        while let Ok((input0, seg)) = Segment::parse(input) {
            input = input0;
            match seg.as_ref() {
                Segment::Extension(ExtensionBlock::GraphicControl(_)) | Segment::Trailer => {
                    // overflows to the next frame
                    break;
                }
                Segment::Image(ImageBlock {
                    left,
                    top,
                    width,
                    lzw_min_code_size,
                    local_color_table,
                    image_data,
                    ..
                }) => {
                    let transparent_color_index = if self.is_transparent {
                        Some(self.transparent_color_index)
                    } else {
                        None
                    };
                    let color_table = local_color_table
                        .or_else(|| self.global_color_table.clone())
                        .unwrap();
                    let raw_image_data = LenPrefixRawDataView::new(image_data);
                    let mut decoder = lzw::Decoder::new(raw_image_data, *lzw_min_code_size);

                    let mut idx: u32 = 0;

                    while let Ok(Some(decoded)) = decoder.decode_next() {
                        target.draw_iter(decoded.iter().filter_map(|&color_index| {
                            if transparent_color_index == Some(color_index) {
                                // skip drawing transparent color
                                idx += 1;
                                return None;
                            }
                            let x = left + (idx % u32::from(*width)) as u16;
                            let y = top + (idx / u32::from(*width)) as u16;
                            idx += 1;

                            let color = color_table.get(color_index).unwrap();
                            Some(Pixel(Point::new(x as i32, y as i32), color.into()))
                        }))?;
                    }
                }
                _ => (),
            }
        }

        Ok(())
    }

    fn draw_sub_image<D>(
        &self,
        target: &mut D,
        area: &embedded_graphics::primitives::Rectangle,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        let mut input = self.raw_data;
        while let Ok((input0, seg)) = Segment::parse(input) {
            input = input0;
            match seg.as_ref() {
                Segment::Extension(ExtensionBlock::GraphicControl(_)) | Segment::Trailer => {
                    // overflows to the next frame
                    break;
                }
                Segment::Image(ImageBlock {
                    left,
                    top,
                    width,
                    lzw_min_code_size,
                    local_color_table,
                    image_data,
                    ..
                }) => {
                    let transparent_color_index = if self.is_transparent {
                        Some(self.transparent_color_index)
                    } else {
                        None
                    };
                    let color_table = local_color_table
                        .or_else(|| self.global_color_table.clone())
                        .unwrap();
                    let raw_image_data = LenPrefixRawDataView::new(image_data);
                    let mut decoder = lzw::Decoder::new(raw_image_data, *lzw_min_code_size);

                    let mut idx: u32 = 0;

                    while let Ok(Some(decoded)) = decoder.decode_next() {
                        target.draw_iter(decoded.iter().filter_map(|&color_index| {
                            if transparent_color_index == Some(color_index) {
                                idx += 1;
                                return None;
                            }

                            let x = left + (idx % u32::from(*width)) as u16;
                            let y = top + (idx / u32::from(*width)) as u16;
                            idx += 1;

                            let pt = Point::new(x as i32, y as i32);
                            if area.contains(pt) {
                                let color = color_table.get(color_index).unwrap();
                                Some(Pixel(pt, color.into()))
                            } else {
                                None
                            }
                        }))?;
                    }
                }
                _ => (),
            }
        }

        Ok(())
    }
}

impl<C> fmt::Debug for Frame<'_, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("frame_index", &self.frame_index)
            .field("delay_centis", &self.delay_centis)
            .field("is_transparent", &self.is_transparent)
            .field("transparent_color_index", &self.transparent_color_index)
            .field("len(remain_data)", &self.raw_data.len())
            .finish()
    }
}

/// Parse error.
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum ParseError {
    /// The image uses an unsupported bit depth.
    UnsupportedBpp(u16),

    /// Unexpected end of file.
    UnexpectedEndOfFile,

    /// Invalid file signatures.
    InvalidFileSignature([u8; 3]),

    /// Unsupported compression method.
    UnsupportedCompressionMethod(u32),

    /// Unsupported header length.
    UnsupportedHeaderLength(u32),

    /// Unsupported channel masks.
    UnsupportedChannelMasks,

    /// Invalid image dimensions.
    InvalidImageDimensions,

    InvalidByte,

    JunkAfterTrailerByte,

    /// Current size bytes should be a constant.
    InvalidConstSizeBytes,

    InvalidExtensionLabel,
}
