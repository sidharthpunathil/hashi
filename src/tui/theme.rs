//! Palette, gradients and the procedural bridge logo.
//!
//! Colours come from the Hashi logo: a violet-to-pink sunset behind a deep
//! indigo bridge, with a single purple sparkle.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agent::AgentKind;

/// An sRGB colour that can be interpolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Linear interpolation, `t` in `0.0..=1.0`.
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| {
            let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
            // Clamped to 0..=255 above, so the cast cannot truncate.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "value clamped to u8 range"
            )]
            let out = v.round().clamp(0.0, 255.0) as u8;
            out
        };
        Self(
            mix(self.0, other.0),
            mix(self.1, other.1),
            mix(self.2, other.2),
        )
    }

    /// Brightens toward white by `t`.
    #[must_use]
    pub fn lighten(self, t: f32) -> Self {
        self.lerp(Self(255, 255, 255), t)
    }
}

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Self {
        Self::Rgb(c.0, c.1, c.2)
    }
}

/// Deep indigo of the bridge silhouette.
pub const INK: Rgb = Rgb(42, 20, 99);
/// Indigo lifted enough to read on dark terminals.
pub const INK_LIGHT: Rgb = Rgb(92, 62, 196);
/// Top of the sunset.
pub const VIOLET: Rgb = Rgb(124, 77, 255);
/// Wordmark purple.
pub const PURPLE: Rgb = Rgb(168, 85, 247);
/// Mid sunset.
pub const ORCHID: Rgb = Rgb(212, 140, 250);
/// Bottom of the sunset.
pub const PINK: Rgb = Rgb(249, 180, 246);
/// The sparkle.
pub const SPARK: Rgb = Rgb(150, 40, 245);
/// Secondary text.
pub const MIST: Rgb = Rgb(139, 133, 168);
/// Borders and rules.
pub const DUSK: Rgb = Rgb(76, 66, 112);
/// Success.
pub const MINT: Rgb = Rgb(94, 234, 212);
/// Failure.
pub const ROSE: Rgb = Rgb(251, 113, 133);
/// Attention.
pub const AMBER: Rgb = Rgb(251, 191, 36);

/// Accent colour for an agent.
#[must_use]
pub const fn agent_color(agent: AgentKind) -> Rgb {
    match agent {
        AgentKind::Claude => PINK,
        AgentKind::Codex => Rgb(140, 170, 255),
        AgentKind::Gemini => Rgb(110, 200, 255),
        AgentKind::Opencode => Rgb(255, 196, 120),
        AgentKind::Qwen => Rgb(180, 150, 255),
        AgentKind::Cursor => Rgb(200, 204, 214),
    }
}

/// Glyph for an agent.
#[must_use]
pub const fn agent_glyph(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "✻",
        AgentKind::Codex => "◈",
        AgentKind::Gemini => "✧",
        AgentKind::Opencode => "▣",
        AgentKind::Qwen => "◉",
        AgentKind::Cursor => "▲",
    }
}

/// Foreground-only style.
#[must_use]
pub fn fg(c: Rgb) -> Style {
    Style::new().fg(c.into())
}

/// Bold foreground style.
#[must_use]
pub fn bold(c: Rgb) -> Style {
    fg(c).add_modifier(Modifier::BOLD)
}

/// Dimmed secondary text.
#[must_use]
pub fn muted() -> Style {
    fg(MIST)
}

/// Border style.
#[must_use]
pub fn border() -> Style {
    fg(DUSK)
}

/// Colours each character of `text` along a horizontal gradient.
#[must_use]
pub fn gradient(text: &str, from: Rgb, to: Rgb, modifier: Modifier) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let last = chars.len().saturating_sub(1).max(1);
    chars
        .iter()
        .enumerate()
        .map(|(i, ch)| {
            #[expect(clippy::cast_precision_loss, reason = "short UI strings")]
            let t = i as f32 / last as f32;
            Span::styled(ch.to_string(), fg(from.lerp(to, t)).add_modifier(modifier))
        })
        .collect()
}

/// A highlight that sweeps across `text`. `phase` runs `0.0..1.0`.
#[must_use]
pub fn shimmer(text: &str, base: Rgb, phase: f32) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    #[expect(clippy::cast_precision_loss, reason = "short UI strings")]
    let len = chars.len() as f32;
    // Sweep from before the first char to after the last so it enters and exits.
    let center = -3.0 + phase.rem_euclid(1.0) * (len + 6.0);
    chars
        .iter()
        .enumerate()
        .map(|(i, ch)| {
            #[expect(clippy::cast_precision_loss, reason = "short UI strings")]
            let d = (i as f32 - center).abs();
            let glow = (1.0 - d / 3.0).max(0.0);
            Span::styled(ch.to_string(), bold(base.lighten(glow * 0.85)))
        })
        .collect()
}

/// Braille spinner frame for tick `n`.
#[must_use]
pub fn spinner(n: u64) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[usize::try_from(n % 10).unwrap_or(0)]
}

/// Twinkling sparkle frame for tick `n`.
#[must_use]
pub fn sparkle(n: u64) -> (&'static str, Rgb) {
    match (n / 4) % 8 {
        0 => ("✦", SPARK.lighten(0.35)),
        1 => ("✦", SPARK),
        2 | 6 => ("✧", SPARK),
        3..=5 => ("✦", SPARK),
        _ => ("·", SPARK),
    }
}

// ── logo ───────────────────────────────────────────────────────────────────

/// Logo width in terminal cells.
pub const LOGO_W: usize = 30;
/// Logo height in pixels (two per terminal row).
pub const LOGO_PX_H: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Px {
    Empty,
    Sun(f32),
    Bridge,
}

#[expect(clippy::cast_precision_loss, reason = "small pixel grid")]
fn pixel(x: usize, y: usize) -> Px {
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    let cx = LOGO_W as f32 / 2.0;
    let h = LOGO_PX_H as f32;

    // Deck: a gentle arch, highest in the middle, with ramps down to the ground.
    let rel = (fx - cx) / cx;
    let deck_top = 10.0 + rel * rel * 2.6;
    let ramp_top = 14.5 - (cx - (fx - cx).abs());
    let top = deck_top.max(ramp_top);

    // Three round arches cut from the underside.
    let arch = [cx - 6.5, cx, cx + 6.5].iter().any(|ax| {
        let dx = (fx - ax) / 2.2;
        let dy = (fy - (h + 0.3)) / 3.2;
        dx * dx + dy * dy <= 1.0
    });

    // Pillars flank the sun.
    let pillar_x = (fx - (cx - 10.0)).abs() <= 1.0 || (fx - (cx + 10.0)).abs() <= 1.0;
    let pillar = pillar_x && fy >= 5.5 && fy <= top + 1.0;

    if pillar || (fy >= top && fy < h && !arch) {
        return Px::Bridge;
    }

    // Sun: a dome resting on the deck.
    let (dx, dy) = (fx - cx, fy - 10.5);
    if dx * dx + dy * dy <= 8.2 * 8.2 && fy < 10.5 {
        return Px::Sun(((fy - 2.0) / 8.0).clamp(0.0, 1.0));
    }
    Px::Empty
}

fn px_color(p: Px) -> Option<Rgb> {
    match p {
        Px::Empty => None,
        Px::Bridge => Some(INK_LIGHT),
        Px::Sun(t) => Some(if t < 0.5 {
            VIOLET.lerp(ORCHID, t * 2.0)
        } else {
            ORCHID.lerp(PINK, (t - 0.5) * 2.0)
        }),
    }
}

/// Renders the bridge logo as half-block lines (`LOGO_PX_H / 2` rows).
#[must_use]
pub fn logo() -> Vec<Line<'static>> {
    (0..LOGO_PX_H / 2)
        .map(|row| {
            let spans: Vec<Span<'static>> = (0..LOGO_W)
                .map(|x| {
                    let top = px_color(pixel(x, row * 2));
                    let bottom = px_color(pixel(x, row * 2 + 1));
                    match (top, bottom) {
                        (None, None) => Span::raw(" "),
                        (Some(t), None) => Span::styled("▀", fg(t)),
                        (None, Some(b)) => Span::styled("▄", fg(b)),
                        (Some(t), Some(b)) if t == b => Span::styled("█", fg(t)),
                        (Some(t), Some(b)) => {
                            Span::styled("▀", Style::new().fg(t.into()).bg(b.into()))
                        }
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// Two-row block lettering for the wordmark.
pub const WORDMARK: [&str; 2] = ["█ █ ▄▀█ █▀ █ █ █", "█▀█ █▀█ ▄█ █▀█ █"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_hits_endpoints() {
        assert_eq!(VIOLET.lerp(PINK, 0.0), VIOLET);
        assert_eq!(VIOLET.lerp(PINK, 1.0), PINK);
    }

    #[test]
    fn logo_has_expected_shape() {
        let lines = logo();
        assert_eq!(lines.len(), LOGO_PX_H / 2);
        assert!(lines.iter().all(|l| l.width() == LOGO_W));
    }

    #[test]
    fn logo_has_open_arches_at_the_bottom() {
        // The middle arch sits under the centre of the bottom row.
        assert_eq!(pixel(LOGO_W / 2, LOGO_PX_H - 1), Px::Empty);
        assert_eq!(pixel(0, LOGO_PX_H - 1), Px::Bridge);
    }

    #[test]
    fn shimmer_preserves_text() {
        let s: String = shimmer("thinking", PURPLE, 0.3)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(s, "thinking");
    }
}
