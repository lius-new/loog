# Extension motion

Extensions can move, turn, scale and fade the widgets they draw. The extension
describes the motion; the host plays it. The host has no named effects (no spinner,
no switch, no progress bar): it interpolates what is described, so a new kind of
animation is a new description, not new host code.

## Contract

`loog:ui/motion@0.1.0` (`wit/loog-extension/deps/ui/world.wit`), imported by the
`extension-ui` world beside `loog:ui/tree`. It is separate from `ui-description`
for the same reason placement is: the original records keep their ABI.

| Call | Meaning |
| --- | --- |
| `animate(node, animation)` | Start an animation, replacing the running one. |
| `stop(node)` | End it; properties return to their resting values. |
| `set(node, property, value)` | Set a resting value. |
| `smooth(node, smoothing)` | Make later changes of a resting value glide. |

An animation is tracks of keyframes over the properties `rotation`, `scale-x`,
`scale-y`, `translate-x`, `translate-y` and `opacity`, with a cubic Bezier or linear
easing per track, a duration, delay, iteration count (endless by default),
direction and whether to hold the last values. Turning and scaling are about the
node's center.

## Cost

- **Nothing per frame crosses to the extension.** Starting and stopping are one
  request each; the host interpolates on its own clock.
- **Nothing is laid out again.** Only properties a compositor can apply to retained
  content are animatable (transform and opacity). Sizes and colors are not; a
  determinate progress bar can smooth changes of `scale-x`. Width changes use
  ordinary layout updates.
- **Frames only while something moves.** A finished animation, or one that was
  stopped or removed, stops asking for frames. Finite animations release admission
  capacity at their host-clock deadline, including those holding their final pose.
  A first render that happens late joins that same timeline.

## Limits (`crates/extension-host/src/ui.rs`)

At most sixteen explicit animations per extension are active at once; each request counts against the round's UI
operations (64); an animation has one to six tracks, one per property, two to sixteen
keyframes from offset 0 to 1, durations and delays up to an hour, and values in range
for the property. A refused request leaves the running animation alone.

## Where things are

- `crates/extension-host/src/ui.rs`: validation, limits and the per-node request (`UiMotion`).
- `src/features/extensions/motion.rs`: the model and the arithmetic (`Animation`,
  `Ease`, `Player`), with no rendering types.
- `src/app/extensions/ui.rs`: wire types to the model.
- `src/ui/extension_widgets.rs`: a node that has a request becomes a composited layer
  in the node's place; `LayerMotion` adapts a `Player` to lgui's layer animation.
- `tests/extensions/rust-guest/src/lib.rs`: a Rust guest calling the motion WIT
  imports; `crates/extension-host/tests/rust_guest.rs` verifies the real component
  boundary. No TypeScript SDK is required.

## Rust guest example

Generate the `extension-ui` world with `wit-bindgen`, then import
`loog::ui::motion`. For example, an endless linear turn is ordinary data:

```rust
use loog::ui::motion::{self, Animation, Direction, Easing, Keyframe, Property, Track};
motion::animate(node, &Animation {
    tracks: vec![Track {
        property: Property::Rotation,
        keyframes: vec![Keyframe { offset: 0.0, value: 0.0 }, Keyframe { offset: 1.0, value: 360.0 }],
        easing: Easing::Linear,
    }],
    duration_ms: 800,
    delay_ms: 0,
    iterations: None,
    direction: Direction::Normal,
    hold: false,
})?;
```

The base host and tree interfaces are unchanged, so components built before
motion was added remain supported. The retained language-client WASM fixture
also exercises this older import surface against the current host. Adding motion
does not require replacing existing signed AOT packages or republishing plugins.
