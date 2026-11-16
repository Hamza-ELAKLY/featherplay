//! Reads the segment index (`sidx`) of a fragmented MP4, as YouTube serves its audio.
//!
//! The index says where each fragment starts in bytes and in time, which lets the player
//! seek by reopening the stream at a fragment instead of scanning the whole file.

pub struct Index {
    /// Length of the initialization segment (`ftyp` + `moov`) that precedes the index.
    pub init_len: usize,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    /// Seconds from the start of the track.
    pub start: f64,
    /// Byte offset of the fragment in the file.
    pub offset: u64,
}

impl Index {
    /// The last segment starting at or before `seconds`.
    pub fn segment_at(&self, seconds: f64) -> Segment {
        let i = self.segments.partition_point(|s| s.start <= seconds).saturating_sub(1);
        self.segments[i]
    }
}

/// Parses the index from the first bytes of a file; `None` if they hold no usable one.
pub fn parse_index(head: &[u8]) -> Option<Index> {
    let mut pos = 0usize;
    let mut seen_moov = false;
    while pos + 8 <= head.len() {
        let size32 = be_u32(head, pos)?;
        let kind = &head[pos + 4..pos + 8];
        let (size, header) = match size32 {
            1 => (usize::try_from(be_u64(head, pos + 8)?).ok()?, 16),
            0 => (head.len() - pos, 8),
            n => (n as usize, 8),
        };
        if size < header {
            return None;
        }
        match kind {
            b"moov" => seen_moov = true,
            b"sidx" if seen_moov => {
                let body = head.get(pos + header..pos + size)?;
                let segments = parse_sidx(body, (pos + size) as u64)?;
                return (!segments.is_empty()).then_some(Index { init_len: pos, segments });
            }
            b"moof" | b"mdat" => return None,
            _ => {}
        }
        pos = pos.checked_add(size)?;
    }
    None
}

/// `anchor` is the byte offset just past the `sidx` box, which its offsets count from.
fn parse_sidx(body: &[u8], anchor: u64) -> Option<Vec<Segment>> {
    let version = *body.first()?;
    let timescale = f64::from(be_u32(body, 8)?);
    let (mut time, first_offset, mut pos) = if version == 0 {
        (u64::from(be_u32(body, 12)?), u64::from(be_u32(body, 16)?), 20)
    } else {
        (be_u64(body, 12)?, be_u64(body, 20)?, 28)
    };
    if timescale == 0.0 {
        return None;
    }
    let count = be_u16(body, pos + 2)?;
    pos += 4;

    let mut offset = anchor + first_offset;
    let mut segments = Vec::with_capacity(count.into());
    for _ in 0..count {
        let reference = be_u32(body, pos)?;
        // A reference to another index rather than to media: not something YouTube serves.
        if reference >> 31 == 1 {
            return None;
        }
        segments.push(Segment { start: time as f64 / timescale, offset });
        offset += u64::from(reference & 0x7fff_ffff);
        time += u64::from(be_u32(body, pos + 4)?);
        pos += 12;
    }
    Some(segments)
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r#box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    /// A version-0 `sidx` with timescale 1000 and the given (size, duration) references.
    fn sidx(first_offset: u32, refs: &[(u32, u32)]) -> Vec<u8> {
        let mut body = vec![0, 0, 0, 0];
        body.extend_from_slice(&1u32.to_be_bytes()); // reference id
        body.extend_from_slice(&1000u32.to_be_bytes()); // timescale
        body.extend_from_slice(&0u32.to_be_bytes()); // earliest presentation time
        body.extend_from_slice(&first_offset.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(refs.len() as u16).to_be_bytes());
        for &(size, duration) in refs {
            body.extend_from_slice(&size.to_be_bytes());
            body.extend_from_slice(&duration.to_be_bytes());
            body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        r#box(b"sidx", &body)
    }

    #[test]
    fn reads_segments_after_the_init_segment() {
        let mut file = r#box(b"ftyp", b"dash");
        file.extend(r#box(b"moov", &[0; 20]));
        let init_len = file.len();
        let index = sidx(0, &[(100, 10_000), (200, 9_500), (50, 2_000)]);
        let anchor = (init_len + index.len()) as u64;
        file.extend(index);

        let parsed = parse_index(&file).expect("index");
        assert_eq!(parsed.init_len, init_len);
        assert_eq!(
            parsed.segments,
            vec![
                Segment { start: 0.0, offset: anchor },
                Segment { start: 10.0, offset: anchor + 100 },
                Segment { start: 19.5, offset: anchor + 300 },
            ]
        );
        assert_eq!(parsed.segment_at(0.0).start, 0.0);
        assert_eq!(parsed.segment_at(15.0).start, 10.0);
        assert_eq!(parsed.segment_at(500.0).start, 19.5);
    }

    #[test]
    fn honours_the_first_offset() {
        let mut file = r#box(b"moov", &[]);
        let index = sidx(64, &[(10, 1_000)]);
        let anchor = (file.len() + index.len()) as u64;
        file.extend(index);
        assert_eq!(parse_index(&file).unwrap().segments[0].offset, anchor + 64);
    }

    #[test]
    fn refuses_files_without_an_index() {
        let mut file = r#box(b"ftyp", b"dash");
        file.extend(r#box(b"moov", &[]));
        file.extend(r#box(b"mdat", &[0; 16]));
        assert!(parse_index(&file).is_none());
        assert!(parse_index(b"garbage").is_none());
        assert!(parse_index(&sidx(0, &[(1, 1)])).is_none(), "an index before moov is unusable");
    }
}
