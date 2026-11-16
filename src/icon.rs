//! The app's icon: a white feather across a cool red disc, drawn rather than shipped as
//! files, at whatever size is asked for.
//!
//! This file depends on nothing, so the build script can include it too and make the
//! Windows executable's icon from the same drawing.

/// The icon as RGBA pixels, `size` × `size`, smoothed by sampling each pixel 4 × 4 times.
pub fn rgba(size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Coverage of the disc and of the feather within this pixel.
            let (mut disc, mut feather) = (0.0, 0.0);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    // From -1 to 1 across the icon, y pointing up.
                    let u = ((x * SAMPLES + sx) as f32 + 0.5) / (size * SAMPLES) as f32 * 2.0 - 1.0;
                    let v = 1.0 - ((y * SAMPLES + sy) as f32 + 0.5) / (size * SAMPLES) as f32 * 2.0;
                    if u * u + v * v <= 0.96 * 0.96 {
                        disc += 1.0;
                        if in_feather(u, v) {
                            feather += 1.0;
                        }
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let (disc, feather) = (disc / n, feather / n);
            // Cool red, #D33A51, under a white feather.
            let red = [0xd3 as f32, 0x3a as f32, 0x51 as f32];
            let colour = if disc > 0.0 { red.map(|c| c + (255.0 - c) * feather / disc) } else { [0.0; 3] };
            pixels.extend_from_slice(&[
                colour[0] as u8,
                colour[1] as u8,
                colour[2] as u8,
                (disc * 255.0).round() as u8,
            ]);
        }
    }
    pixels
}

/// Whether a point of the icon lies on the feather: a slender, gently curved vane along
/// the diagonal, wider on one side as real feathers are, split by its shaft and parted
/// twice, with a short quill below.
fn in_feather(u: f32, v: f32) -> bool {
    // Along the feather (a, towards the top right) and across it (b).
    let a = (u + v) * std::f32::consts::FRAC_1_SQRT_2;
    let across = (v - u) * std::f32::consts::FRAC_1_SQRT_2;
    let (tip, base) = (0.7, -0.4);
    // The shaft bows a little, so the whole feather curves.
    let bow = 0.07 * (1.0 - ((a - 0.15) / 0.75).powi(2)).max(0.0);
    let b = across - bow;
    let shaft = 0.024;

    // The quill: a short stroke below the vane.
    if (-0.76..base).contains(&a) && b.abs() < shaft * 1.25 {
        return true;
    }
    if !(base..tip).contains(&a) {
        return false;
    }
    // The vane: widest below the middle, tapering to the tip; the leading side narrower.
    let t = (a - base) / (tip - base);
    let shape = (std::f32::consts::PI * t.powf(0.8)).sin().powf(0.7);
    let half_width = if b > 0.0 { 0.25 } else { 0.19 } * shape;
    if b.abs() > half_width {
        return false;
    }
    // The shaft shows through as a red line, stopping short of the tip.
    if b.abs() < shaft && a < tip - 0.14 {
        return false;
    }
    // Two partings where the barbs separate, slanting back towards the quill.
    let parting =
        |at: f32, side: f32| side * b > 0.0 && (a - at - 0.7 * b.abs()).abs() < 0.02 && b.abs() > half_width * 0.4;
    !(parting(0.2, 1.0) || parting(-0.02, -1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(icon: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        icon[((y * size + x) * 4) as usize..][..4].try_into().unwrap()
    }

    #[test]
    fn a_white_feather_on_a_red_disc() {
        let size = 64;
        let icon = rgba(size);
        assert_eq!(icon.len(), (size * size * 4) as usize);
        assert_eq!(pixel(&icon, size, 0, 0)[3], 0, "the corners are transparent");
        assert_eq!(pixel(&icon, size, 32, 4), [0xd3, 0x3a, 0x51, 255], "the top of the disc is cool red");
        let opaque = icon.chunks(4).filter(|p| p[3] == 255).count();
        let white = icon.chunks(4).filter(|p| *p == [255, 255, 255, 255]).count();
        let share = white as f32 / opaque as f32;
        assert!((0.08..0.3).contains(&share), "the feather is a clear part of the badge, not most of it: {share}");
    }

    #[test]
    fn every_size_draws() {
        for size in [16, 24, 32, 48, 64, 128, 256] {
            let icon = rgba(size);
            assert_eq!(icon.len(), (size * size * 4) as usize);
            assert!(icon.chunks(4).any(|p| p == [255, 255, 255, 255]), "{size}: the feather shows");
        }
    }
}
