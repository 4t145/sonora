use gpui::{App, Context, Entity, Task};
use state::{AppSettings, Playback, Queue, Sonora};
use ui::{ActiveTheme as _, CoverPalette, Look, Theme};

/// Drives the theme's tint from the playing track's cover, and keeps the next
/// track's cover sampled ahead of time so a track change recolours on the frame
/// it happens rather than once a download and a decode have come back.
pub struct Adaptive {
    playback: Entity<Playback>,
    queue: Entity<Queue>,
    settings: Entity<AppSettings>,
    /// Whether the fullscreen shell is up. The ambient background is painted
    /// out of the tint, so fullscreen samples the cover whatever the adaptive
    /// theme setting says.
    fullscreen: bool,
    cover: Option<String>,
    ahead: Option<Ahead>,
    task: Option<Task<()>>,
}

/// The cover of the track queued after the current one, with the palette once
/// the sampling has finished. The task is held so that requeueing cancels a
/// sampling that is no longer wanted; it is never cleared from inside itself.
struct Ahead {
    cover: String,
    palette: Option<CoverPalette>,
    _task: Task<()>,
}

impl Adaptive {
    pub fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let sonora = Sonora::global(cx);
        let (queue, settings) = (sonora.queue.clone(), sonora.settings.clone());
        cx.observe(&playback, |this, _, cx| this.sync(false, cx))
            .detach();
        cx.observe(&settings, |this, _, cx| this.sync(false, cx))
            .detach();
        cx.observe(&queue, |this, _, cx| this.look_ahead(cx))
            .detach();

        let mut adaptive = Self {
            playback,
            queue,
            settings,
            fullscreen: false,
            cover: None,
            ahead: None,
            task: None,
        };
        adaptive.sync(false, cx);
        adaptive
    }

    /// Records whether fullscreen is up and resamples. Leaving fullscreen with the adaptive
    /// theme off drops the tint here, so the rest of the app never keeps a cover's colour.
    /// The swap is instant either way: the whole palette appears or disappears at once, and
    /// washing the workspace through that on the way out is a change nobody asked for.
    pub fn set_fullscreen(&mut self, fullscreen: bool, cx: &mut Context<Self>) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        self.sync(true, cx);
    }

    /// Resamples the cover and hands the palette to the theme. `instant` skips the fade, for
    /// a change that is not a track change.
    fn sync(&mut self, instant: bool, cx: &mut Context<Self>) {
        let cover = self
            .settings
            .read(cx)
            .cover_tint(self.fullscreen)
            .then(|| {
                self.playback
                    .read(cx)
                    .track()
                    .and_then(|track| track.cover.clone())
            })
            .flatten();

        if cover == self.cover {
            return;
        }
        self.cover = cover.clone();

        let Some(cover) = cover else {
            self.task = None;
            apply(CoverPalette::default(), instant, cx);
            self.look_ahead(cx);
            return;
        };

        // The track we sampled ahead is usually the one that just started, so
        // the recolour lands without a round trip.
        match self.ready(&cover) {
            Some(palette) => {
                self.task = None;
                apply(palette, instant, cx);
            }
            None => {
                let palette = ui::palette(cover, cx);
                self.task = Some(cx.spawn(async move |this, cx| {
                    let palette = palette.await;
                    this.update(cx, |_, cx| apply(palette, instant, cx)).ok();
                }));
            }
        }
        self.look_ahead(cx);
    }

    /// The palette sampled ahead for `cover`, taken out of the slot so the next
    /// track can claim it.
    fn ready(&mut self, cover: &str) -> Option<CoverPalette> {
        let palette = self
            .ahead
            .as_ref()
            .filter(|ahead| ahead.cover == cover)?
            .palette?;
        self.ahead = None;
        Some(palette)
    }

    /// Samples the cover of the track queued next, so its palette is waiting
    /// when it starts. A shuffle or a jump elsewhere in the library simply
    /// misses and falls back to sampling on the change.
    fn look_ahead(&mut self, cx: &mut Context<Self>) {
        let next = match self.settings.read(cx).cover_tint(self.fullscreen) {
            true => {
                let queue = self.queue.read(cx);
                queue
                    .upcoming()
                    .next()
                    .or_else(|| queue.similar().next())
                    .and_then(|track| track.cover.clone())
            }
            false => None,
        };

        let Some(cover) = next else {
            self.ahead = None;
            return;
        };
        if self
            .ahead
            .as_ref()
            .is_some_and(|ahead| ahead.cover == cover)
        {
            return;
        }

        let palette = ui::palette(cover.clone(), cx);
        let wanted = cover.clone();
        let task = cx.spawn(async move |this, cx| {
            let palette = palette.await;
            this.update(cx, |this, _| {
                let Some(ahead) = this.ahead.as_mut().filter(|ahead| ahead.cover == wanted) else {
                    return;
                };
                ahead.palette = Some(palette);
            })
            .ok();
        });
        self.ahead = Some(Ahead {
            cover,
            palette: None,
            _task: task,
        });
    }
}

/// Puts the palette on the theme, as a fade or as a swap on the spot.
fn apply(palette: CoverPalette, instant: bool, cx: &mut App) {
    if cx.theme().tint == palette.primary && cx.theme().tint_secondary == palette.secondary {
        return;
    }

    let settings = Sonora::global(cx).settings.clone();
    let (look, overrides) = {
        let settings = settings.read(cx);
        (
            Look {
                tint: palette.primary,
                tint_secondary: palette.secondary,
                ..settings.look()
            },
            settings.theme_overrides().clone(),
        )
    };
    match instant {
        true => Theme::set(look, &overrides, cx),
        false => Theme::fade(look, &overrides, cx),
    }
}
