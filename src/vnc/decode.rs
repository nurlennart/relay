//! The remote screen and the encodings that update it. Relay asks for 32-bit little-endian
//! true colour, so every pixel arrives as blue, green, red and a padding byte, and ZRLE's
//! compact pixels as blue, green and red.
use flate2::{Decompress, FlushDecompress, Status};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const RAW: i32 = 0;
pub const COPY_RECT: i32 = 1;
pub const HEXTILE: i32 = 5;
pub const ZRLE: i32 = 16;
/// Pseudo-encodings: the server's pointer shape, and a new screen size.
pub const CURSOR: i32 = -239;
pub const DESKTOP_SIZE: i32 = -223;
pub const EXTENDED_DESKTOP_SIZE: i32 = -308;

/// The encodings Relay accepts, most preferred first.
pub const ENCODINGS: [i32; 7] = [
    ZRLE,
    HEXTILE,
    COPY_RECT,
    RAW,
    CURSOR,
    EXTENDED_DESKTOP_SIZE,
    DESKTOP_SIZE,
];

/// Screens beyond this many pixels a side are refused rather than allocated.
pub const MAX_SIDE: u16 = 16384;
/// The largest pointer shape accepted, in pixels a side.
const MAX_POINTER: u16 = 256;
/// The most compressed data one ZRLE rectangle may carry.
const MAX_ZRLE: usize = 64 << 20;

/// A rectangle of the screen, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// The pointer shape the server asked for, as RGBA pixels with its hot spot.
#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub size: [usize; 2],
    pub hotspot: [u16; 2],
    pub rgba: Vec<u8>,
}

/// What a rectangle did to the screen.
#[derive(Debug, PartialEq)]
pub enum Change {
    /// These pixels changed.
    Pixels(Rect),
    /// The screen has a new size, all black until the server sends its contents.
    Resized,
    Cursor(Cursor),
    Nothing,
}

/// The remote screen as RGBA bytes.
pub struct Screen {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// ZRLE's compression continues across rectangles for the whole connection.
    zlib: Decompress,
}

impl Screen {
    pub fn new(width: u16, height: u16) -> Self {
        let mut screen = Self {
            width: 0,
            height: 0,
            rgba: vec![],
            zlib: Decompress::new(true),
        };
        screen.resize(width, height);
        screen
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = width.into();
        self.height = height.into();
        self.rgba = black(self.width * self.height);
    }

    /// Reads one rectangle's data in `encoding` from `reader` and applies it.
    pub async fn apply<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
        rect: Rect,
        encoding: i32,
    ) -> Result<Change, String> {
        let pixels = usize::from(rect.width) * usize::from(rect.height);
        // Sizes come from the server; check them before allocating anything for them.
        let inside = usize::from(rect.x) + usize::from(rect.width) <= self.width
            && usize::from(rect.y) + usize::from(rect.height) <= self.height;
        if matches!(encoding, RAW | COPY_RECT | HEXTILE | ZRLE) && !inside {
            return Err("the server sent an update outside the screen".into());
        }
        if encoding == CURSOR && (rect.width > MAX_POINTER || rect.height > MAX_POINTER) {
            return Err("the server sent an oversized pointer".into());
        }
        match encoding {
            RAW => {
                let mut data = vec![0u8; pixels * 4];
                reader.read_exact(&mut data).await.map_err(io)?;
                self.raw(rect, &data);
            }
            COPY_RECT => {
                let x = reader.read_u16().await.map_err(io)?;
                let y = reader.read_u16().await.map_err(io)?;
                self.copy(rect, [x, y]);
            }
            HEXTILE => self.hextile(reader, rect).await?,
            ZRLE => {
                let length = reader.read_u32().await.map_err(io)? as usize;
                if length > MAX_ZRLE {
                    return Err("the server sent an oversized update".into());
                }
                let mut data = vec![0u8; length];
                reader.read_exact(&mut data).await.map_err(io)?;
                let plain = self.inflate(&data, rect)?;
                self.zrle(rect, &plain)?;
            }
            DESKTOP_SIZE => {
                self.resize(valid_side(rect.width)?, valid_side(rect.height)?);
                return Ok(Change::Resized);
            }
            EXTENDED_DESKTOP_SIZE => {
                let screens = reader.read_u8().await.map_err(io)?;
                let mut skip = vec![0u8; 3 + usize::from(screens) * 16];
                reader.read_exact(&mut skip).await.map_err(io)?;
                // A refused request (a nonzero status in `y`) leaves the screen as it is.
                if rect.y != 0 || (rect.width, rect.height) == (0, 0) {
                    return Ok(Change::Nothing);
                }
                self.resize(valid_side(rect.width)?, valid_side(rect.height)?);
                return Ok(Change::Resized);
            }
            CURSOR => {
                let mut colors = vec![0u8; pixels * 4];
                reader.read_exact(&mut colors).await.map_err(io)?;
                let row = usize::from(rect.width).div_ceil(8);
                let mut mask = vec![0u8; row * usize::from(rect.height)];
                reader.read_exact(&mut mask).await.map_err(io)?;
                return Ok(Change::Cursor(cursor(rect, &colors, &mask)));
            }
            other => {
                return Err(format!(
                    "the server used encoding {other}, which Relay did not ask for"
                ));
            }
        }
        Ok(Change::Pixels(rect))
    }

    /// Sets the pixel at `x`, `y` to the blue, green, red bytes in `bgr`. Pixels outside the
    /// screen are dropped, so a server's stray rectangle cannot write past it.
    fn set(&mut self, x: usize, y: usize, bgr: &[u8]) {
        if x < self.width && y < self.height {
            let i = (y * self.width + x) * 4;
            self.rgba[i..i + 4].copy_from_slice(&[bgr[2], bgr[1], bgr[0], 255]);
        }
    }

    fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, bgr: &[u8]) {
        for row in y..(y + height).min(self.height) {
            for column in x..(x + width).min(self.width) {
                self.set(column, row, bgr);
            }
        }
    }

    /// Raw pixels, four bytes each, row by row.
    fn raw(&mut self, rect: Rect, data: &[u8]) {
        let width = usize::from(rect.width);
        for (i, pixel) in data.as_chunks::<4>().0.iter().enumerate() {
            self.set(
                usize::from(rect.x) + i % width,
                usize::from(rect.y) + i / width,
                pixel,
            );
        }
    }

    /// Copies the rectangle at `from` to `rect`; the two may overlap.
    fn copy(&mut self, rect: Rect, from: [u16; 2]) {
        let (width, height) = (usize::from(rect.width), usize::from(rect.height));
        let [sx, sy] = from.map(usize::from);
        let mut rows = Vec::with_capacity(width * height * 4);
        for y in sy..sy + height {
            for x in sx..sx + width {
                let pixel = if x < self.width && y < self.height {
                    let i = (y * self.width + x) * 4;
                    [self.rgba[i + 2], self.rgba[i + 1], self.rgba[i]]
                } else {
                    [0; 3]
                };
                rows.extend_from_slice(&pixel);
                rows.push(0);
            }
        }
        self.raw(rect, &rows);
    }

    /// Hextile: 16×16 tiles, each raw or a background with optional subrectangles.
    async fn hextile<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
        rect: Rect,
    ) -> Result<(), String> {
        const RAW: u8 = 1;
        const BACKGROUND: u8 = 2;
        const FOREGROUND: u8 = 4;
        const SUBRECTS: u8 = 8;
        const COLOURED: u8 = 16;
        let (mut background, mut foreground) = ([0u8; 4], [0u8; 4]);
        let right = usize::from(rect.x) + usize::from(rect.width);
        let bottom = usize::from(rect.y) + usize::from(rect.height);
        for top in (usize::from(rect.y)..bottom).step_by(16) {
            for left in (usize::from(rect.x)..right).step_by(16) {
                let (w, h) = ((right - left).min(16), (bottom - top).min(16));
                let flags = reader.read_u8().await.map_err(io)?;
                if flags & RAW != 0 {
                    let mut data = vec![0u8; w * h * 4];
                    reader.read_exact(&mut data).await.map_err(io)?;
                    for (i, pixel) in data.as_chunks::<4>().0.iter().enumerate() {
                        self.set(left + i % w, top + i / w, pixel);
                    }
                    continue;
                }
                if flags & BACKGROUND != 0 {
                    reader.read_exact(&mut background).await.map_err(io)?;
                }
                self.fill(left, top, w, h, &background);
                if flags & FOREGROUND != 0 {
                    reader.read_exact(&mut foreground).await.map_err(io)?;
                }
                if flags & SUBRECTS == 0 {
                    continue;
                }
                let count = reader.read_u8().await.map_err(io)?;
                for _ in 0..count {
                    let mut color = foreground;
                    if flags & COLOURED != 0 {
                        reader.read_exact(&mut color).await.map_err(io)?;
                    }
                    let place = reader.read_u8().await.map_err(io)?;
                    let size = reader.read_u8().await.map_err(io)?;
                    let (x, y) = (usize::from(place >> 4), usize::from(place & 15));
                    let (sw, sh) = (usize::from(size >> 4) + 1, usize::from(size & 15) + 1);
                    self.fill(
                        left + x,
                        top + y,
                        sw.min(w.saturating_sub(x)),
                        sh.min(h.saturating_sub(y)),
                        &color,
                    );
                }
            }
        }
        Ok(())
    }

    /// Decompresses one ZRLE rectangle's data, which can hold at most every tile in raw form.
    fn inflate(&mut self, data: &[u8], rect: Rect) -> Result<Vec<u8>, String> {
        let tiles = usize::from(rect.width).div_ceil(64) * usize::from(rect.height).div_ceil(64);
        let pixels = usize::from(rect.width) * usize::from(rect.height);
        // Raw tiles take three bytes a pixel; run lengths can take a little more.
        let limit = tiles * (1 + 16 * 3) + pixels * 4 + 1024;
        let mut out = Vec::with_capacity((pixels * 3 + tiles).min(limit));
        let mut consumed = 0;
        loop {
            if out.len() == out.capacity() {
                if out.len() >= limit {
                    return Err("the server sent an update larger than the screen".into());
                }
                out.reserve((out.len().max(4096)).min(limit - out.len()));
            }
            let (read, written) = (self.zlib.total_in(), self.zlib.total_out());
            let status = self
                .zlib
                .decompress_vec(&data[consumed..], &mut out, FlushDecompress::Sync)
                .map_err(|e| format!("the server sent damaged compressed data: {e}"))?;
            let used = (self.zlib.total_in() - read) as usize;
            consumed += used;
            // With all input taken and room to spare, nothing is held back.
            if status == Status::StreamEnd || (consumed == data.len() && out.len() < out.capacity())
            {
                return Ok(out);
            }
            if used == 0 && self.zlib.total_out() == written {
                return Err("the server sent incomplete compressed data".into());
            }
        }
    }

    /// ZRLE: 64×64 tiles of compact three-byte pixels, raw, solid, palette-packed or
    /// run-length encoded.
    fn zrle(&mut self, rect: Rect, data: &[u8]) -> Result<(), String> {
        let mut input = Input { data, at: 0 };
        let right = usize::from(rect.x) + usize::from(rect.width);
        let bottom = usize::from(rect.y) + usize::from(rect.height);
        for top in (usize::from(rect.y)..bottom).step_by(64) {
            for left in (usize::from(rect.x)..right).step_by(64) {
                let (w, h) = ((right - left).min(64), (bottom - top).min(64));
                let kind = input.byte()?;
                match kind {
                    0 => {
                        for i in 0..w * h {
                            let pixel = input.take(3)?;
                            self.set(left + i % w, top + i / w, pixel);
                        }
                    }
                    1 => {
                        let pixel = input.take(3)?;
                        self.fill(left, top, w, h, pixel);
                    }
                    2..=16 => {
                        let palette = input.take(usize::from(kind) * 3)?.to_vec();
                        let bits = match kind {
                            2 => 1,
                            3 | 4 => 2,
                            _ => 4,
                        };
                        for y in 0..h {
                            let row = input.take((w * bits).div_ceil(8))?;
                            for x in 0..w {
                                let bit = x * bits;
                                let shift = 8 - bits - bit % 8;
                                let index = usize::from(row[bit / 8] >> shift) & ((1 << bits) - 1);
                                let pixel = palette
                                    .get(index * 3..index * 3 + 3)
                                    .ok_or("the server sent a pixel outside its palette")?;
                                self.set(left + x, top + y, pixel);
                            }
                        }
                    }
                    128 => {
                        let mut i = 0;
                        while i < w * h {
                            let pixel = input.take(3)?.to_vec();
                            let run = input.run()?;
                            for j in i..(i + run).min(w * h) {
                                self.set(left + j % w, top + j / w, &pixel);
                            }
                            i += run;
                        }
                    }
                    130..=255 => {
                        let palette = input.take(usize::from(kind - 128) * 3)?.to_vec();
                        let mut i = 0;
                        while i < w * h {
                            let index = input.byte()?;
                            let run = if index & 128 != 0 { input.run()? } else { 1 };
                            let index = usize::from(index & 127);
                            let pixel = palette
                                .get(index * 3..index * 3 + 3)
                                .ok_or("the server sent a pixel outside its palette")?;
                            for j in i..(i + run).min(w * h) {
                                self.set(left + j % w, top + j / w, pixel);
                            }
                            i += run;
                        }
                    }
                    _ => return Err(format!("the server sent an unknown ZRLE tile ({kind})")),
                }
            }
        }
        Ok(())
    }
}

/// A screen's side, which must be at least a pixel and at most `MAX_SIDE`.
pub fn valid_side(side: u16) -> Result<u16, String> {
    if side == 0 || side > MAX_SIDE {
        return Err(format!(
            "the server asked for an unusable screen size ({side} pixels)"
        ));
    }
    Ok(side)
}

fn black(pixels: usize) -> Vec<u8> {
    [0, 0, 0, 255].repeat(pixels)
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

/// Decompressed ZRLE data being read.
struct Input<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let bytes = self
            .data
            .get(self.at..self.at + n)
            .ok_or("the server sent a truncated update")?;
        self.at += n;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    /// A run length: bytes added up while they are 255, plus one.
    fn run(&mut self) -> Result<usize, String> {
        let mut run = 1;
        loop {
            let b = self.byte()?;
            run += usize::from(b);
            if b != 255 {
                return Ok(run);
            }
        }
    }
}

/// The pointer shape from its pixels and the bitmask of which ones show.
fn cursor(rect: Rect, colors: &[u8], mask: &[u8]) -> Cursor {
    let (width, height) = (usize::from(rect.width), usize::from(rect.height));
    let row = width.div_ceil(8);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let shown = mask[y * row + x / 8] & (0x80 >> (x % 8)) != 0;
            rgba.extend_from_slice(&[
                colors[i + 2],
                colors[i + 1],
                colors[i],
                if shown { 255 } else { 0 },
            ]);
        }
    }
    Cursor {
        size: [width, height],
        hotspot: [rect.x, rect.y],
        rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn pixel(screen: &Screen, x: usize, y: usize) -> [u8; 4] {
        let i = (y * screen.width + x) * 4;
        screen.rgba[i..i + 4].try_into().unwrap()
    }

    async fn apply(screen: &mut Screen, r: Rect, encoding: i32, data: &[u8]) -> Change {
        let mut reader = data;
        let change = screen.apply(&mut reader, r, encoding).await.unwrap();
        assert!(reader.is_empty(), "{} bytes were left unread", reader.len());
        change
    }

    #[tokio::test]
    async fn raw_and_copied_pixels_land_in_place() {
        let mut screen = Screen::new(4, 3);
        assert_eq!(pixel(&screen, 0, 0), [0, 0, 0, 255]);
        // Blue, green, red, padding: a red and a blue pixel.
        let data = [0, 0, 200, 0, 200, 0, 0, 0];
        let change = apply(&mut screen, rect(1, 1, 2, 1), RAW, &data).await;
        assert_eq!(change, Change::Pixels(rect(1, 1, 2, 1)));
        assert_eq!(pixel(&screen, 1, 1), [200, 0, 0, 255]);
        assert_eq!(pixel(&screen, 2, 1), [0, 0, 200, 255]);
        // Copy them one row down and one to the left, overlapping the source.
        apply(&mut screen, rect(0, 1, 2, 2), COPY_RECT, &[0, 1, 0, 1]).await;
        assert_eq!(pixel(&screen, 0, 1), [200, 0, 0, 255]);
        assert_eq!(pixel(&screen, 1, 1), [0, 0, 200, 255]);
        assert_eq!(pixel(&screen, 0, 2), [0, 0, 0, 255]);
    }

    #[tokio::test]
    async fn rectangles_outside_the_screen_are_refused_before_reading() {
        let mut screen = Screen::new(2, 2);
        for encoding in [RAW, COPY_RECT, HEXTILE, ZRLE] {
            // A huge rectangle would otherwise ask for gigabytes.
            let mut reader = &[][..];
            let huge = rect(0, 0, u16::MAX, u16::MAX);
            assert!(screen.apply(&mut reader, huge, encoding).await.is_err());
            let mut reader = &[][..];
            assert!(
                screen
                    .apply(&mut reader, rect(1, 1, 2, 1), encoding)
                    .await
                    .is_err()
            );
        }
        let mut reader = &[][..];
        let pointer = rect(0, 0, 1000, 1000);
        assert!(screen.apply(&mut reader, pointer, CURSOR).await.is_err());
        // Copying from outside the screen reads black rather than past it.
        apply(&mut screen, rect(0, 0, 2, 2), COPY_RECT, &[0, 9, 0, 9]).await;
        assert_eq!(pixel(&screen, 1, 1), [0, 0, 0, 255]);
    }

    #[tokio::test]
    async fn hextile_tiles_with_backgrounds_and_subrectangles() {
        let mut screen = Screen::new(20, 16);
        let mut data = vec![];
        // First tile, 16×16: a white background with one green 2×3 subrectangle at 4,5.
        data.extend([2 | 4 | 8, 255, 255, 255, 0, 0, 200, 0, 0, 1, 0x45, 0x12]);
        // Second tile, 4×16: the same background, with one coloured subrectangle.
        data.extend([8 | 16, 1, 10, 20, 30, 0, 0x00, 0x00]);
        apply(&mut screen, rect(0, 0, 20, 16), HEXTILE, &data).await;
        assert_eq!(pixel(&screen, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&screen, 4, 5), [0, 200, 0, 255]);
        assert_eq!(pixel(&screen, 5, 7), [0, 200, 0, 255]);
        assert_eq!(pixel(&screen, 6, 5), [255, 255, 255, 255]);
        assert_eq!(pixel(&screen, 16, 0), [30, 20, 10, 255]);
        assert_eq!(pixel(&screen, 17, 0), [255, 255, 255, 255]);
        // A raw tile.
        let mut raw = vec![1];
        raw.extend([9u8, 8, 7, 0].repeat(4));
        apply(&mut screen, rect(0, 0, 2, 2), HEXTILE, &raw).await;
        assert_eq!(pixel(&screen, 1, 1), [7, 8, 9, 255]);
    }

    /// ZRLE data for `tiles`, compressed by `compress` as a server's single stream would be.
    fn zrle(compress: &mut Compress, tiles: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(tiles.len() + 64);
        compress
            .compress_vec(tiles, &mut out, FlushCompress::Sync)
            .unwrap();
        let mut message = (out.len() as u32).to_be_bytes().to_vec();
        message.extend(out);
        message
    }

    #[tokio::test]
    async fn zrle_tiles_of_every_kind_share_one_stream() {
        let mut screen = Screen::new(70, 4);
        let mut compress = Compress::new(Compression::default(), true);
        // A solid 64×4 tile, then a 6×4 one packed from a two-colour palette.
        let mut tiles = vec![1, 1, 2, 3];
        tiles.extend([2, 0, 0, 0, 255, 255, 255]);
        tiles.extend([0b1010_1000, 0b0000_0000, 0b1111_1100, 0b0100_0000]);
        let data = zrle(&mut compress, &tiles);
        apply(&mut screen, rect(0, 0, 70, 4), ZRLE, &data).await;
        assert_eq!(pixel(&screen, 63, 3), [3, 2, 1, 255]);
        assert_eq!(pixel(&screen, 64, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&screen, 65, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&screen, 69, 2), [255, 255, 255, 255]);
        assert_eq!(pixel(&screen, 65, 3), [255, 255, 255, 255]);
        // The next rectangle continues the same stream: plain RLE, then palette RLE.
        let tiles = [
            128, 10, 20, 30, 2, // three pixels
            40, 50, 60, 255, 0, // 1 + 255 + 0 = 256, more than the rest of the 8×4 tile
        ];
        let data = zrle(&mut compress, &tiles);
        apply(&mut screen, rect(0, 0, 8, 4), ZRLE, &data).await;
        assert_eq!(pixel(&screen, 2, 0), [30, 20, 10, 255]);
        assert_eq!(pixel(&screen, 3, 0), [60, 50, 40, 255]);
        assert_eq!(pixel(&screen, 7, 3), [60, 50, 40, 255]);
        let tiles = [130, 1, 1, 1, 9, 9, 9, 0x81, 1, 0, 0x81, 2];
        let data = zrle(&mut compress, &tiles);
        apply(&mut screen, rect(0, 0, 3, 2), ZRLE, &data).await;
        // Index 1 twice, index 0 once, then index 1 for the remaining three.
        assert_eq!(pixel(&screen, 0, 0), [9, 9, 9, 255]);
        assert_eq!(pixel(&screen, 2, 0), [1, 1, 1, 255]);
        assert_eq!(pixel(&screen, 2, 1), [9, 9, 9, 255]);
    }

    #[tokio::test]
    async fn damaged_zrle_is_an_error_not_a_crash() {
        let mut screen = Screen::new(8, 8);
        let mut compress = Compress::new(Compression::default(), true);
        // A raw tile cut short.
        let data = zrle(&mut compress, &[0, 1, 2]);
        let mut reader = &data[..];
        assert!(
            screen
                .apply(&mut reader, rect(0, 0, 8, 8), ZRLE)
                .await
                .is_err()
        );
        // Garbage that is not zlib at all.
        let mut screen = Screen::new(8, 8);
        let data = [0, 0, 0, 4, 1, 2, 3, 4];
        let mut reader = &data[..];
        assert!(
            screen
                .apply(&mut reader, rect(0, 0, 8, 8), ZRLE)
                .await
                .is_err()
        );
        // A palette index beyond the palette.
        let mut screen = Screen::new(2, 1);
        let mut compress = Compress::new(Compression::default(), true);
        let data = zrle(&mut compress, &[130, 1, 1, 1, 9, 9, 9, 5, 5]);
        let mut reader = &data[..];
        assert!(
            screen
                .apply(&mut reader, rect(0, 0, 2, 1), ZRLE)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sizes_and_cursors() {
        let mut screen = Screen::new(4, 4);
        let change = apply(&mut screen, rect(0, 0, 8, 6), DESKTOP_SIZE, &[]).await;
        assert_eq!(change, Change::Resized);
        assert_eq!(
            (screen.width, screen.height, screen.rgba.len()),
            (8, 6, 8 * 6 * 4)
        );
        let mut reader = &[][..];
        assert!(
            screen
                .apply(&mut reader, rect(0, 0, 0, 6), DESKTOP_SIZE)
                .await
                .is_err()
        );
        // An extended size with one screen; a refused one changes nothing.
        let mut data = vec![1, 0, 0, 0];
        data.extend([0u8; 16]);
        assert_eq!(
            apply(&mut screen, rect(1, 0, 10, 5), EXTENDED_DESKTOP_SIZE, &data).await,
            Change::Resized
        );
        assert_eq!((screen.width, screen.height), (10, 5));
        assert_eq!(
            apply(
                &mut screen,
                rect(1, 3, 20, 20),
                EXTENDED_DESKTOP_SIZE,
                &data
            )
            .await,
            Change::Nothing
        );
        // A 2×2 cursor with its hot spot at 1,0 and only the top-left pixel shown.
        let mut data = [10, 20, 30, 0].repeat(4);
        data.extend([0b1000_0000, 0b0000_0000]);
        let Change::Cursor(cursor) = apply(&mut screen, rect(1, 0, 2, 2), CURSOR, &data).await
        else {
            panic!("expected a cursor");
        };
        assert_eq!((cursor.size, cursor.hotspot), ([2, 2], [1, 0]));
        assert_eq!(&cursor.rgba[..4], &[30, 20, 10, 255]);
        assert_eq!(cursor.rgba[7], 0);
        let mut reader = &[][..];
        assert!(
            screen
                .apply(&mut reader, rect(0, 0, 1, 1), 7)
                .await
                .is_err()
        );
    }
}
