//! What an extension asked the host to do with a node's look, and the arithmetic that
//! turns it into values over time. No window or rendering types, and no named effects:
//! an animation is keyframes of properties, whatever it is meant to look like.
use std::{sync::Arc, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prop {
    Rotation,
    ScaleX,
    ScaleY,
    TranslateX,
    TranslateY,
    Opacity,
}

impl Prop {
    pub const ALL: [Prop; 6] = [
        Prop::Rotation,
        Prop::ScaleX,
        Prop::ScaleY,
        Prop::TranslateX,
        Prop::TranslateY,
        Prop::Opacity,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The value that leaves the node as it is.
    pub fn rest(self) -> f32 {
        match self {
            Prop::ScaleX | Prop::ScaleY | Prop::Opacity => 1.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    Linear,
    /// The CSS cubic Bezier through (0, 0), (x1, y1), (x2, y2), (1, 1).
    Bezier(f32, f32, f32, f32),
}

impl Ease {
    /// The eased progress for a linear progress `x` in 0 to 1.
    pub fn at(self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let Ease::Bezier(x1, y1, x2, y2) = self else {
            return x;
        };
        let (cx, cy) = (3.0 * x1, 3.0 * y1);
        let (bx, by) = (3.0 * (x2 - x1) - cx, 3.0 * (y2 - y1) - cy);
        let (ax, ay) = (1.0 - cx - bx, 1.0 - cy - by);
        let curve_x = |t: f32| ((ax * t + bx) * t + cx) * t;
        let slope_x = |t: f32| (3.0 * ax * t + 2.0 * bx) * t + cx;
        // Newton from the straight guess, then bisection where the slope is too flat.
        let mut t = x;
        for _ in 0..8 {
            let error = curve_x(t) - x;
            if error.abs() < 1e-6 {
                break;
            }
            let slope = slope_x(t);
            if slope.abs() < 1e-6 {
                break;
            }
            t -= error / slope;
        }
        if (curve_x(t) - x).abs() >= 1e-4 || !(0.0..=1.0).contains(&t) {
            let (mut low, mut high) = (0.0_f32, 1.0_f32);
            t = x;
            for _ in 0..32 {
                if curve_x(t) < x {
                    low = t;
                } else {
                    high = t;
                }
                t = (low + high) / 2.0;
            }
        }
        ((ay * t + by) * t + cy) * t
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub prop: Prop,
    /// `(offset, value)` from offset 0 to 1, in order.
    pub keys: Vec<(f32, f32)>,
    pub ease: Ease,
}

impl Track {
    fn at(&self, progress: f32) -> f32 {
        let Some(last) = self.keys.last() else {
            return self.prop.rest();
        };
        let next = self
            .keys
            .iter()
            .position(|(offset, _)| *offset >= progress)
            .unwrap_or(self.keys.len() - 1);
        if next == 0 {
            return self.keys[0].1;
        }
        let ((from_offset, from), (to_offset, to)) = (self.keys[next - 1], self.keys[next]);
        if to_offset <= from_offset {
            return last.1;
        }
        let along = ((progress - from_offset) / (to_offset - from_offset)).clamp(0.0, 1.0);
        from + (to - from) * self.ease.at(along)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub tracks: Vec<Track>,
    pub duration_ms: u32,
    pub delay_ms: u32,
    /// `None` repeats until it is stopped or replaced.
    pub iterations: Option<u32>,
    pub alternate: bool,
    /// Keep the last values once it ends.
    pub hold: bool,
}

/// The values of an animation at one moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub values: [Option<f32>; 6],
    /// It will not change any more.
    pub done: bool,
}

impl Animation {
    pub fn sample(&self, elapsed_ms: f64) -> Sample {
        let none = Sample {
            values: [None; 6],
            done: false,
        };
        let time = elapsed_ms - f64::from(self.delay_ms);
        if time < 0.0 || self.duration_ms == 0 {
            return none;
        }
        let laps = time / f64::from(self.duration_ms);
        let finished = self
            .iterations
            .is_some_and(|count| laps >= f64::from(count));
        if finished && !self.hold {
            return Sample { done: true, ..none };
        }
        let (lap, progress) = if finished {
            (
                u64::from(self.iterations.unwrap_or(1).saturating_sub(1)),
                1.0,
            )
        } else {
            let lap = laps.floor();
            (lap as u64, (laps - lap) as f32)
        };
        let progress = if self.alternate && lap % 2 == 1 {
            1.0 - progress
        } else {
            progress
        };
        let mut values = [None; 6];
        for track in &self.tracks {
            values[track.prop.index()] = Some(track.at(progress));
        }
        Sample {
            values,
            done: finished,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smoothing {
    pub duration_ms: u32,
    pub ease: Ease,
}

/// Everything asked for one node. `revision` is 0 until the extension asks anything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Motion {
    pub revision: u64,
    pub resting: [Option<f32>; 6],
    pub smoothing: [Option<Smoothing>; 6],
    /// The revision it was started at, and the animation.
    pub animation: Option<(u64, Arc<Animation>)>,
    /// A delayed first render joins the host timeline instead of restarting it.
    pub started_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct Glide {
    from: f32,
    to: f32,
    start: f64,
    smoothing: Smoothing,
}

#[derive(Clone, Debug)]
struct Running {
    revision: u64,
    start: f64,
    animation: Arc<Animation>,
    done: bool,
}

/// Plays a node's `Motion`: keeps the clock, smooths changed resting values and runs
/// the animation, so the values at any moment are known without asking the extension.
#[derive(Clone, Debug)]
pub struct Player {
    clock: f64,
    revision: u64,
    target: [f32; 6],
    shown: [f32; 6],
    gliding: [Option<Glide>; 6],
    running: Option<Running>,
}

impl Default for Player {
    fn default() -> Self {
        let rest = Prop::ALL.map(Prop::rest);
        Self {
            clock: 0.0,
            revision: 0,
            target: rest,
            shown: rest,
            gliding: [None; 6],
            running: None,
        }
    }
}

impl Player {
    /// Take what the extension asked for; nothing changes unless it is a new request.
    pub fn apply(&mut self, motion: &Motion) {
        if motion.revision == self.revision {
            return;
        }
        self.revision = motion.revision;
        for prop in Prop::ALL {
            let i = prop.index();
            let want = motion.resting[i].unwrap_or(prop.rest());
            if want == self.target[i] {
                continue;
            }
            self.target[i] = want;
            match motion.smoothing[i] {
                Some(smoothing) if smoothing.duration_ms > 0 => {
                    self.gliding[i] = Some(Glide {
                        from: self.shown[i],
                        to: want,
                        start: self.clock,
                        smoothing,
                    });
                }
                _ => {
                    self.gliding[i] = None;
                    self.shown[i] = want;
                }
            }
        }
        self.running = match (&motion.animation, self.running.take()) {
            (Some((revision, _)), Some(running)) if running.revision == *revision => Some(running),
            (Some((revision, animation)), _) => Some(Running {
                revision: *revision,
                start: self.clock
                    - motion
                        .started_at
                        .map_or(0.0, |start| start.elapsed().as_secs_f64() * 1000.0),
                animation: animation.clone(),
                done: false,
            }),
            (None, _) => None,
        };
    }

    /// Move the clock on; whether anything shown changed.
    pub fn advance(&mut self, elapsed_ms: f32) -> bool {
        self.clock += f64::from(elapsed_ms.max(0.0));
        let mut changed = false;
        for i in 0..6 {
            let Some(glide) = self.gliding[i] else {
                continue;
            };
            let progress = ((self.clock - glide.start) / f64::from(glide.smoothing.duration_ms))
                .clamp(0.0, 1.0) as f32;
            self.shown[i] =
                glide.from + (glide.to - glide.from) * glide.smoothing.ease.at(progress);
            if progress >= 1.0 {
                self.shown[i] = glide.to;
                self.gliding[i] = None;
            }
            changed = true;
        }
        if let Some(running) = &mut self.running {
            let sample = running.animation.sample(self.clock - running.start);
            changed |= !running.done;
            running.done = sample.done;
        }
        changed
    }

    /// Something still moves, so frames are needed.
    pub fn busy(&self) -> bool {
        self.gliding.iter().any(Option::is_some)
            || self.running.as_ref().is_some_and(|running| !running.done)
    }

    /// The value of each property now, in `Prop::ALL` order.
    pub fn values(&self) -> [f32; 6] {
        let mut values = self.shown;
        if let Some(running) = &self.running {
            let sample = running.animation.sample(self.clock - running.start);
            for (value, animated) in values.iter_mut().zip(sample.values) {
                if let Some(animated) = animated {
                    *value = animated;
                }
            }
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-3
    }

    fn spin() -> Animation {
        Animation {
            tracks: vec![Track {
                prop: Prop::Rotation,
                keys: vec![(0.0, 0.0), (1.0, 360.0)],
                ease: Ease::Linear,
            }],
            duration_ms: 1000,
            delay_ms: 0,
            iterations: None,
            alternate: false,
            hold: false,
        }
    }

    fn rotation(sample: Sample) -> Option<f32> {
        sample.values[Prop::Rotation.index()]
    }

    #[test]
    fn a_curve_starts_and_ends_where_it_says_and_bends_between() {
        let ease = Ease::Bezier(0.42, 0.0, 0.58, 1.0);
        assert!(close(ease.at(0.0), 0.0) && close(ease.at(1.0), 1.0));
        assert!(close(ease.at(0.5), 0.5));
        assert!(ease.at(0.25) < 0.25, "ease-in-out starts slowly");
        assert!(ease.at(0.75) > 0.75, "and ends slowly");
        assert!(close(Ease::Linear.at(0.3), 0.3));
        // A curve that overshoots may leave 0 to 1 between its ends.
        assert!(Ease::Bezier(0.34, 1.56, 0.64, 1.0).at(0.6) > 1.0);
    }

    #[test]
    fn keyframes_interpolate_with_the_easing_of_each_segment() {
        let animation = Animation {
            tracks: vec![Track {
                prop: Prop::Opacity,
                keys: vec![(0.0, 1.0), (0.5, 0.2), (1.0, 1.0)],
                ease: Ease::Linear,
            }],
            duration_ms: 1000,
            ..spin()
        };
        let at = |ms| animation.sample(ms).values[Prop::Opacity.index()].unwrap();
        assert!(close(at(0.0), 1.0));
        assert!(close(at(250.0), 0.6));
        assert!(close(at(500.0), 0.2));
        assert!(close(at(750.0), 0.6));
    }

    #[test]
    fn an_endless_animation_wraps_and_an_alternating_one_runs_back() {
        let animation = spin();
        assert!(close(rotation(animation.sample(250.0)).unwrap(), 90.0));
        assert!(close(rotation(animation.sample(1250.0)).unwrap(), 90.0));
        assert!(!animation.sample(1_000_000.0).done);
        let back = Animation {
            alternate: true,
            ..spin()
        };
        assert!(close(rotation(back.sample(1250.0)).unwrap(), 270.0));
        assert!(close(rotation(back.sample(2250.0)).unwrap(), 90.0));
    }

    #[test]
    fn delay_and_a_set_number_of_iterations_end_it_or_hold_its_last_values() {
        let once = Animation {
            iterations: Some(1),
            delay_ms: 200,
            ..spin()
        };
        assert_eq!(
            rotation(once.sample(100.0)),
            None,
            "nothing before the delay"
        );
        assert!(close(rotation(once.sample(700.0)).unwrap(), 180.0));
        let after = once.sample(1300.0);
        assert!(after.done && rotation(after).is_none(), "back to resting");
        let held = Animation { hold: true, ..once };
        let after = held.sample(1300.0);
        assert!(after.done && close(rotation(after).unwrap(), 360.0));
        // Two alternating laps end where they began.
        let twice = Animation {
            iterations: Some(2),
            alternate: true,
            hold: true,
            ..spin()
        };
        assert!(close(rotation(twice.sample(5000.0)).unwrap(), 0.0));
    }

    #[test]
    fn a_player_runs_an_animation_from_the_moment_it_is_taken() {
        let mut player = Player::default();
        player.advance(5000.0); // time before the request does not count
        player.apply(&Motion {
            revision: 1,
            animation: Some((1, Arc::new(spin()))),
            ..Default::default()
        });
        assert!(player.busy());
        assert!(close(player.values()[Prop::Rotation.index()], 0.0));
        player.advance(250.0);
        assert!(close(player.values()[Prop::Rotation.index()], 90.0));
        // The same request again does not restart it.
        player.apply(&Motion {
            revision: 1,
            animation: Some((1, Arc::new(spin()))),
            ..Default::default()
        });
        player.advance(250.0);
        assert!(close(player.values()[Prop::Rotation.index()], 180.0));
        // A new request replaces it from the start; none ends it.
        player.apply(&Motion {
            revision: 2,
            animation: Some((2, Arc::new(spin()))),
            ..Default::default()
        });
        assert!(close(player.values()[Prop::Rotation.index()], 0.0));
        player.apply(&Motion {
            revision: 3,
            ..Default::default()
        });
        assert!(!player.busy());
        assert!(close(player.values()[Prop::Rotation.index()], 0.0));
    }

    #[test]
    fn a_finished_animation_stops_asking_for_frames() {
        let mut player = Player::default();
        player.apply(&Motion {
            revision: 1,
            animation: Some((
                1,
                Arc::new(Animation {
                    iterations: Some(1),
                    hold: true,
                    ..spin()
                }),
            )),
            ..Default::default()
        });
        player.advance(500.0);
        assert!(player.busy());
        player.advance(600.0);
        assert!(!player.busy());
        assert!(close(player.values()[Prop::Rotation.index()], 360.0));
        player.advance(600.0);
        assert!(
            close(player.values()[Prop::Rotation.index()], 360.0),
            "held"
        );
    }

    #[test]
    fn a_changed_resting_value_glides_when_smoothing_was_asked_for_and_jumps_otherwise() {
        let mut player = Player::default();
        let mut motion = Motion {
            revision: 1,
            ..Default::default()
        };
        motion.resting[Prop::TranslateX.index()] = Some(10.0);
        player.apply(&motion);
        assert!(
            close(player.values()[Prop::TranslateX.index()], 10.0),
            "no smoothing: it jumps"
        );
        assert!(!player.busy());

        motion.revision = 2;
        motion.resting[Prop::TranslateX.index()] = Some(30.0);
        motion.smoothing[Prop::TranslateX.index()] = Some(Smoothing {
            duration_ms: 100,
            ease: Ease::Linear,
        });
        player.apply(&motion);
        assert!(player.busy());
        assert!(close(player.values()[Prop::TranslateX.index()], 10.0));
        player.advance(50.0);
        assert!(close(player.values()[Prop::TranslateX.index()], 20.0));
        // Changing course half way starts from where it is.
        motion.revision = 3;
        motion.resting[Prop::TranslateX.index()] = Some(0.0);
        player.apply(&motion);
        player.advance(50.0);
        assert!(close(player.values()[Prop::TranslateX.index()], 10.0));
        player.advance(60.0);
        assert!(close(player.values()[Prop::TranslateX.index()], 0.0));
        assert!(!player.busy());
    }

    #[test]
    fn an_animated_property_wins_over_the_resting_one_and_gives_it_back() {
        let mut player = Player::default();
        let mut motion = Motion {
            revision: 1,
            animation: Some((1, Arc::new(spin()))),
            ..Default::default()
        };
        motion.resting[Prop::Rotation.index()] = Some(45.0);
        motion.resting[Prop::Opacity.index()] = Some(0.5);
        player.apply(&motion);
        player.advance(500.0);
        let values = player.values();
        assert!(close(values[Prop::Rotation.index()], 180.0));
        assert!(
            close(values[Prop::Opacity.index()], 0.5),
            "not animated, so resting"
        );
        motion.revision = 2;
        motion.animation = None;
        player.apply(&motion);
        assert!(close(player.values()[Prop::Rotation.index()], 45.0));
    }

    #[test]
    fn late_rendering_joins_the_host_timeline_and_holds_the_completed_pose() {
        let mut player = Player::default();
        player.apply(&Motion {
            revision: 1,
            started_at: Some(Instant::now() - std::time::Duration::from_secs(2)),
            animation: Some((
                1,
                Arc::new(Animation {
                    iterations: Some(1),
                    hold: true,
                    ..spin()
                }),
            )),
            ..Default::default()
        });
        player.advance(0.0);
        assert!(!player.busy());
        assert!(close(player.values()[Prop::Rotation.index()], 360.0));
    }

    #[test]
    fn long_lived_players_keep_subframe_precision() {
        let mut player = Player {
            clock: 100.0 * 24.0 * 3600.0 * 1000.0,
            ..Default::default()
        };
        player.apply(&Motion {
            revision: 1,
            animation: Some((1, Arc::new(spin()))),
            ..Default::default()
        });
        player.advance(8.0);
        assert!(close(player.values()[Prop::Rotation.index()], 2.88));
    }
}
