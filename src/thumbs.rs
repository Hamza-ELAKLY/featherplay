//! Video thumbnails. YouTube serves a small JPEG for every video at a fixed address;
//! it is fetched, decoded and shrunk to the size it is shown at, so a thumbnail
//! costs a few kilobytes of memory however large the picture YouTube sent.

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The picture's representative colour, for tinting around it.
    pub colour: [u8; 3],
}

/// The video id in a YouTube link: `watch?v=`, `youtu.be/`, `/shorts/`, `/live/` or `/embed/`.
pub fn video_id(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let query = url.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or(q)).unwrap_or("");
    let from_query = query.split('&').find_map(|pair| pair.strip_prefix("v="));
    let from_path = ["youtu.be/", "/shorts/", "/live/", "/embed/"]
        .iter()
        .find_map(|marker| path.split_once(marker).map(|(_, rest)| rest.split('/').next().unwrap_or(rest)));
    let id = from_query.or(from_path)?;
    // Ids are 11 characters of [A-Za-z0-9_-]; anything else is not one.
    (id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then(|| id.to_string())
}

/// Fetches a video's 16:9 thumbnail and shrinks it to `width` pixels across.
pub fn fetch(id: &str, width: u32) -> Result<Thumbnail, String> {
    let jpeg = crate::net::agent()
        .get(&format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"))
        .call()
        .and_then(|mut response| response.body_mut().with_config().limit(1 << 20).read_to_vec())
        .map_err(|e| e.to_string())?;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&jpeg), options);
    let rgba = decoder.decode().map_err(|e| format!("{e:?}"))?;
    let (w, h) = decoder.dimensions().ok_or("no image size")?;
    let height = (width * 9).div_ceil(16);
    let rgba = shrink(&rgba, w as u32, h as u32, width, height);
    let colour = representative_colour(&rgba);
    Ok(Thumbnail { width, height, rgba, colour })
}

/// Fetches a square picture (a channel's) from Google and shrinks it to `size` pixels.
pub fn fetch_picture(url: &str, size: u32) -> Result<Thumbnail, String> {
    let host = url.strip_prefix("https://").and_then(|rest| rest.split('/').next()).unwrap_or("");
    if !(host.ends_with(".ggpht.com") || host.ends_with(".googleusercontent.com")) {
        return Err(format!("not a Google picture: {host}"));
    }
    let jpeg = crate::net::agent()
        .get(url)
        .call()
        .and_then(|mut response| response.body_mut().with_config().limit(1 << 20).read_to_vec())
        .map_err(|e| e.to_string())?;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&jpeg), options);
    let rgba = decoder.decode().map_err(|e| format!("{e:?}"))?;
    let (w, h) = decoder.dimensions().ok_or("no image size")?;
    let rgba = shrink(&rgba, w as u32, h as u32, size, size);
    let colour = representative_colour(&rgba);
    Ok(Thumbnail { width: size, height: size, rgba, colour })
}

/// The average colour, leaning on the colourful pixels: a plain average of most pictures
/// is a muddy grey, while the eye remembers the reds and blues.
fn representative_colour(rgba: &[u8]) -> [u8; 3] {
    let mut sum = [0.0f64; 3];
    let mut total = 0.0;
    for pixel in rgba.as_chunks::<4>().0 {
        let (max, min) = (pixel[..3].iter().max().copied().unwrap_or(0), pixel[..3].iter().min().copied().unwrap_or(0));
        let weight = 0.1 + f64::from(max - min) / 255.0;
        for (s, &channel) in sum.iter_mut().zip(pixel) {
            *s += f64::from(channel) * weight;
        }
        total += weight;
    }
    if total == 0.0 {
        return [0, 0, 0];
    }
    sum.map(|s| (s / total).round() as u8)
}

/// Scales an RGBA image down by averaging each target pixel's area of the source,
/// which keeps small thumbnails smooth where picking single pixels would shimmer.
fn shrink(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1).min(sh));
        for x in 0..dw {
            let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1).min(sw));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let at = ((sy * sw + sx) * 4) as usize;
                    for (total, &channel) in sum.iter_mut().zip(&src[at..at + 4]) {
                        *total += u32::from(channel);
                    }
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            out.extend(sum.map(|total| (total / count) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_video_id_in_every_kind_of_link() {
        let id = Some("dQw4w9WgXcQ".to_string());
        assert_eq!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), id);
        assert_eq!(video_id("https://www.youtube.com/watch?list=RD1&v=dQw4w9WgXcQ&t=5#x"), id);
        assert_eq!(video_id("https://youtu.be/dQw4w9WgXcQ?si=abc"), id);
        assert_eq!(video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ"), id);
        assert_eq!(video_id("https://www.youtube.com/embed/dQw4w9WgXcQ/"), id);
        assert_eq!(video_id("https://www.youtube.com/playlist?list=PL123"), None);
        assert_eq!(video_id("https://www.youtube.com/watch?v=short"), None);
    }

    #[test]
    fn shrinking_averages_each_area() {
        // 4x2: a black half and a white half, shrunk to 2x1.
        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        let src: Vec<u8> = [black, black, white, white, black, black, white, white].concat();
        assert_eq!(shrink(&src, 4, 2, 2, 1), [black, white].concat());
        // To 1x1, the average of both.
        assert_eq!(shrink(&src, 4, 2, 1, 1), [127, 127, 127, 255]);
    }

    #[test]
    fn the_representative_colour_leans_on_colourful_pixels() {
        let grey = [128, 128, 128, 255];
        let red = [220, 30, 40, 255];
        // Three greys and one red: a plain average would be greyish pink.
        let picture: Vec<u8> = [grey, grey, grey, red].concat();
        let [r, g, b] = representative_colour(&picture);
        assert!(r > 180 && g < 70 && b < 80, "the red wins: {r},{g},{b}");
        assert_eq!(representative_colour(&[grey, grey].concat()), [128, 128, 128], "an all-grey picture stays grey");
        assert_eq!(representative_colour(&[]), [0, 0, 0]);
    }

    #[test]
    fn shrinking_to_the_same_size_changes_nothing() {
        let src: Vec<u8> = (0..36).map(|n| n as u8).collect();
        assert_eq!(shrink(&src, 3, 3, 3, 3), src);
    }
}
