// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Parsed glyph-data payloads (see FORMAT.md), shared by the model codec and
//! payload serializer. Serialization uses canonical varints and triplets.

use crate::model::{Component, DecodedGlyph, Payload, Shape, FIXED_HEADER_SIZE, NUM_STRINGS};
use crate::Error;

const NUM_STREAMS: usize = 7;

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, Error> {
        let b = *self.bytes.get(self.pos).ok_or(Error::InvalidData)?;
        self.pos += 1;
        Ok(b)
    }

    fn i16(&mut self) -> Result<i64, Error> {
        Ok(i16::from_be_bytes([self.u8()?, self.u8()?]) as i64)
    }

    fn varint(&mut self) -> Result<u64, Error> {
        let mut value = 0;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            value += ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(value);
            }
        }
        Err(Error::InvalidData)
    }

    fn zigzag(&mut self) -> Result<i64, Error> {
        let v = self.varint()? as i64;
        Ok(if v % 2 != 0 { -(v + 1) / 2 } else { v / 2 })
    }

    fn take(&mut self, length: u64) -> Result<&'a [u8], Error> {
        let end = usize::try_from(length)
            .ok()
            .and_then(|length| self.pos.checked_add(length))
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Error::InvalidData)?;
        let data = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(data)
    }
}

/// WOFF2 "triplet" decoding of a point delta: (on-curve, dx, dy).
fn read_triplet(flags: &mut Reader, coords: &mut Reader) -> Result<(u8, i64, i64), Error> {
    let mut flag = flags.u8()?;
    let on = u8::from(flag >> 7 == 0);
    flag &= 0x7f;
    let (mut dx, mut dy);
    if flag < 10 {
        dx = 0;
        dy = (((flag & 14) as i64) << 7) + coords.u8()? as i64;
        if flag & 1 == 0 {
            dy = -dy;
        }
        return Ok((on, dx, dy));
    }
    if flag < 20 {
        dx = ((((flag - 10) & 14) as i64) << 7) + coords.u8()? as i64;
        if flag & 1 == 0 {
            dx = -dx;
        }
        return Ok((on, dx, 0));
    }
    if flag < 84 {
        let b0 = (flag - 20) as i64;
        let b1 = coords.u8()? as i64;
        dx = 1 + (b0 & 0x30) + (b1 >> 4);
        dy = 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f);
    } else if flag < 120 {
        let b0 = (flag - 84) as i64;
        dx = 1 + ((b0 / 12) << 8) + coords.u8()? as i64;
        dy = 1 + (((b0 % 12) >> 2) << 8) + coords.u8()? as i64;
    } else if flag < 124 {
        let b1 = coords.u8()? as i64;
        let b2 = coords.u8()? as i64;
        dx = (b1 << 4) + (b2 >> 4);
        dy = ((b2 & 0x0f) << 8) + coords.u8()? as i64;
    } else {
        dx = ((coords.u8()? as i64) << 8) + coords.u8()? as i64;
        dy = ((coords.u8()? as i64) << 8) + coords.u8()? as i64;
    }
    if flag & 1 == 0 {
        dx = -dx;
    }
    if flag & 2 == 0 {
        dy = -dy;
    }
    Ok((on, dx, dy))
}

/// Parses an uncompressed payload.
pub(crate) fn parse(bytes: &[u8]) -> Result<Payload, Error> {
    let mut r = Reader { bytes, pos: 0 };
    let mut fixed = [0; FIXED_HEADER_SIZE];
    fixed.copy_from_slice(r.take(FIXED_HEADER_SIZE as u64)?);
    let num_glyphs = r.varint()?;
    let mut strings = Vec::with_capacity(NUM_STRINGS);
    for _ in 0..NUM_STRINGS {
        let length = r.varint()?;
        strings.push(r.take(length)?.to_vec());
    }
    let mut streams = Vec::with_capacity(NUM_STREAMS);
    for _ in 0..NUM_STREAMS {
        let length = r.varint()?;
        streams.push(Reader {
            bytes: r.take(length)?,
            pos: 0,
        });
    }
    if r.pos != bytes.len() {
        return Err(Error::InvalidData);
    }
    let [cmap, glyph_infos, advances, contours, flags, coordinates, components] = &mut streams[..]
    else {
        unreachable!()
    };

    let first_gid = cmap.varint()?;
    let mut lists = [Vec::new(), Vec::new()];
    for list in &mut lists {
        for _ in 0..cmap.varint()? * 2 {
            list.push(cmap.varint()?);
        }
    }
    let [runs, aliases] = lists;

    let mut glyphs = Vec::new();
    for _ in 0..num_glyphs {
        let info = glyph_infos.varint()?;
        let advance = advances.varint()? as i64;
        let count = info / 2;
        let shape = if info == 0 {
            Shape::Empty
        } else if info == 1 {
            // A composite glyph without components.
            return Err(Error::InvalidData);
        } else if info & 1 != 0 {
            let mut list = Vec::new();
            for _ in 0..count {
                let header = components.varint()?;
                let transform = (header % 4) as u32;
                let gid = (header / 4) as i64;
                let dx = components.zigzag()?;
                let dy = components.zigzag()?;
                let num_values = [0, 1, 2, 4][transform as usize];
                let values = (0..num_values)
                    .map(|_| components.i16())
                    .collect::<Result<_, _>>()?;
                list.push(Component {
                    gid,
                    transform,
                    dx,
                    dy,
                    values,
                });
            }
            Shape::Composite(list)
        } else {
            let mut sizes = Vec::new();
            for _ in 0..count {
                let size = contours.varint()? + 1;
                sizes.push(u16::try_from(size).map_err(|_| Error::InvalidData)?);
            }
            let total: usize = sizes.iter().map(|&s| s as usize).sum();
            let (mut on, mut x, mut y) = (Vec::new(), Vec::new(), Vec::new());
            let (mut px, mut py) = (0i64, 0i64);
            for _ in 0..total {
                let (o, dx, dy) = read_triplet(flags, coordinates)?;
                px += dx;
                py += dy;
                on.push(o);
                x.push(px as i32);
                y.push(py as i32);
            }
            Shape::Simple { sizes, on, x, y }
        };
        glyphs.push(DecodedGlyph { shape, advance });
    }
    // Reject bytes not consumed by the declared cmap and glyphs.
    if streams
        .iter()
        .any(|stream| stream.pos != stream.bytes.len())
    {
        return Err(Error::InvalidData);
    }
    Ok(Payload {
        fixed,
        strings,
        first_gid,
        runs,
        aliases,
        glyphs,
    })
}

/// A growable byte buffer, for the streams of the payload.
#[derive(Default)]
pub(crate) struct Writer {
    pub data: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.data.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i16(&mut self, v: i64) -> Result<(), String> {
        let v = i16::try_from(v).map_err(|_| format!("Value out of int16 range: {v}"))?;
        self.data.extend_from_slice(&v.to_be_bytes());
        Ok(())
    }

    pub fn varint(&mut self, mut v: u64) -> Result<(), String> {
        if v > 0xffff_ffff {
            return Err(format!("Invalid varint: {v}"));
        }
        while v >= 0x80 {
            self.u8((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
        self.u8(v as u8);
        Ok(())
    }

    pub fn zigzag(&mut self, v: i64) -> Result<(), String> {
        self.varint(if v >= 0 {
            v as u64 * 2
        } else {
            (-v) as u64 * 2 - 1
        })
    }

    fn sized_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        self.varint(data.len() as u64)?;
        self.bytes(data);
        Ok(())
    }

    pub fn bytes(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }
}

/// WOFF2 "triplet" encoding of a point delta (see the WOFF2 specification,
/// section 5.2): one flag byte (bit 7 marks off-curve points) and 1 to 4
/// coordinate bytes.
pub(crate) fn write_triplet(
    flags: &mut Writer,
    coords: &mut Writer,
    on: bool,
    dx: i64,
    dy: i64,
) -> Result<(), String> {
    let on_curve_bit = if on { 0 } else { 128 };
    let (abs_x, abs_y) = (dx.unsigned_abs(), dy.unsigned_abs());
    let x_sign = u64::from(dx >= 0);
    let y_sign = u64::from(dy >= 0);
    let signs = x_sign + 2 * y_sign;
    if abs_x > 0xffff || abs_y > 0xffff {
        return Err("Point delta is too large.".to_string());
    }
    let flag = |v: u64| (on_curve_bit + v) as u8;
    if dx == 0 && abs_y < 1280 {
        flags.u8(flag(((abs_y & 0xf00) >> 7) + y_sign));
        coords.u8(abs_y as u8);
    } else if dy == 0 && abs_x < 1280 {
        flags.u8(flag(10 + ((abs_x & 0xf00) >> 7) + x_sign));
        coords.u8(abs_x as u8);
    } else if abs_x < 65 && abs_y < 65 {
        flags.u8(flag(
            20 + ((abs_x - 1) & 0x30) + (((abs_y - 1) & 0x30) >> 2) + signs,
        ));
        coords.u8(((((abs_x - 1) & 0xf) << 4) | ((abs_y - 1) & 0xf)) as u8);
    } else if abs_x < 769 && abs_y < 769 {
        flags.u8(flag(
            84 + 12 * (((abs_x - 1) & 0x300) >> 8) + (((abs_y - 1) & 0x300) >> 6) + signs,
        ));
        coords.u8((abs_x - 1) as u8);
        coords.u8((abs_y - 1) as u8);
    } else if abs_x < 4096 && abs_y < 4096 {
        flags.u8(flag(120 + signs));
        coords.u8((abs_x >> 4) as u8);
        coords.u8((((abs_x & 0xf) << 4) | (abs_y >> 8)) as u8);
        coords.u8(abs_y as u8);
    } else {
        flags.u8(flag(124 + signs));
        coords.u16(abs_x as u16);
        coords.u16(abs_y as u16);
    }
    Ok(())
}

/// Serializes a payload.
pub(crate) fn serialize(payload: &Payload) -> Result<Vec<u8>, String> {
    let mut streams: [Writer; NUM_STREAMS] = Default::default();
    let [cmap, glyph_infos, advances, contours, flags, coordinates, components] = &mut streams;
    cmap.varint(payload.first_gid)?;
    for list in [&payload.runs, &payload.aliases] {
        cmap.varint(list.len() as u64 / 2)?;
        for &v in list.iter() {
            cmap.varint(v)?;
        }
    }
    for glyph in &payload.glyphs {
        advances.varint(glyph.advance as u64)?;
        match &glyph.shape {
            Shape::Empty => glyph_infos.varint(0)?,
            Shape::Composite(list) => {
                glyph_infos.varint(list.len() as u64 * 2 + 1)?;
                for c in list {
                    components.varint(c.gid as u64 * 4 + c.transform as u64)?;
                    components.zigzag(c.dx)?;
                    components.zigzag(c.dy)?;
                    for &v in &c.values {
                        components.i16(v)?;
                    }
                }
            }
            Shape::Simple { sizes, on, x, y } => {
                glyph_infos.varint(sizes.len() as u64 * 2)?;
                for &size in sizes {
                    contours.varint(size as u64 - 1)?;
                }
                let (mut px, mut py) = (0i64, 0i64);
                for i in 0..on.len() {
                    let (cx, cy) = (x[i] as i64, y[i] as i64);
                    write_triplet(flags, coordinates, on[i] != 0, cx - px, cy - py)?;
                    (px, py) = (cx, cy);
                }
            }
        }
    }
    serialize_streams(
        &payload.fixed,
        payload.glyphs.len(),
        &payload.strings,
        &streams,
    )
}

/// A payload from its parts: the fixed header, the number of glyphs, the
/// strings (family, version, copyright...) and the streams, the strings and
/// the streams being prefixed by their length.
pub(crate) fn serialize_streams(
    fixed: &[u8],
    num_glyphs: usize,
    strings: &[impl AsRef<[u8]>],
    streams: &[Writer; NUM_STREAMS],
) -> Result<Vec<u8>, String> {
    let mut out = Writer::default();
    out.bytes(fixed);
    out.varint(num_glyphs as u64)?;
    for string in strings {
        out.sized_bytes(string.as_ref())?;
    }
    for stream in streams {
        out.sized_bytes(&stream.data)?;
    }
    Ok(out.data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{decode_model_payload, encode_model_payload, EncoderOptions};

    /// A payload with a composite glyph of `count` components.
    fn composite(count: usize) -> Vec<u8> {
        let component = Component {
            gid: 0,
            transform: 0,
            dx: 0,
            dy: 0,
            values: Vec::new(),
        };
        let glyph = |shape| DecodedGlyph { shape, advance: 0 };
        serialize(&Payload {
            fixed: [0; FIXED_HEADER_SIZE],
            strings: vec![Vec::new(); NUM_STRINGS],
            first_gid: 0,
            runs: Vec::new(),
            aliases: Vec::new(),
            glyphs: vec![
                glyph(Shape::Empty),
                glyph(Shape::Composite(vec![component; count])),
            ],
        })
        .unwrap()
    }

    #[test]
    fn rejects_the_composite_glyphs_without_components() {
        assert!(parse(&composite(1)).is_ok());
        assert_eq!(parse(&composite(0)).err(), Some(Error::InvalidData));
        assert_eq!(
            encode_model_payload(&composite(0), EncoderOptions::default()).err(),
            Some(Error::InvalidData)
        );
    }

    /// The cmap stream without runs and aliases.
    const NO_CMAP: &[u8] = &[0, 0, 0];

    /// A payload of empty glyphs, with these streams of the cmap, the glyph
    /// infos and the advances (the streams of the outlines being empty).
    fn empty_glyphs(num_glyphs: u64, cmap: &[u8], glyph_infos: &[u8], advances: &[u8]) -> Vec<u8> {
        let mut w = Writer::default();
        w.bytes(&[0; FIXED_HEADER_SIZE]);
        w.varint(num_glyphs).unwrap();
        w.bytes(&[0; NUM_STRINGS]);
        let streams: [&[u8]; NUM_STREAMS] = [cmap, glyph_infos, advances, &[], &[], &[], &[]];
        for stream in streams {
            w.varint(stream.len() as u64).unwrap();
            w.bytes(stream);
        }
        w.data
    }

    #[test]
    fn rejects_the_unused_bytes() {
        let bytes = empty_glyphs(1, NO_CMAP, &[0], &[0]);
        assert!(serialize(&parse(&bytes).unwrap()).unwrap() == bytes);
        let bytes = empty_glyphs(1, NO_CMAP, &[0, 42], &[0]);
        assert_eq!(parse(&bytes).err(), Some(Error::InvalidData));
    }

    /// The codec gives back the very same bytes, else the payload is
    /// rejected.
    #[test]
    fn rejects_the_payloads_which_cant_be_coded_exactly() {
        let encode = |payload: &[u8]| encode_model_payload(payload, EncoderOptions::default());
        let payload = empty_glyphs(1, NO_CMAP, &[0], &[0]);
        assert!(decode_model_payload(&encode(&payload).unwrap()).unwrap() == payload);
        let many = vec![0; (1 << 21) + 1];
        let cases = [
            // An advance of 2^32.
            (
                "huge advance",
                empty_glyphs(1, NO_CMAP, &[0], &[0x80, 0x80, 0x80, 0x80, 0x10]),
            ),
            // A run of 2^32 codepoints (an unsigned value of 0xFFFFFFFF).
            (
                "huge cmap value",
                empty_glyphs(1, &[0, 1, 0, 0xff, 0xff, 0xff, 0xff, 0x0f, 0], &[0], &[0]),
            ),
            (
                "overlong varint",
                empty_glyphs(1, NO_CMAP, &[0x80, 0], &[0]),
            ),
            (
                "too many glyphs",
                empty_glyphs(many.len() as u64, NO_CMAP, &many, &many),
            ),
        ];
        for (name, payload) in cases {
            assert!(parse(&payload).is_ok(), "{name}");
            assert_eq!(encode(&payload).err(), Some(Error::InvalidData), "{name}");
        }
    }
}
