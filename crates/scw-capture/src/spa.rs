//! PipeWire's parameter format (SPA "pods"), as far as a screen capture
//! needs it: the formats sc-watch accepts, the buffer kinds it can read, and
//! the format the stream settled on. Written by hand from the headers in
//! `spa/pod/pod.h`, `spa/param/format.h`, `spa/param/video/raw.h` and
//! `spa/param/buffers.h`, so the program needs no PipeWire headers or C
//! compiler to build (the library itself is loaded at run time).
//!
//! A pod is a header - the body's size and a type, two `u32`s - then the
//! body, padded to 8 bytes. An object's body is its type and id, then
//! properties: a key, flags, and a pod each. A choice holds one child header
//! and then its values packed together (default first).
//!
//! Plain Rust with no system calls, so it is tested on every system.

use scw_vision::Image;

// spa/utils/type.h
const TYPE_ID: u32 = 3;
const TYPE_INT: u32 = 4;
const TYPE_RECTANGLE: u32 = 10;
const TYPE_FRACTION: u32 = 11;
const TYPE_OBJECT: u32 = 15;
const TYPE_CHOICE: u32 = 19;
const TYPE_OBJECT_FORMAT: u32 = 0x40003;
const TYPE_OBJECT_PARAM_BUFFERS: u32 = 0x40004;

// spa/param/param.h
pub const PARAM_ENUM_FORMAT: u32 = 3;
pub const PARAM_FORMAT: u32 = 4;
pub const PARAM_BUFFERS: u32 = 5;

// spa/param/format.h
const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const FORMAT_VIDEO_SIZE: u32 = 0x20003;
const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const MEDIA_TYPE_VIDEO: u32 = 2;
const MEDIA_SUBTYPE_RAW: u32 = 1;

// spa/param/buffers.h, spa/buffer/buffer.h
const PARAM_BUFFERS_DATA_TYPE: u32 = 6;
pub const DATA_MEM_PTR: u32 = 1;
pub const DATA_MEM_FD: u32 = 2;
pub const DATA_DMA_BUF: u32 = 3;

// spa/pod/pod.h
const CHOICE_RANGE: u32 = 1;
const CHOICE_ENUM: u32 = 3;
const CHOICE_FLAGS: u32 = 4;

/// The 8-bit, 4-bytes-a-pixel layouts a desktop hands out (spa/param/video/
/// raw.h's numbers), named by their bytes in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Rgbx = 7,
    Bgrx = 8,
    Xrgb = 9,
    Xbgr = 10,
    Rgba = 11,
    Bgra = 12,
    Argb = 13,
    Abgr = 14,
}

impl Layout {
    pub fn from_id(id: u32) -> Option<Layout> {
        use Layout::*;
        Some(match id {
            7 => Rgbx,
            8 => Bgrx,
            9 => Xrgb,
            10 => Xbgr,
            11 => Rgba,
            12 => Bgra,
            13 => Argb,
            14 => Abgr,
            _ => return None,
        })
    }

    /// Where blue, green and red sit in a pixel's four bytes.
    fn bgr_at(self) -> [usize; 3] {
        use Layout::*;
        match self {
            Bgrx | Bgra => [0, 1, 2],
            Rgbx | Rgba => [2, 1, 0],
            Xrgb | Argb => [3, 2, 1],
            Xbgr | Abgr => [1, 2, 3],
        }
    }
}

/// What a stream settled on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoFormat {
    /// spa_video_format; [`Layout::from_id`] says whether it is readable.
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

struct Pod(Vec<u8>);

impl Pod {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_ne_bytes());
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }

    /// A header whose size is filled in by [`Pod::close`].
    fn open(&mut self, kind: u32) -> usize {
        let at = self.0.len();
        self.u32(0);
        self.u32(kind);
        at
    }

    fn close(&mut self, at: usize) {
        let size = (self.0.len() - at - 8) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_ne_bytes());
        self.pad();
    }

    fn id(&mut self, v: u32) {
        self.u32(4);
        self.u32(TYPE_ID);
        self.u32(v);
        self.pad();
    }

    /// A choice of `kind` among `values`, each `child_size` bytes of
    /// `child_type`; the first is the default.
    fn choice(&mut self, kind: u32, child_type: u32, child_size: u32, values: &[&[u32]]) {
        let at = self.open(TYPE_CHOICE);
        self.u32(kind);
        self.u32(0); // flags
        self.u32(child_size);
        self.u32(child_type);
        for v in values {
            for w in *v {
                self.u32(*w);
            }
        }
        self.close(at);
    }

    fn object(&mut self, kind: u32, id: u32, props: impl FnOnce(&mut Pod)) {
        let at = self.open(TYPE_OBJECT);
        self.u32(kind);
        self.u32(id);
        props(self);
        self.close(at);
    }

    fn prop(&mut self, key: u32) {
        self.u32(key);
        self.u32(0); // flags
    }
}

/// The formats sc-watch takes (EnumFormat): raw video in any 4-byte 8-bit
/// layout, any size, any rate. No DMA-BUF modifiers are offered, so the
/// desktop sends memory the program can read directly.
pub fn enum_format() -> Vec<u8> {
    let mut p = Pod(Vec::new());
    p.object(TYPE_OBJECT_FORMAT, PARAM_ENUM_FORMAT, |p| {
        p.prop(FORMAT_MEDIA_TYPE);
        p.id(MEDIA_TYPE_VIDEO);
        p.prop(FORMAT_MEDIA_SUBTYPE);
        p.id(MEDIA_SUBTYPE_RAW);
        p.prop(FORMAT_VIDEO_FORMAT);
        let ids = [
            Layout::Bgrx,
            Layout::Bgrx,
            Layout::Bgra,
            Layout::Rgbx,
            Layout::Rgba,
            Layout::Xrgb,
            Layout::Xbgr,
            Layout::Argb,
            Layout::Abgr,
        ]
        .map(|l| [l as u32]);
        let ids: Vec<&[u32]> = ids.iter().map(|a| a.as_slice()).collect();
        p.choice(CHOICE_ENUM, TYPE_ID, 4, &ids);
        p.prop(FORMAT_VIDEO_SIZE);
        p.choice(
            CHOICE_RANGE,
            TYPE_RECTANGLE,
            8,
            &[&[1920, 1080], &[1, 1], &[16384, 16384]],
        );
        p.prop(FORMAT_VIDEO_FRAMERATE);
        p.choice(
            CHOICE_RANGE,
            TYPE_FRACTION,
            8,
            &[&[30, 1], &[0, 1], &[1000, 1]],
        );
    });
    p.0
}

/// The buffers sc-watch can read (Buffers): plain or shared memory.
pub fn buffers() -> Vec<u8> {
    let mut p = Pod(Vec::new());
    p.object(TYPE_OBJECT_PARAM_BUFFERS, PARAM_BUFFERS, |p| {
        p.prop(PARAM_BUFFERS_DATA_TYPE);
        let mask = (1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD);
        p.choice(CHOICE_FLAGS, TYPE_INT, 4, &[&[mask]]);
    });
    p.0
}

/// A pod copied into 8-byte-aligned memory, as PipeWire reads it.
pub fn aligned(bytes: &[u8]) -> Vec<u64> {
    let mut out = vec![0u64; bytes.len().div_ceil(8)];
    for (w, c) in out.iter_mut().zip(bytes.chunks(8)) {
        let mut b = [0u8; 8];
        b[..c.len()].copy_from_slice(c);
        *w = u64::from_ne_bytes(b);
    }
    out
}

fn word(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The value of a property: its type and body, a choice's default taken.
fn value(b: &[u8]) -> Option<(u32, &[u8])> {
    let (size, kind) = (word(b, 0)? as usize, word(b, 4)?);
    let body = b.get(8..8 + size)?;
    if kind != TYPE_CHOICE {
        return Some((kind, body));
    }
    let (child_size, child_type) = (word(body, 8)? as usize, word(body, 12)?);
    Some((child_type, body.get(16..16 + child_size)?))
}

/// The format a stream settled on (a Format object), when it names a type,
/// a size and is video.
pub fn parse_format(b: &[u8]) -> Option<VideoFormat> {
    let size = word(b, 0)? as usize;
    if word(b, 4)? != TYPE_OBJECT || word(b, 8)? != TYPE_OBJECT_FORMAT {
        return None;
    }
    let end = (8 + size).min(b.len());
    let mut at = 16;
    let (mut format, mut dims, mut video) = (None, None, false);
    while at + 16 <= end {
        let key = word(b, at)?;
        let pod = b.get(at + 8..end)?;
        let (kind, body) = value(pod)?;
        match (key, kind) {
            (FORMAT_MEDIA_TYPE, TYPE_ID) => video = word(body, 0)? == MEDIA_TYPE_VIDEO,
            (FORMAT_VIDEO_FORMAT, TYPE_ID) => format = Some(word(body, 0)?),
            (FORMAT_VIDEO_SIZE, TYPE_RECTANGLE) => dims = Some((word(body, 0)?, word(body, 4)?)),
            _ => {}
        }
        let pod_size = word(pod, 0)? as usize;
        at += 8 + 8 + pod_size.next_multiple_of(8);
    }
    let (width, height) = dims?;
    video.then_some(VideoFormat {
        format: format?,
        width,
        height,
    })
}

/// One frame of `layout` pixels, `stride` bytes a row, as BGR. None when
/// the bytes are too few for the size.
pub fn to_bgr(
    data: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    layout: Layout,
) -> Option<Image<u8>> {
    if width == 0 || height == 0 || stride < width * 4 {
        return None;
    }
    if data.len() < stride * (height - 1) + width * 4 {
        return None;
    }
    let [b, g, r] = layout.bgr_at();
    let mut out = Image::new(width, height, 3);
    for (y, row) in out.data.chunks_exact_mut(width * 3).enumerate() {
        let src = &data[y * stride..y * stride + width * 4];
        for (o, px) in row
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0)
        {
            *o = [px[b], px[g], px[r]];
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks(4)
            .map(|c| u32::from_ne_bytes(c.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn buffers_pod_is_laid_out_as_the_headers_say() {
        // Object(ParamBuffers, Buffers) { dataType: Choice(Flags, Int, [6]) }
        // - a parent's size counts its children's padding
        assert_eq!(
            words(&buffers()),
            vec![
                48, 15, 0x40004, 5, // object: body 48 bytes
                6, 0, // key dataType, flags
                20, 19, 4, 0, 4, 4, 6, // choice body 20: Flags, 0, child Int 4 bytes, mask
                0, // padding to 8
            ]
        );
    }

    #[test]
    fn enum_format_pod_is_laid_out_as_the_headers_say() {
        let w = words(&enum_format());
        assert_eq!(w[0] as usize, w.len() * 4 - 8, "the object's size");
        assert_eq!(&w[1..4], &[15, 0x40003, 3]);
        // mediaType: Id video; mediaSubtype: Id raw
        assert_eq!(&w[4..10], &[1, 0, 4, 3, 2, 0]);
        assert_eq!(&w[10..16], &[2, 0, 4, 3, 1, 0]);
        // format: Choice(Enum, Id) of 9, BGRx first (the default) and again
        assert_eq!(&w[16..18], &[0x20001, 0]);
        assert_eq!(&w[18..24], &[16 + 9 * 4, 19, 3, 0, 4, 3]);
        assert_eq!(&w[24..33], &[8, 8, 12, 7, 11, 9, 10, 13, 14]);
        assert_eq!(w[33], 0, "padding");
        // size: Choice(Range, Rectangle) default, min, max
        assert_eq!(&w[34..36], &[0x20003, 0]);
        assert_eq!(
            &w[36..48],
            &[40, 19, 1, 0, 8, 10, 1920, 1080, 1, 1, 16384, 16384]
        );
        // framerate: Choice(Range, Fraction)
        assert_eq!(&w[48..50], &[0x20004, 0]);
        assert_eq!(&w[50..62], &[40, 19, 1, 0, 8, 11, 30, 1, 0, 1, 1000, 1]);
        assert_eq!(w.len(), 62);
    }

    /// A Format object as a producer sends it: plain values, or choices of
    /// one (both appear in practice).
    fn format_pod(layout: u32, w: u32, h: u32, as_choice: bool) -> Vec<u8> {
        let mut p = Pod(Vec::new());
        p.object(TYPE_OBJECT_FORMAT, PARAM_FORMAT, |p| {
            p.prop(FORMAT_MEDIA_TYPE);
            p.id(MEDIA_TYPE_VIDEO);
            p.prop(FORMAT_MEDIA_SUBTYPE);
            p.id(MEDIA_SUBTYPE_RAW);
            p.prop(FORMAT_VIDEO_FORMAT);
            if as_choice {
                p.choice(0, TYPE_ID, 4, &[&[layout]]);
            } else {
                p.id(layout);
            }
            p.prop(0x20002); // a modifier (Long) to step over
            p.u32(8);
            p.u32(5);
            p.u32(0);
            p.u32(0);
            p.prop(FORMAT_VIDEO_SIZE);
            if as_choice {
                p.choice(0, TYPE_RECTANGLE, 8, &[&[w, h]]);
            } else {
                p.u32(8);
                p.u32(TYPE_RECTANGLE);
                p.u32(w);
                p.u32(h);
            }
        });
        p.0
    }

    #[test]
    fn reads_the_settled_format() {
        for choice in [false, true] {
            assert_eq!(
                parse_format(&format_pod(12, 5120, 1440, choice)),
                Some(VideoFormat {
                    format: 12,
                    width: 5120,
                    height: 1440
                })
            );
        }
        // what was offered also parses, to its defaults
        assert_eq!(
            parse_format(&enum_format()),
            Some(VideoFormat {
                format: 8,
                width: 1920,
                height: 1080
            })
        );
        // not a format, or cut short
        assert_eq!(parse_format(&buffers()), None);
        let cut = format_pod(8, 640, 480, false);
        assert_eq!(parse_format(&cut[..cut.len() - 16]), None);
        assert_eq!(parse_format(&[]), None);
    }

    #[test]
    fn every_layout_reads_as_bgr_with_a_stride() {
        // 2x2 pixels: blue=10+i, green=20+i, red=30+i; rows padded to 12 bytes
        let place = |layout: Layout| -> Vec<u8> {
            let mut out = Vec::new();
            for y in 0..2u8 {
                for x in 0..2u8 {
                    let i = y * 2 + x;
                    let [b, g, r] = layout.bgr_at();
                    let mut px = [99u8; 4];
                    px[b] = 10 + i;
                    px[g] = 20 + i;
                    px[r] = 30 + i;
                    out.extend_from_slice(&px);
                }
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            out
        };
        for id in 7..=14 {
            let layout = Layout::from_id(id).unwrap();
            let img = to_bgr(&place(layout), 2, 2, 12, layout).unwrap();
            assert_eq!(
                img.data,
                vec![10, 20, 30, 11, 21, 31, 12, 22, 32, 13, 23, 33],
                "{layout:?}"
            );
        }
        // BGRx's own bytes, spelled out
        let img = to_bgr(&[1, 2, 3, 4], 1, 1, 4, Layout::Bgrx).unwrap();
        assert_eq!(img.data, vec![1, 2, 3]);
        let img = to_bgr(&[1, 2, 3, 4], 1, 1, 4, Layout::Rgbx).unwrap();
        assert_eq!(img.data, vec![3, 2, 1]);
        // the last row needs no padding; one byte fewer is refused
        assert!(to_bgr(&[0; 20], 2, 2, 12, Layout::Bgrx).is_some());
        assert!(to_bgr(&[0; 19], 2, 2, 12, Layout::Bgrx).is_none());
        assert!(
            to_bgr(&[0; 16], 2, 2, 4, Layout::Bgrx).is_none(),
            "stride too small"
        );
        assert_eq!(Layout::from_id(16), None, "3-byte BGR is not offered");
    }

    #[test]
    fn aligned_keeps_the_bytes() {
        let pod = enum_format();
        let a = aligned(&pod);
        let back: Vec<u8> = a.iter().flat_map(|w| w.to_ne_bytes()).collect();
        assert_eq!(&back[..pod.len()], &pod[..]);
    }
}
