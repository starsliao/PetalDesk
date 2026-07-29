//! Native phase-correlation fallback for long-screenshot displacement hints.
//!
//! The result is only a coarse candidate. Callers must verify it against the
//! original pixels before accepting a stitch.

use rustfft::{num_complex::Complex, FftPlanner};
use std::cell::RefCell;

const MAX_DOWNSAMPLED_DIMENSION: usize = 512;
const PEAK_EXCLUSION_RADIUS: usize = 5;

thread_local! {
    static FFT_PLANNER: RefCell<FftPlanner<f32>> = RefCell::new(FftPlanner::new());
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseOffset {
    pub dx: i32,
    pub dy: i32,
    pub factor: u32,
    pub psr: f32,
}

pub fn phase_offset_rgba(
    previous: &[u8],
    current: &[u8],
    width: usize,
    height: usize,
) -> Option<PhaseOffset> {
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if width < 24 || height < 24 || previous.len() != expected || current.len() != expected {
        return None;
    }

    // Fixed title bars, status bars and window edges commonly dominate a full
    // screen correlation. The central body is a better translation signal.
    let margin_x = width / 10;
    let margin_y = height / 8;
    let crop_width = width.checked_sub(margin_x * 2)?;
    let crop_height = height.checked_sub(margin_y * 2)?;
    if crop_width < 16 || crop_height < 16 {
        return None;
    }
    let factor = downsample_factor(crop_width.max(crop_height));
    let (mut left, sampled_width, sampled_height) = downsample_luma(
        previous,
        width,
        margin_x,
        margin_y,
        crop_width,
        crop_height,
        factor,
    )?;
    let (mut right, right_width, right_height) = downsample_luma(
        current,
        width,
        margin_x,
        margin_y,
        crop_width,
        crop_height,
        factor,
    )?;
    if sampled_width != right_width || sampled_height != right_height {
        return None;
    }

    normalize_and_window(&mut left, sampled_width, sampled_height);
    normalize_and_window(&mut right, sampled_width, sampled_height);
    let padded_width = sampled_width.checked_mul(2)?.next_power_of_two();
    let padded_height = sampled_height.checked_mul(2)?.next_power_of_two();
    let padded_len = padded_width.checked_mul(padded_height)?;
    let mut left_frequency = vec![Complex::new(0.0_f32, 0.0_f32); padded_len];
    let mut right_frequency = vec![Complex::new(0.0_f32, 0.0_f32); padded_len];
    copy_real_to_padded(
        &left,
        sampled_width,
        sampled_height,
        &mut left_frequency,
        padded_width,
    );
    copy_real_to_padded(
        &right,
        sampled_width,
        sampled_height,
        &mut right_frequency,
        padded_width,
    );

    fft_2d(&mut left_frequency, padded_width, padded_height, false);
    fft_2d(&mut right_frequency, padded_width, padded_height, false);
    for (left, right) in left_frequency.iter_mut().zip(right_frequency) {
        let cross = left.conj() * right;
        let magnitude = cross.norm();
        *left = if magnitude > 1.0e-6 {
            cross / magnitude
        } else {
            Complex::new(0.0, 0.0)
        };
    }
    fft_2d(&mut left_frequency, padded_width, padded_height, true);

    let (peak_index, peak) = left_frequency
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.re.total_cmp(&right.re))
        .map(|(index, value)| (index, value.re))?;
    if !peak.is_finite() {
        return None;
    }
    let peak_x = peak_index % padded_width;
    let peak_y = peak_index / padded_width;
    let psr = peak_to_sidelobe_ratio(
        &left_frequency,
        padded_width,
        padded_height,
        peak_x,
        peak_y,
        peak,
    )?;
    let dx = wrapped_offset(peak_x, padded_width).checked_mul(factor as i32)?;
    let dy = wrapped_offset(peak_y, padded_height).checked_mul(factor as i32)?;
    Some(PhaseOffset {
        dx,
        dy,
        factor: factor as u32,
        psr,
    })
}

fn downsample_factor(max_dimension: usize) -> usize {
    let required = max_dimension.div_ceil(MAX_DOWNSAMPLED_DIMENSION).max(1);
    required.next_power_of_two()
}

fn downsample_luma(
    rgba: &[u8],
    source_width: usize,
    start_x: usize,
    start_y: usize,
    crop_width: usize,
    crop_height: usize,
    factor: usize,
) -> Option<(Vec<f32>, usize, usize)> {
    let width = crop_width / factor;
    let height = crop_height / factor;
    if width == 0 || height == 0 {
        return None;
    }
    let mut result = Vec::with_capacity(width.checked_mul(height)?);
    for output_y in 0..height {
        let source_y = start_y + output_y * factor;
        for output_x in 0..width {
            let source_x = start_x + output_x * factor;
            let mut sum = 0.0_f32;
            for y in source_y..source_y + factor {
                for x in source_x..source_x + factor {
                    let offset = (y * source_width + x) * 4;
                    sum += f32::from(rgba[offset]) * 0.299
                        + f32::from(rgba[offset + 1]) * 0.587
                        + f32::from(rgba[offset + 2]) * 0.114;
                }
            }
            result.push(sum / (factor * factor) as f32);
        }
    }
    Some((result, width, height))
}

fn normalize_and_window(values: &mut [f32], width: usize, height: usize) {
    let mean = values.iter().copied().sum::<f32>() / values.len().max(1) as f32;
    let width_denominator = width.saturating_sub(1).max(1) as f32;
    let height_denominator = height.saturating_sub(1).max(1) as f32;
    for y in 0..height {
        let window_y =
            0.5 * (1.0 - (2.0 * std::f32::consts::PI * y as f32 / height_denominator).cos());
        for x in 0..width {
            let window_x =
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * x as f32 / width_denominator).cos());
            values[y * width + x] = (values[y * width + x] - mean) * window_x * window_y;
        }
    }
}

fn copy_real_to_padded(
    source: &[f32],
    width: usize,
    height: usize,
    destination: &mut [Complex<f32>],
    padded_width: usize,
) {
    for y in 0..height {
        for x in 0..width {
            destination[y * padded_width + x].re = source[y * width + x];
        }
    }
}

fn fft_2d(values: &mut [Complex<f32>], width: usize, height: usize, inverse: bool) {
    let (row_fft, column_fft) = FFT_PLANNER.with(|planner| {
        let mut planner = planner.borrow_mut();
        let row = if inverse {
            planner.plan_fft_inverse(width)
        } else {
            planner.plan_fft_forward(width)
        };
        let column = if inverse {
            planner.plan_fft_inverse(height)
        } else {
            planner.plan_fft_forward(height)
        };
        (row, column)
    });

    for row in values.chunks_exact_mut(width) {
        row_fft.process(row);
    }
    let mut column = vec![Complex::new(0.0_f32, 0.0_f32); height];
    for x in 0..width {
        for y in 0..height {
            column[y] = values[y * width + x];
        }
        column_fft.process(&mut column);
        for y in 0..height {
            values[y * width + x] = column[y];
        }
    }
    if inverse {
        let scale = (width * height) as f32;
        for value in values {
            *value /= scale;
        }
    }
}

fn peak_to_sidelobe_ratio(
    correlation: &[Complex<f32>],
    width: usize,
    height: usize,
    peak_x: usize,
    peak_y: usize,
    peak: f32,
) -> Option<f32> {
    let mut sum = 0.0_f64;
    let mut square_sum = 0.0_f64;
    let mut samples = 0_u64;
    for y in 0..height {
        let distance_y = y.abs_diff(peak_y).min(height - y.abs_diff(peak_y));
        for x in 0..width {
            let distance_x = x.abs_diff(peak_x).min(width - x.abs_diff(peak_x));
            if distance_x <= PEAK_EXCLUSION_RADIUS && distance_y <= PEAK_EXCLUSION_RADIUS {
                continue;
            }
            let value = f64::from(correlation[y * width + x].re);
            sum += value;
            square_sum += value * value;
            samples += 1;
        }
    }
    if samples < 2 {
        return None;
    }
    let mean = sum / samples as f64;
    let variance = (square_sum / samples as f64 - mean * mean).max(0.0);
    let deviation = variance.sqrt();
    (deviation > 1.0e-9).then_some(((f64::from(peak) - mean) / deviation) as f32)
}

fn wrapped_offset(index: usize, length: usize) -> i32 {
    if index > length / 2 {
        index as i32 - length as i32
    } else {
        index as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterned_rgba(width: usize, height: usize) -> Vec<u8> {
        let mut pixels = vec![0_u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let offset = (y * width + x) * 4;
                pixels[offset] = ((x * 19 + y * 7 + (x * y) % 31) % 251) as u8;
                pixels[offset + 1] = ((x * 3 + y * 23 + (x + y) % 47) % 253) as u8;
                pixels[offset + 2] = ((x * 11 + y * 13 + (x * y) % 17) % 249) as u8;
                pixels[offset + 3] = 255;
            }
        }
        pixels
    }

    fn translated_rgba(source: &[u8], width: usize, height: usize, dx: i32, dy: i32) -> Vec<u8> {
        let mut output = vec![0_u8; source.len()];
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let source_x = x - dx;
                let source_y = y - dy;
                let output_offset = (y as usize * width + x as usize) * 4;
                output[output_offset + 3] = 255;
                if source_x < 0
                    || source_y < 0
                    || source_x >= width as i32
                    || source_y >= height as i32
                {
                    continue;
                }
                let source_offset = (source_y as usize * width + source_x as usize) * 4;
                output[output_offset..output_offset + 4]
                    .copy_from_slice(&source[source_offset..source_offset + 4]);
            }
        }
        output
    }

    #[test]
    fn phase_offset_finds_vertical_translation_and_sign() {
        let width = 192;
        let height = 144;
        let previous = patterned_rgba(width, height);
        let current = translated_rgba(&previous, width, height, 0, -17);
        let matched = phase_offset_rgba(&previous, &current, width, height).unwrap();
        assert_eq!(matched.dx, 0);
        assert!(
            (matched.dy + 17).abs() <= matched.factor as i32 + 1,
            "{matched:?}"
        );
        assert!(matched.psr >= 5.0, "{matched:?}");
    }

    #[test]
    fn phase_offset_rejects_uniform_images() {
        let pixels = vec![220_u8; 96 * 80 * 4];
        assert!(phase_offset_rgba(&pixels, &pixels, 96, 80).is_none());
    }

    #[test]
    fn downsample_factor_keeps_fft_inputs_bounded() {
        assert_eq!(downsample_factor(512), 1);
        assert_eq!(downsample_factor(513), 2);
        assert_eq!(downsample_factor(3_840), 8);
    }

    #[test]
    #[ignore = "release-only 4K performance probe"]
    fn phase_offset_handles_4k_without_unbounded_fft_inputs() {
        let width = 3_840;
        let height = 2_160;
        let previous = patterned_rgba(width, height);
        let current = translated_rgba(&previous, width, height, 0, -137);
        let started = std::time::Instant::now();
        let matched = phase_offset_rgba(&previous, &current, width, height).unwrap();
        eprintln!("4K phase correlation: {:?}, {matched:?}", started.elapsed());
        assert_eq!(matched.factor, 8);
        assert!(matched.dx.abs() <= matched.factor as i32 + 1, "{matched:?}");
        assert!(
            (matched.dy + 137).abs() <= matched.factor as i32 + 1,
            "{matched:?}"
        );
        assert!(matched.psr >= 5.0, "{matched:?}");
    }
}
