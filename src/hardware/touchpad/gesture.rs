//! Pure Precision Touchpad edge-slider gesture state machine.
//!
//! Tracks single-finger vertical slides initiated within the left or right edge strip
//! (normalized coordinates `x, y in 0.0..=1.0`, top=0.0, bottom=1.0).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchContact {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeSlideDelta {
    pub side: EdgeSide,
    /// Positive = swiped up (increase), negative = swiped down (decrease).
    pub steps: i32,
}

#[derive(Debug, Clone, Copy)]
struct ActiveSlide {
    contact_id: u32,
    side: EdgeSide,
    last_y: f32,
    accum_y: f32,
}

pub struct EdgeGestureTracker {
    strip_width_frac: f32,
    step_frac: f32,
    active: Option<ActiveSlide>,
    locked_out_until_release: bool,
}

impl EdgeGestureTracker {
    /// `strip_width_frac`: edge strip width as fraction of touchpad width (e.g. `12mm / 139mm ≈ 0.09`).
    /// `step_frac`: vertical fraction required to emit one step (e.g. `0.028` ≈ 35 steps per full height).
    pub fn new(strip_width_frac: f32, step_frac: f32) -> Self {
        Self {
            strip_width_frac: strip_width_frac.clamp(0.03, 0.25),
            step_frac: step_frac.clamp(0.01, 0.20),
            active: None,
            locked_out_until_release: false,
        }
    }

    pub fn reconfigure(&mut self, strip_width_frac: f32, step_frac: f32) {
        self.strip_width_frac = strip_width_frac.clamp(0.03, 0.25);
        self.step_frac = step_frac.clamp(0.01, 0.20);
        self.active = None;
    }

    /// Process a raw contact frame. Returns `Some(EdgeSlideDelta)` when enough vertical movement
    /// accumulates for one or more discrete brightness/volume steps.
    pub fn update(&mut self, contacts: &[TouchContact]) -> Option<EdgeSlideDelta> {
        if contacts.is_empty() {
            self.active = None;
            self.locked_out_until_release = false;
            return None;
        }

        // Multi-finger touch (scrolling/pinch) cancels edge sliding until all fingers lift.
        if contacts.len() != 1 {
            self.active = None;
            self.locked_out_until_release = true;
            return None;
        }

        if self.locked_out_until_release {
            return None;
        }

        let c = contacts[0];
        let x = c.x.clamp(0.0, 1.0);
        let y = c.y.clamp(0.0, 1.0);

        match &mut self.active {
            None => {
                let side = if x <= self.strip_width_frac {
                    Some(EdgeSide::Left)
                } else if x >= 1.0 - self.strip_width_frac {
                    Some(EdgeSide::Right)
                } else {
                    None
                };

                if let Some(side) = side {
                    self.active = Some(ActiveSlide {
                        contact_id: c.id,
                        side,
                        last_y: y,
                        accum_y: 0.0,
                    });
                } else {
                    // Finger started in the middle — don't accidentally trigger edge slide if it drifts.
                    self.locked_out_until_release = true;
                }
                None
            }
            Some(slide) => {
                if slide.contact_id != c.id {
                    self.active = None;
                    self.locked_out_until_release = true;
                    return None;
                }

                // Swipe UP decreases `y`, so `delta = last_y - y` is positive when moving up.
                let dy = slide.last_y - y;
                slide.last_y = y;
                slide.accum_y += dy;

                let steps = (slide.accum_y / self.step_frac).trunc() as i32;
                if steps != 0 {
                    slide.accum_y -= (steps as f32) * self.step_frac;
                    Some(EdgeSlideDelta {
                        side: slide.side,
                        steps,
                    })
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_left_and_right_vertical_slides() {
        let mut t = EdgeGestureTracker::new(0.10, 0.05);

        // Start on left edge at y=0.50
        assert_eq!(t.update(&[TouchContact { id: 1, x: 0.04, y: 0.50 }]), None);
        // Move up by 0.12 -> +2 steps
        assert_eq!(
            t.update(&[TouchContact { id: 1, x: 0.05, y: 0.38 }]),
            Some(EdgeSlideDelta {
                side: EdgeSide::Left,
                steps: 2
            })
        );
        // Release
        assert_eq!(t.update(&[]), None);

        // Start on right edge and swipe down
        assert_eq!(t.update(&[TouchContact { id: 2, x: 0.95, y: 0.20 }]), None);
        assert_eq!(
            t.update(&[TouchContact { id: 2, x: 0.94, y: 0.36 }]),
            Some(EdgeSlideDelta {
                side: EdgeSide::Right,
                steps: -3
            })
        );
    }

    #[test]
    fn ignores_center_drift_and_multifinger_scroll() {
        let mut t = EdgeGestureTracker::new(0.10, 0.05);

        // Finger starts in center then drifts into left strip -> must not trigger
        assert_eq!(t.update(&[TouchContact { id: 1, x: 0.50, y: 0.50 }]), None);
        assert_eq!(t.update(&[TouchContact { id: 1, x: 0.02, y: 0.20 }]), None);
        assert_eq!(t.update(&[]), None);

        // Second finger drops mid-slide -> cancels until full release
        assert_eq!(t.update(&[TouchContact { id: 1, x: 0.02, y: 0.60 }]), None);
        assert_eq!(
            t.update(&[
                TouchContact { id: 1, x: 0.02, y: 0.50 },
                TouchContact { id: 2, x: 0.40, y: 0.50 },
            ]),
            None
        );
        assert_eq!(t.update(&[TouchContact { id: 1, x: 0.02, y: 0.30 }]), None);
    }
}
