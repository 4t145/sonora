use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, EntityId, Window};
use music::Spectrum;
use ui::Levels;

const EASE: f32 = 0.35;

#[derive(Default)]
struct State {
    shown: Levels,
    armed: bool,
    visible: bool,
}

#[derive(Clone, Default)]
pub struct VisualizerDrive {
    state: Rc<RefCell<State>>,
}

impl VisualizerDrive {
    pub fn levels(&self) -> Levels {
        self.state.borrow().shown.clone()
    }

    pub fn show(&self, watch: EntityId, spectrum: Spectrum, window: &mut Window) {
        let mut state = self.state.borrow_mut();
        state.visible = true;
        if state.armed {
            return;
        }
        state.armed = true;
        drop(state);

        let drive = self.clone();
        window.on_next_frame(move |window, cx| drive.step(watch, spectrum, window, cx));
    }

    pub fn hide(&self) {
        self.state.borrow_mut().visible = false;
    }

    fn step(&self, watch: EntityId, spectrum: Spectrum, window: &mut Window, cx: &mut App) {
        {
            let mut state = self.state.borrow_mut();
            if !state.visible {
                state.armed = false;
                return;
            }

            ease(&mut state.shown.left, spectrum.left());
            ease(&mut state.shown.right, spectrum.right());
        }

        cx.notify(watch);
        let drive = self.clone();
        window.on_next_frame(move |window, cx| drive.step(watch, spectrum, window, cx));
    }
}

/// Walks `shown` a fraction of the way towards `target`, adopting it outright when the band
/// count changes.
fn ease(shown: &mut Vec<f32>, target: Vec<f32>) {
    if shown.len() != target.len() {
        *shown = target;
        return;
    }
    for (shown, target) in shown.iter_mut().zip(&target) {
        *shown += (target - *shown) * EASE;
    }
}
