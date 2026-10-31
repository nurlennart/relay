//! A remote desktop's picture, shared between the connection thread that decodes it and the
//! UI that uploads the changed parts to a texture, and where the picture sits in its tab.
//! RDP (through IronRDP) and VNC both draw this way.
use eframe::egui::{self, Color32, ColorImage, Pos2, Rect};

/// A rectangle of remote pixels with exclusive right and bottom edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl Region {
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
    fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }
}

/// What the texture needs to catch up with the remote desktop.
#[derive(Debug, PartialEq)]
pub enum Update {
    /// A new picture, after connecting or a resolution change.
    Full(ColorImage),
    /// Pixels changed at `[x, y]`.
    Partial([usize; 2], ColorImage),
}

/// The latest remote picture and the part of it the UI has not uploaded yet.
#[derive(Default)]
pub struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<Color32>,
    dirty: Option<Region>,
    /// The size changed since the last upload, so the texture must be recreated.
    resized: bool,
}

impl Frame {
    /// Starts a black picture of a new size.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![Color32::BLACK; width * height];
        self.dirty = None;
        self.resized = true;
    }

    /// Copies `region` from `rgba`, a whole picture of this frame's size with four bytes per
    /// pixel. Remote pixels are opaque whatever their alpha byte says.
    pub fn copy(&mut self, rgba: &[u8], region: Region) {
        let region = Region {
            right: region.right.min(self.width),
            bottom: region.bottom.min(self.height),
            ..region
        };
        if region.is_empty() || rgba.len() < self.width * self.height * 4 {
            return;
        }
        for y in region.top..region.bottom {
            let row = y * self.width;
            for x in region.left..region.right {
                let i = (row + x) * 4;
                self.pixels[row + x] = Color32::from_rgb(rgba[i], rgba[i + 1], rgba[i + 2]);
            }
        }
        self.dirty = Some(self.dirty.map_or(region, |d| d.union(region)));
    }

    /// Takes what changed since the last call, if anything.
    pub fn take(&mut self) -> Option<Update> {
        if std::mem::take(&mut self.resized) {
            self.dirty = None;
            if self.width == 0 || self.height == 0 {
                return None;
            }
            let image = ColorImage::new([self.width, self.height], self.pixels.clone());
            return Some(Update::Full(image));
        }
        let region = self.dirty.take()?;
        let (width, height) = (region.right - region.left, region.bottom - region.top);
        let mut pixels = Vec::with_capacity(width * height);
        for y in region.top..region.bottom {
            let row = y * self.width;
            pixels.extend_from_slice(&self.pixels[row + region.left..row + region.right]);
        }
        Some(Update::Partial(
            [region.left, region.top],
            ColorImage::new([width, height], pixels),
        ))
    }
}

/// Applies `update` to the desktop's texture, creating it when needed.
pub fn upload(ctx: &egui::Context, texture: &mut Option<egui::TextureHandle>, update: Update) {
    let options = egui::TextureOptions::LINEAR;
    match (update, texture.as_mut()) {
        (Update::Partial(at, image), Some(texture)) => texture.set_partial(at, image, options),
        (Update::Full(image), Some(texture)) => texture.set(image, options),
        (Update::Full(image), None) => {
            *texture = Some(ctx.load_texture("remote-desktop", image, options));
        }
        // A partial update cannot arrive before the first full one.
        (Update::Partial(..), None) => {}
    }
}

/// Where a remote desktop is drawn, to map pointer positions onto it.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// The area the picture covers, in points.
    pub rect: Rect,
    /// The remote desktop size in pixels.
    pub size: [usize; 2],
}

impl View {
    /// The area of `available` that a picture of `size` fills when scaled to fit it without
    /// distortion, centered.
    pub fn fit(available: Rect, size: [usize; 2]) -> Self {
        let [w, h] = size.map(|v| v.max(1) as f32);
        let scale = (available.width() / w).min(available.height() / h);
        let rect = Rect::from_center_size(available.center(), egui::vec2(w * scale, h * scale));
        Self { rect, size }
    }

    /// The remote pixel under `pos`, clamped to the desktop so drags past its edge still end.
    pub fn remote(&self, pos: Pos2) -> [u16; 2] {
        let at = |offset: f32, extent: f32, pixels: usize| {
            let max = pixels.saturating_sub(1) as f32;
            ((offset / extent.max(1.)) * pixels as f32)
                .floor()
                .clamp(0., max) as u16
        };
        [
            at(pos.x - self.rect.min.x, self.rect.width(), self.size[0]),
            at(pos.y - self.rect.min.y, self.rect.height(), self.size[1]),
        ]
    }

    /// Where remote pixel `[x, y]` appears, in points.
    pub fn local(&self, [x, y]: [f32; 2]) -> Pos2 {
        let [w, h] = self.size.map(|v| v.max(1) as f32);
        self.rect.min + egui::vec2(x / w * self.rect.width(), y / h * self.rect.height())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn rgba(width: usize, height: usize, value: u8) -> Vec<u8> {
        vec![value; width * height * 4]
    }

    #[test]
    fn a_new_size_uploads_the_whole_picture_once() {
        let mut frame = Frame::default();
        assert_eq!(frame.take(), None, "nothing before the first picture");
        frame.resize(4, 3);
        let Some(Update::Full(image)) = frame.take() else {
            panic!("expected a full picture");
        };
        assert_eq!(image.size, [4, 3]);
        assert!(image.pixels.iter().all(|p| *p == Color32::BLACK));
        assert_eq!(frame.take(), None, "taken");
    }

    #[test]
    fn changes_accumulate_into_one_partial_update() {
        let mut frame = Frame::default();
        frame.resize(8, 6);
        frame.take();
        let picture = rgba(8, 6, 200);
        frame.copy(
            &picture,
            Region {
                left: 1,
                top: 1,
                right: 3,
                bottom: 2,
            },
        );
        frame.copy(
            &picture,
            Region {
                left: 5,
                top: 3,
                right: 6,
                bottom: 5,
            },
        );
        let Some(Update::Partial(at, image)) = frame.take() else {
            panic!("expected a partial update");
        };
        assert_eq!(at, [1, 1]);
        assert_eq!(image.size, [5, 4], "the union of both regions");
        // The gap between the regions keeps its old pixels.
        assert_eq!(image.pixels[0], Color32::from_rgb(200, 200, 200));
        assert_eq!(image.pixels[5 + 4], Color32::BLACK);
        assert_eq!(frame.take(), None);
    }

    #[test]
    fn remote_pixels_are_opaque_and_regions_are_clipped() {
        let mut frame = Frame::default();
        frame.resize(2, 2);
        frame.take();
        let mut picture = rgba(2, 2, 0);
        picture[4..8].copy_from_slice(&[10, 20, 30, 0]);
        frame.copy(
            &picture,
            Region {
                left: 1,
                top: 0,
                right: 9,
                bottom: 9,
            },
        );
        let Some(Update::Partial(at, image)) = frame.take() else {
            panic!("expected a partial update");
        };
        assert_eq!((at, image.size), ([1, 0], [1, 2]));
        assert_eq!(image.pixels[0], Color32::from_rgb(10, 20, 30));
        assert_eq!(image.pixels[0].a(), 255);
    }

    #[test]
    fn a_resize_replaces_pending_changes() {
        let mut frame = Frame::default();
        frame.resize(4, 4);
        frame.copy(
            &rgba(4, 4, 1),
            Region {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            },
        );
        frame.resize(6, 2);
        assert!(matches!(frame.take(), Some(Update::Full(image)) if image.size == [6, 2]));
        // A short or empty source is ignored rather than read out of bounds.
        frame.copy(
            &[0; 8],
            Region {
                left: 0,
                top: 0,
                right: 6,
                bottom: 2,
            },
        );
        frame.copy(
            &rgba(6, 2, 1),
            Region {
                left: 3,
                top: 1,
                right: 3,
                bottom: 2,
            },
        );
        assert_eq!(frame.take(), None);
    }

    #[test]
    fn pointer_positions_scale_to_remote_pixels() {
        let v = View {
            rect: Rect::from_min_size(pos2(100., 50.), vec2(400., 300.)),
            size: [800, 600],
        };
        assert_eq!(v.remote(pos2(100., 50.)), [0, 0]);
        assert_eq!(v.remote(pos2(300., 200.)), [400, 300]);
        assert_eq!(v.remote(pos2(900., 900.)), [799, 599]);
        assert_eq!(v.remote(pos2(-5., 60.)), [0, 20]);
        assert_eq!(v.local([400., 300.]), pos2(300., 200.));
        // A 16:9 desktop in a square area is letterboxed, centered.
        let fit = View::fit(
            Rect::from_min_size(pos2(0., 0.), vec2(400., 400.)),
            [1600, 900],
        );
        assert_eq!(
            fit.rect,
            Rect::from_min_max(pos2(0., 87.5), pos2(400., 312.5))
        );
    }
}
