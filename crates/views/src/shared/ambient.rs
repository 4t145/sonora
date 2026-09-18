//! The ambient background: a flowing colour field behind fullscreen, five soft
//! blobs under a heavy blur with a dark overlay so lyrics stay readable. `Root`
//! owns the one entity and paints it under everything, title bar included.
//!
//! `ambient` decides whether it is painted at all, and with it whether the
//! fullscreen controls frost what they float over. `ambient_motion` decides
//! whether the field drifts.
//!
//! The colours are the theme's `tint` and `tint_secondary`, which fullscreen
//! samples off the cover whatever the adaptive theme setting says, resolved
//! through `Theme::accent` rather than read off `Theme::selection`, which lags
//! a track change by the length of the theme's own fade. Art with no colour
//! leaves no tint and the stage stays neutral. `painted` eases the colours on
//! screen toward those, so a cover change washes in rather than cuts.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, Bounds, Context, Hsla, Render, Rgba, Window, canvas, div, px};
use state::Sonora;
use ui::{ActiveTheme as _, Theme};

/// How many blobs make up the field.
const BLOBS: usize = 5;
/// The blur shader runs a capped kernel, so giant radii melt nothing: the
/// falloff is baked into the discs below and the layer blur only has to erase
/// the small steps between them, which a true gaussian at this radius does.
const MERGE_BLUR: f32 = 24.;
/// Dark overlay keeping lyrics readable over the field.
const SHADE: f32 = 0.55;

/// One blob: base centre (fractions of the layer), diameter (fraction of the
/// layer's smaller side), drift period in seconds, phase, drift amplitude
/// (fractions).
const SPECS: [(f32, f32, f32, f32, f32, f32, f32); BLOBS] = [
    (0.22, 0.30, 1.10, 34., 0.0, 0.13, 0.10),
    (0.80, 0.24, 1.00, 27., 1.7, 0.11, 0.13),
    (0.52, 0.68, 1.20, 41., 3.4, 0.14, 0.09),
    (0.12, 0.78, 0.90, 24., 5.1, 0.10, 0.12),
    (0.85, 0.72, 0.72, 31., 2.5, 0.12, 0.11),
];

/// Concentric discs faking a radial falloff. Sixteen shallow steps stay
/// invisible even if the layer blur ever misses a frame; three steep ones
/// read as rings wherever the blur kernel runs thin.
const DISCS: usize = 16;
/// Opacity ramp from the outermost disc to the core.
const DISC_FAINT: f32 = 0.08;
const DISC_STRONG: f32 = 0.22;
/// Time constant of the exponential ease onto a new palette, in seconds. The
/// field lands within a few percent of the target after about three of these.
const WASH: f32 = 0.8;

pub(crate) struct Ambient {
    started: Instant,
    stepped: Instant,
    bounds: Rc<Cell<Bounds<gpui::Pixels>>>,
    painted: Option<[Hsla; BLOBS]>,
}

impl Ambient {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let settings = Sonora::global(cx).settings.clone();
        // Turning the drift back on has no frame of its own to land in: with
        // motion off nothing asks for one.
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let now = Instant::now();
        Self {
            started: now,
            stepped: now,
            bounds: Rc::new(Cell::new(Bounds::default())),
            painted: None,
        }
    }

    /// Eases the painted colours one frame toward `target` and returns them.
    /// The step is the real time since the last frame, so a gap long enough to
    /// mean the field was off screen lands on the target outright and entering
    /// fullscreen never washes in from whatever played before. With motion off
    /// there are no frames to ease over, so the target is taken as it is.
    fn wash(&mut self, target: [Hsla; BLOBS], animates: bool) -> [Hsla; BLOBS] {
        let now = Instant::now();
        let step = now.duration_since(self.stepped).as_secs_f32();
        self.stepped = now;

        let painted = match self.painted {
            Some(painted) if animates => {
                let delta = 1. - (-step / WASH).exp();
                std::array::from_fn(|index| blend(painted[index], target[index], delta))
            }
            _ => target,
        };
        self.painted = Some(painted);
        painted
    }

    /// Resolves the five blob colours from the theme: the cover's leading hue
    /// carried from a light highlight down to a dark shadow, plus its runner-up
    /// where the art names one. Art with no colour leaves no tints behind, and
    /// then the stage is quiet neutrals.
    fn colors(theme: &Theme) -> [Hsla; BLOBS] {
        let neutral = |light: f32| Hsla {
            h: 0.06,
            s: 0.12,
            l: light,
            a: 1.,
        };
        // A dark theme gets a dark stage; lift the floor a little so the
        // blobs stay apart from the base.
        let floor = (0.13 + theme.background.l * 0.8).clamp(0.13, 0.30);

        let Some(tint) = theme.tint else {
            return [
                neutral(floor),
                neutral(floor + 0.025),
                neutral(floor + 0.05),
                neutral(floor + 0.015),
                neutral(floor + 0.04),
            ];
        };
        let accent = theme.accent(tint);
        let base = Hsla {
            h: accent.h,
            s: (accent.s * 0.9 + 0.04).clamp(0.45, 0.75),
            l: (0.40 + (accent.l - 0.45) * 0.5).clamp(0.28, 0.52),
            a: 1.,
        };
        let second = match theme.tint_secondary {
            Some(second) => Hsla {
                h: second.h,
                s: (second.s * 0.9).clamp(0.4, 0.72),
                l: (0.38 + (second.l - 0.45) * 0.5).clamp(0.28, 0.5),
                a: 1.,
            },
            None => neutral(floor + 0.03),
        };
        [
            base,
            Hsla {
                l: (base.l + 0.14).clamp(0.3, 0.62),
                s: (base.s - 0.08).clamp(0.4, 0.75),
                ..base
            },
            second,
            Hsla {
                l: (base.l - 0.13).clamp(0.18, 0.45),
                s: (base.s + 0.04).clamp(0.45, 0.8),
                ..base
            },
            Hsla {
                h: (base.h + 0.97).rem_euclid(1.),
                l: (base.l - 0.16).clamp(0.16, 0.4),
                s: (base.s - 0.02).clamp(0.4, 0.78),
                ..base
            },
        ]
    }
}

impl Render for Ambient {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        // Without motion the field is a still gradient: no frame requests, so
        // fullscreen stops redrawing once settled. Either the system preference
        // or the setting of its own is enough to stop it.
        let animates =
            ui::motion::animates(cx) && Sonora::global(cx).settings.read(cx).ambient_motion();
        let elapsed = match animates {
            true => {
                window.request_animation_frame();
                self.started.elapsed().as_secs_f32()
            }
            false => 0.,
        };
        let colors = self.wash(Self::colors(&theme), animates);

        div()
            .id("ambient")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .child(
                canvas(
                    {
                        let bounds = self.bounds.clone();
                        move |got, _, _| bounds.set(got)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(div().absolute().inset_0().bg(theme.background))
            .child(div().absolute().inset_0().blur(px(MERGE_BLUR)).children(
                SPECS.iter().enumerate().map(|(index, spec)| {
                    let (base_x, base_y, size, period, phase, amp_x, amp_y) = *spec;
                    let spin = TAU * elapsed / period;
                    let x = base_x + amp_x * (spin + phase).sin();
                    let y = base_y + amp_y * (spin * 0.83 + phase * 1.7).cos();
                    // Geometry resolves against the layer's own pixel bounds so
                    // the discs stay circular at any window aspect ratio.
                    let bounds = self.bounds.get();
                    let wide = bounds.size.width.as_f32().max(1.);
                    let high = bounds.size.height.as_f32().max(1.);
                    let grown =
                        wide.min(high) * size * (1. + 0.12 * (spin * 0.6 + phase * 2.3).sin());
                    let color = colors[index];
                    div()
                        .absolute()
                        .left(px(x * wide - grown / 2.))
                        .top(px(y * high - grown / 2.))
                        .size(px(grown))
                        .children((0..DISCS).map(move |step| {
                            let fraction =
                                1. - step as f32 / DISCS as f32 * (1. - 1. / DISCS as f32);
                            let opacity = DISC_FAINT
                                + step as f32 / (DISCS as f32 - 1.) * (DISC_STRONG - DISC_FAINT);
                            let stepped = grown * fraction;
                            div()
                                .absolute()
                                .left(px((grown - stepped) / 2.))
                                .top(px((grown - stepped) / 2.))
                                .size(px(stepped))
                                .rounded_full()
                                .bg(color.opacity(opacity))
                        }))
                }),
            ))
            .child(div().absolute().inset_0().bg(gpui::black().opacity(SHADE)))
    }
}

/// Whether the ambient background is on. Fullscreen reads it for the frosted
/// glass on its controls as well, which only has the field to blur.
pub(crate) fn shown(cx: &App) -> bool {
    Sonora::global(cx).settings.read(cx).ambient()
}

/// Straight-line blend between two colours through RGB, so a wash between two
/// unrelated hues passes through grey rather than through every hue between
/// them.
fn blend(from: Hsla, to: Hsla, delta: f32) -> Hsla {
    let (from, to) = (Rgba::from(from), Rgba::from(to));
    let channel = |from: f32, to: f32| from + (to - from) * delta;

    Hsla::from(Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: channel(from.a, to.a),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tinted(hue: f32) -> Theme {
        let mut theme = Theme::dark();
        theme.tint = Some(Hsla {
            h: hue,
            s: 0.6,
            l: 0.4,
            a: 1.,
        });
        theme.selection = Hsla {
            h: hue,
            s: 0.7,
            l: 0.44,
            a: 1.,
        };
        theme.tint_secondary = Some(Hsla {
            h: (hue + 0.5).rem_euclid(1.),
            s: 0.6,
            l: 0.4,
            a: 1.,
        });
        theme
    }

    #[test]
    fn accent_theme_yields_its_family() {
        let colors = Ambient::colors(&tinted(0.78));

        for (index, color) in colors.iter().enumerate() {
            // The runner-up sits in slot two; everything else is family.
            if index == 2 {
                let gap = (color.h - 0.28).abs().min(1. - (color.h - 0.28).abs());
                assert!(gap < 0.06, "second hue was {}", color.h);
                continue;
            }
            let gap = (color.h - 0.78).abs().min(1. - (color.h - 0.78).abs());
            assert!(gap < 0.06, "hue was {}", color.h);
        }
        assert!(colors[1].l > colors[0].l, "highlight lifts");
        assert!(colors[3].l < colors[0].l, "shadow sinks");
    }

    #[test]
    fn untinted_theme_yields_neutrals() {
        let colors = Ambient::colors(&Theme::dark());

        assert!(colors.iter().all(|color| color.s < 0.2));
    }
}
