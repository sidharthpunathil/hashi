//! Invite links as QR codes, drawn with Unicode half blocks: each terminal
//! cell holds two modules, the upper and the lower one.

use qrcode::{Color, QrCode};

/// Light modules around the code. The standard asks for four; two keep it
/// small and phones still read it.
pub const QUIET: usize = 2;

/// The code for `text` as lines of ` `, `▀`, `▄` and `█`, where a drawn
/// half is a dark module. Draw them dark on light. `None` when the text is
/// too long for a QR code.
#[must_use]
pub fn lines(text: &str) -> Option<Vec<String>> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + 2 * QUIET;
    let dark = |x: usize, y: usize| {
        let (Some(x), Some(y)) = (x.checked_sub(QUIET), y.checked_sub(QUIET)) else {
            return false;
        };
        x < width && y < width && colors[y * width + x] == Color::Dark
    };
    Some(
        (0..size.div_ceil(2))
            .map(|row| {
                (0..size)
                    .map(|x| match (dark(x, 2 * row), dark(x, 2 * row + 1)) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    })
                    .collect()
            })
            .collect(),
    )
}

/// Width and height in cells of the code for `text`.
#[must_use]
pub fn size(text: &str) -> Option<(u16, u16)> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let side = code.width() + 2 * QUIET;
    Some((
        u16::try_from(side).ok()?,
        u16::try_from(side.div_ceil(2)).ok()?,
    ))
}
