//! Runtime widget descriptions composed through the normal retained LGUI tree.
use crate::features::extensions::motion::Player;
use crate::features::extensions::ui::{Attachment, Kind, Node, Origin, Payload, Relation};
use crate::ui::theme;
use lgui::core::*;
use lgui::prelude::panel;
use lgui::services::ServicesContextExt;
use std::sync::Arc;
pub type Sink = Arc<dyn Fn(Origin, Payload) + Send + Sync>;

/// A public placement target for any extension's transient form or panel.
/// The owner supplies its content, visibility and dismiss controls.
pub fn overlays(rect: UiRect) -> Element {
    // The layer spans the window, but only what extensions place in it should be under the
    // pointer: the cursor is resolved from the frontmost node, even one that handles no
    // events, so a hit area here would hide the cursor of everything beneath it, such as the
    // resize cursors of the drawers, panels and splits.
    Element::new(move |cx| {
        UiElement::group(cx.id, rect)
            .hit_rect(UiRect::new(rect.left, rect.top, rect.left, rect.top))
            .children(cx.children)
    })
    .key("window.overlays")
    .public_id("window.overlays")
    .layout(LayoutSpec::Flex(FlexLayout {
        axis: Axis::Vertical,
        justify: Justify::Center,
        align: Align::Center,
        gap: 12.0,
        padding: EdgeInsets::all(16.0),
        ..Default::default()
    }))
    .overflow(Overflow::Clip)
}

/// One application-wide staging tree. Generic LGUI projection resolves public
/// targets before measurement, independent of which native view declared them.
pub fn layer(rect: UiRect, nodes: Vec<Arc<Node>>, sink: Sink) -> Element {
    lgui::core::group(rect)
        .key("extension-ui-layer")
        .visibility(Visibility::Collapsed)
        .children(
            nodes
                .into_iter()
                // COMPAT(legacy-ui-mount): Old roots are rendered by the statusbar mount.
                // New roots enter this generic layer via Detached/Relative attachments.
                // Remove this exclusion with legacy root routing; no removal is scheduled.
                .filter(|node| node.attachment != Attachment::Legacy)
                .map(|node| {
                    // One stable native region per extension-supplied key. The
                    // selected content retains its private identity and event owner.
                    let shared = matches!(&node.attachment, Attachment::Shared { .. });
                    let (key, attachment) = match &node.attachment {
                        Attachment::Shared { key, placement, .. } => (
                            Arc::<str>::from(format!("shared-ui.{}:{key}", key.len())),
                            placement.as_ref().clone(),
                        ),
                        attachment => (node.origin.public_id(), attachment.clone()),
                    };
                    let widget = render(node, sink.clone());
                    let widget = if shared {
                        group(UiRect::default())
                            .public_id(key.clone())
                            .size_constraints(SizeConstraints::new(
                                Dimension::Content,
                                Dimension::Content,
                            ))
                            .flex_item(FlexItem {
                                shrink: 0.0,
                                ..Default::default()
                            })
                            .child(widget)
                    } else {
                        widget
                    };
                    Element::new(move |cx: ElementRenderCx<'_, '_, '_>| {
                        let widget = cx.compile(widget);
                        match attachment {
                            Attachment::Relative {
                                target,
                                relation,
                                order,
                            } => widget.relative_to(RelativePlacement {
                                target,
                                relation: match relation {
                                    Relation::Before => RelativeRelation::Before,
                                    Relation::After => RelativeRelation::After,
                                    Relation::Append => RelativeRelation::Append,
                                },
                                order,
                            }),
                            _ => widget,
                        }
                    })
                    .key(key.as_ref())
                }),
        )
}
/// The state of a node's composited layer: the host plays the `Motion` of the node.
#[derive(Clone, Default)]
struct LayerMotion(Player);
impl CompositingLayerAnimation for LayerMotion {
    fn advance(&mut self, elapsed_ms: f32) -> bool {
        self.0.advance(elapsed_ms)
    }
    fn compositing_layer_spec(&self) -> CompositingLayerSpec {
        let [
            rotation,
            scale_x,
            scale_y,
            translate_x,
            translate_y,
            opacity,
        ] = self.0.values();
        CompositingLayerSpec::new().opacity(opacity).transform(
            LayerTransform::identity()
                .rotation_degrees(rotation)
                .scale_xy(scale_x, scale_y)
                .translation(translate_x, translate_y),
        )
    }
    fn wants_frame(&self) -> bool {
        self.0.busy()
    }
}
#[derive(Clone)]
struct Props {
    node: Arc<Node>,
    sink: Sink,
}
/// Something at or under `node` is moved by the extension.
fn moves(node: &Node) -> bool {
    node.motion.revision != 0 || node.children.iter().any(|child| moves(child))
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        // A skipped subtree is not visited, and the layer of a moving node loses its
        // clock when it is not visited while what is around it is rebuilt, so the turn
        // would start over at every change of its neighbours. Such nodes are visited.
        self.node == other.node && !moves(&self.node)
    }
}
fn current(input: &(String, lgui::TextInputModel), external: &str) -> lgui::TextInputModel {
    if input.0 != external && input.1.text != external {
        lgui::TextInputModel::new(external)
    } else {
        input.1.clone()
    }
}
fn clipped(mut text: String) -> String {
    if text.len() > 4096 {
        let mut end = 4096;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
pub fn render(node: Arc<Node>, sink: Sink) -> Element {
    let key = node.origin.public_id();
    component(Props { node, sink }, |cx, props| {
        let node = props.node.clone();
        let p = &node.properties;
        let external = p.text.clone().unwrap_or_default();
        let initial = external.clone();
        let input = cx.state_with(move || (initial.clone(), lgui::TextInputModel::new(initial)));
        let height = p.height.unwrap_or(28.0);
        // Auto-sized text occupies its shaped advance. Paragraph paint bounds get
        // separate slack below, so avoiding line wrapping does not add layout gaps.
        let auto_text = p.kind == Kind::Text && p.width.is_none() && !external.is_empty();
        let width = p.width.or(match p.kind {
            Kind::TextInput => Some(120.0),
            Kind::Icon => Some(16.0),
            Kind::Text if !external.is_empty() => {
                Some(crate::ui::tabs::measure(&external, theme::SMALL, 400))
            }
            _ => None,
        });
        let rect = UiRect::new(0.0, 0.0, width.unwrap_or(0.0), height);
        let color = p.foreground.map(Color).unwrap_or(theme::c().text);
        // Named families: without them the box is measured in one font and painted in
        // another, and a block sized by its text cuts the text off. Status text is mono.
        let style = theme::mono(color, theme::SMALL);
        let panel_style = p
            .background
            .map(Color)
            .map(VisualStyle::filled)
            .unwrap_or_default();
        let mut element = match p.kind {
            Kind::Text if auto_text => {
                let paint_width = width.unwrap() + crate::ui::tabs::TEXT_MARGIN;
                group(rect).child(
                    text(
                        UiRect::new(0.0, 0.0, paint_width, height),
                        external.clone(),
                        style,
                    )
                    .size_constraints(SizeConstraints::new(
                        Dimension::Points(paint_width),
                        Dimension::Points(height),
                    )),
                )
            }
            Kind::Text => text(rect, external.clone(), style),
            Kind::Icon => {
                let icon = p.icon.clone().unwrap_or_else(|| Arc::from(""));
                Element::new(move |cx: ElementRenderCx<'_, '_, '_>| {
                    UiElement::icon(cx.id, rect, icon).icon_style(IconStyle::new(color))
                })
            }
            Kind::TextInput => {
                let read = input.clone();
                let expected = external.clone();
                let write = input;
                let previous = external.clone();
                let sink = props.sink.clone();
                let origin = node.origin.clone();
                let value = lgui::WidgetValue::new(
                    move || current(&read.get(), &expected),
                    move |mut model: lgui::TextInputModel| {
                        let before = current(&write.get(), &previous);
                        model.text = clipped(model.text);
                        model.select(
                            model.anchor.min(model.text.len()),
                            model.cursor.min(model.text.len()),
                        );
                        write.set((previous.clone(), model.clone()));
                        if before.text != model.text {
                            sink(origin.clone(), Payload::Input(model.text.clone()));
                        }
                        if before.selection() != model.selection() {
                            sink(origin.clone(), Payload::Selection(model.selection()));
                        }
                        if before.focused != model.focused {
                            sink(origin.clone(), Payload::Focus(model.focused));
                        }
                    },
                );
                let label = p.label.clone().unwrap_or_default();
                Element::new(move |cx: ElementRenderCx<'_, '_, '_>| {
                    let clipboard = cx
                        .try_use_context::<lgui::application::ApplicationContext>()
                        .map(|app| app.clipboard());
                    let read = clipboard.clone();
                    let clipboard = lgui::TextClipboard {
                        read: read.map(|port| {
                            Arc::new(move || port.read_text().ok().flatten().unwrap_or_default())
                                as Arc<dyn Fn() -> String + Send + Sync>
                        }),
                        write: clipboard.map(|port| {
                            Arc::new(move |text: String| {
                                let _ = port.write_text(&text);
                            }) as Arc<dyn Fn(String) + Send + Sync>
                        }),
                    };
                    cx.compile(
                        lgui::text_input(rect, value)
                            .label(label)
                            .clipboard(clipboard)
                            .style(panel_style, style)
                            .into(),
                    )
                })
            }
            _ => {
                let mut group = panel(rect, panel_style).layout(LayoutSpec::Flex(FlexLayout {
                    axis: if p.kind == Kind::Column {
                        Axis::Vertical
                    } else {
                        Axis::Horizontal
                    },
                    gap: p.gap,
                    padding: EdgeInsets::all(p.padding),
                    ..Default::default()
                }));
                if !external.is_empty() {
                    group = group.child(
                        text(UiRect::new(0.0, 0.0, 0.0, height), external, style).size_constraints(
                            SizeConstraints::new(Dimension::Content, Dimension::Points(height)),
                        ),
                    );
                }
                group.children(
                    node.children
                        .iter()
                        .map(|child| render(child.clone(), props.sink.clone())),
                )
            }
        };
        if matches!(p.kind, Kind::Block | Kind::Button) {
            let sink = props.sink.clone();
            let origin = node.origin.clone();
            element = element.on_event(UiEventKind::Click, move |cx, _| {
                cx.stop_propagation();
                sink(origin.clone(), Payload::Click);
            });
            if p.kind == Kind::Button {
                element = element
                    .interaction(InteractionRole::Button)
                    .cursor(CursorIcon::Pointer);
            }
        }
        if p.kind != Kind::TextInput {
            let role = match p.kind {
                Kind::Text => SemanticRole::Text,
                Kind::Icon => SemanticRole::Image,
                Kind::Button => SemanticRole::Button,
                _ => SemanticRole::Group,
            };
            let mut semantics = Semantics::new(role);
            if let Some(label) = p
                .label
                .clone()
                .or_else(|| (p.kind == Kind::Text).then(|| p.text.clone()).flatten())
            {
                semantics = semantics.name(label);
            }
            element = element.semantics(semantics);
        }
        // A node sized by its content keeps that size: when the bar is short of room
        // the block that fits its text is not squeezed until the text is cut off.
        let content_sized = p.width.is_none();
        let sizing = SizeConstraints::new(
            width.map(Dimension::Points).unwrap_or(Dimension::Content),
            Dimension::Points(height),
        );
        let flex = FlexItem {
            // Icons occupy a smaller box than the surrounding status text row.
            align_self: (p.kind == Kind::Icon).then_some(Align::Center),
            shrink: if content_sized { 0.0 } else { 1.0 },
            ..FlexItem::default()
        };
        let visibility = if p.visible {
            Visibility::Visible
        } else {
            Visibility::Collapsed
        };
        if node.motion.revision == 0 {
            return element
                .public_id(node.origin.public_id())
                .size_constraints(sizing)
                .flex_item(flex)
                .visibility(visibility)
                .overflow(Overflow::Clip);
        }
        // A node the extension moves, turns, scales or fades is a composited layer: the
        // host plays what was asked on the retained content, with no frame going back to
        // the extension and nothing laid out again. The layer takes the node's place in the
        // layout and the node sits in it unchanged.
        let motion = node.motion.clone();
        // Keyed by the node, so the state of the layer, which holds the clock, survives
        // every rebuild of what is around it.
        animated_compositing_layer(rect, move |layer: &mut LayerMotion| layer.0.apply(&motion))
            .key(format!("{}/motion", node.origin.public_id()))
            .public_id(node.origin.public_id())
            .size_constraints(sizing)
            .flex_item(flex)
            .visibility(visibility)
            .child(element.size_constraints(sizing).overflow(Overflow::Clip))
    })
    .key(key.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::extensions::motion::{Animation, Ease, Motion, Prop, Track};
    use crate::features::extensions::ui::Properties;
    use std::sync::Mutex;
    #[test]
    fn the_window_overlay_layer_does_not_mask_the_resize_cursors_under_it() {
        use lgui::application::AppView;
        use lgui::prelude::VisualStyle;
        use lgui::session::UiSession;
        let viewport = UiRect::new(0.0, 0.0, 800.0, 600.0);
        let handle = UiRect::new(492.0, 0.0, 500.0, 600.0);
        let view: AppView = Arc::new(move |_| {
            // A drawer's resize strip, then the layer the application puts over everything.
            panel(viewport, VisualStyle::default())
                .child(crate::ui::components::resize_handle::render(
                    "drawer",
                    handle,
                    UiRect::new(499.0, 0.0, 500.0, 600.0),
                    CursorIcon::ResizeHorizontal,
                    false,
                ))
                .child(overlays(viewport))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(
            session.tree().cursor_at(Point::new(496.0, 300.0)),
            Some(CursorIcon::ResizeHorizontal)
        );
    }

    fn node(kind: Kind) -> Arc<Node> {
        Arc::new(Node {
            attachment: Attachment::Legacy,
            origin: Origin {
                owner: "example.widgets".into(),
                session: 1,
                generation: 1,
                node: 1,
                id: "status".into(),
            },
            properties: Properties {
                id: "status".into(),
                kind,
                text: Some("Ready".into()),
                icon: None,
                foreground: Some(0xffffff),
                background: Some(0x123abc),
                width: Some(120.0),
                height: Some(28.0),
                padding: 0.0,
                gap: 0.0,
                visible: true,
                label: Some("Extension status".into()),
            },
            motion: Default::default(),
            children: vec![],
        })
    }
    fn input(
        session: &mut lgui::session::UiSession,
        context: &lgui::application::ApplicationContext,
        event: InputEvent,
    ) {
        let output = session.handle_input(event);
        lgui::core::dispatch_runtime_output(
            output,
            context,
            &lgui::window::WindowId::new("extension-widget-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
    }
    fn pointer(
        session: &mut lgui::session::UiSession,
        context: &lgui::application::ApplicationContext,
        point: Point,
    ) {
        let pointer = PointerData::mouse(point);
        input(
            session,
            context,
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
        );
        input(
            session,
            context,
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        );
    }
    #[test]
    fn overlay_target_centers_plugin_content_and_routes_clicks_without_a_window() {
        let mut widget = (*node(Kind::Button)).clone();
        widget.attachment = Attachment::Relative {
            target: "window.overlays".into(),
            relation: Relation::Append,
            order: 0,
        };
        let public = widget.origin.public_id();
        let widget = Arc::new(widget);
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let view: lgui::application::AppView = Arc::new(move |cx| {
            let viewport = cx.viewport();
            let seen = seen.clone();
            group(viewport).child(overlays(viewport)).child(layer(
                viewport,
                vec![widget.clone()],
                Arc::new(move |origin, payload| seen.lock().unwrap().push((origin, payload))),
            ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        for (width, height) in [(800.0, 600.0), (600.0, 360.0)] {
            session.render_view(&view, UiRect::new(0.0, 0.0, width, height), UiScale::ONE);
            let bounds = session.tree().public_node(&public).unwrap().bounds;
            assert!((bounds.left + bounds.width() / 2.0 - width / 2.0).abs() < 1.0);
            assert!((bounds.top + bounds.height() / 2.0 - height / 2.0).abs() < 1.0);
            pointer(
                &mut session,
                &context,
                Point::new(width / 2.0, height / 2.0),
            );
        }
        assert_eq!(events.lock().unwrap().len(), 2);
    }
    #[test]
    fn a_runtime_colored_button_has_a_public_identity_and_routes_native_clicks() {
        let node = node(Kind::Button);
        let public = node.origin.public_id();
        let seen = Arc::new(Mutex::new(vec![]));
        let events = seen.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            let events = events.clone();
            group(UiRect::new(0.0, 0.0, 600.0, 28.0))
                .layout(LayoutSpec::Flex(FlexLayout::default()))
                .child(render(
                    node.clone(),
                    Arc::new(move |origin, payload| events.lock().unwrap().push((origin, payload))),
                ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        session.render_view(&view, UiRect::new(0.0, 0.0, 600.0, 28.0), UiScale::ONE);
        let bounds = session.tree().public_node(&public).unwrap().bounds;
        assert_eq!(bounds.width(), 120.0);
        let native = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.public_id.as_deref() == Some(public.as_ref()))
            .unwrap();
        assert_eq!(native.style, VisualStyle::filled(Color(0x123abc)));
        pointer(&mut session, &context, Point::new(60.0, 14.0));
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, Payload::Click);
        assert_eq!(events[0].0.public_id(), public);
    }
    /// An icon beside a text, with what the extension asked for the icon.
    fn moving_row(motion: Motion) -> Arc<Node> {
        let child = |id: &str, node: u64, kind, motion| {
            let mut value = (*self::tests::node(kind)).clone();
            value.origin.node = node;
            value.origin.id = id.into();
            value.properties.id = id.into();
            value.properties.width = Some(14.0);
            value.properties.height = Some(14.0);
            value.properties.text = None;
            value.motion = motion;
            Arc::new(value)
        };
        // As a status block is built: the icon sits in a row of its own beside the rest.
        let mut mark = (*child("mark", 4, Kind::Row, Motion::default())).clone();
        mark.properties.width = None;
        mark.children = vec![child("icon", 2, Kind::Icon, motion)];
        let mut row = (*node(Kind::Row)).clone();
        row.properties.width = None;
        row.properties.text = None;
        row.children = vec![
            Arc::new(mark),
            child("after", 3, Kind::Block, Motion::default()),
        ];
        Arc::new(row)
    }
    fn spin_motion(revision: u64, animate: bool) -> Motion {
        Motion {
            revision,
            animation: animate.then(|| {
                (
                    revision,
                    Arc::new(Animation {
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
                    }),
                )
            }),
            ..Default::default()
        }
    }
    #[test]
    fn a_node_the_extension_moves_turns_on_the_hosts_frames_and_keeps_its_place() {
        let rect = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let build = |row: Arc<Mutex<Arc<Node>>>| -> lgui::application::AppView {
            Arc::new(move |_| {
                group(rect)
                    .layout(LayoutSpec::Flex(FlexLayout::default()))
                    .child(render(row.lock().unwrap().clone(), Arc::new(|_, _| {})))
            })
        };
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let icon = moving_row(Motion::default()).children[0].children[0]
            .origin
            .public_id();
        let after = moving_row(Motion::default()).children[1].origin.public_id();

        // Where the nodes are with no motion asked for.
        let still = Arc::new(Mutex::new(moving_row(Motion::default())));
        let still_view = build(still);
        let mut plain = lgui::session::UiSession::new();
        plain.render_view(&still_view, rect, UiScale::ONE);
        let place = |session: &lgui::session::UiSession| {
            (
                session.tree().public_node(&icon).unwrap().bounds,
                session.tree().public_node(&after).unwrap().bounds,
            )
        };
        let resting = place(&plain);

        let row = Arc::new(Mutex::new(moving_row(spin_motion(1, true))));
        let view = build(row.clone());
        let mut session = lgui::session::UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(place(&session), resting, "moving it does not move anything");
        let turned = |session: &lgui::session::UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.public_id.as_deref() == Some(icon.as_ref()))
                .and_then(|node| node.compositing_layer)
                .expect("a moving node is a composited layer")
                .transform
                .rotation_degrees_f32()
        };
        assert_eq!(session.runtime().frame_interval_ms(), Some(16));
        session.advance(16.0);
        session.advance(234.0);
        session.render_view(&view, rect, UiScale::ONE);
        let angle = turned(&session);
        assert!(
            (angle - 90.0).abs() < 8.0,
            "a quarter of a turn in 250 ms: {angle}"
        );
        assert_eq!(place(&session), resting, "nor does turning");

        // A change beside it, or in the row around it, does not restart the turn.
        let mut busy_on_the_side = (*moving_row(spin_motion(1, true))).clone();
        busy_on_the_side.properties.background = Some(0x445566);
        let mut sibling = (*busy_on_the_side.children[1]).clone();
        sibling.properties.width = Some(30.0);
        busy_on_the_side.children[1] = Arc::new(sibling);
        *row.lock().unwrap() = Arc::new(busy_on_the_side);
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(turned(&session), angle, "the layer was kept, not rebuilt");
        session.advance(250.0);
        session.render_view(&view, rect, UiScale::ONE);
        assert!(turned(&session) > angle + 45.0, "and it kept turning");

        // Stopping it returns the node to rest and ends the frames.
        *row.lock().unwrap() = moving_row(spin_motion(2, false));
        session.render_view(&view, rect, UiScale::ONE);
        session.advance(16.0);
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(turned(&session), 0.0);
        assert!(session.runtime().frame_interval_ms().is_none());
    }
    /// What a window shows when it repaints only what changed is what a full repaint shows.
    #[test]
    fn repainting_only_what_changed_shows_what_a_full_repaint_shows() {
        use lgui::assets::RenderResources;
        use lgui::core::PhysicalRect;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        let (width, height) = (60_u32, 28_u32);
        let rect = UiRect::new(0.0, 0.0, width as f32, height as f32);
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                lgui::icons::SvgIconRegistry::new().with_icon("sync", icondata::VsSync),
            )));
        let mut row = (*moving_row(spin_motion(1, true))).clone();
        let mut mark = (*row.children[0]).clone();
        mark.properties.background = Some(0xffffff);
        mark.properties.padding = 5.0;
        mark.properties.height = Some(24.0);
        let mut icon = (*mark.children[0]).clone();
        icon.properties.icon = Some("sync".into());
        icon.properties.foreground = Some(0x818cf8);
        icon.properties.background = None;
        mark.children = vec![Arc::new(icon)];
        row.children = vec![Arc::new(mark)];
        let row = Arc::new(row);
        let view: lgui::application::AppView = Arc::new(move |_| {
            // Fractional offsets, as in a bar where text before it has a fractional width.
            group(rect)
                .layout(LayoutSpec::Flex(FlexLayout {
                    padding: EdgeInsets {
                        left: 3.7,
                        top: 0.35,
                        right: 0.0,
                        bottom: 0.0,
                    },
                    ..Default::default()
                }))
                .child(render(row.clone(), Arc::new(|_, _| {})))
        });
        let mut session = lgui::session::UiSession::new();
        let first = session.render_view(&view, rect, UiScale::ONE);
        let mut surface = SkiaSoftwareSurface::new_opaque(64 << 20);
        let mut full = SkiaSoftwareSurface::new_opaque(64 << 20);
        let viewport = PhysicalRect::new(0, 0, width as i32, height as i32);
        let mut counts = Vec::new();
        let mut damage = vec![viewport];
        let mut committed_scene = first.scene;
        let mut shown = Vec::new();
        for index in 0..130_u32 {
            if index > 0 {
                let output = session.advance(16.0);
                let commit = session.render_view(&view, rect, UiScale::ONE);
                // What a window would be told to repaint this frame.
                let mut rects: Vec<UiRect> = commit.damage.dirty.rects().to_vec();
                if let Some(bounds) = output.dirty_bounds {
                    rects.push(bounds);
                }
                damage = if commit.damage.dirty.is_full() {
                    vec![viewport]
                } else {
                    rects
                        .iter()
                        .map(|r| {
                            PhysicalRect::new(
                                r.left.floor() as i32,
                                r.top.floor() as i32,
                                r.right.ceil() as i32,
                                r.bottom.ceil() as i32,
                            )
                        })
                        .collect()
                };
                committed_scene = commit.scene;
            }
            let scene = &committed_scene;
            let reference = session.tree().scene();
            let frame = FrameInfo::new(
                viewport,
                &damage,
                UiScale::ONE,
                FrameReason::SceneChange,
                index == 0,
            );
            lgui::assets::backend::with_render_resources(&context, RenderResources::new(), || {
                surface.draw(scene, &frame).expect("draw")
            });
            let whole = [viewport];
            let everything = FrameInfo::new(
                viewport,
                &whole,
                UiScale::ONE,
                FrameReason::SceneChange,
                true,
            );
            lgui::assets::backend::with_render_resources(&context, RenderResources::new(), || {
                full.draw(&reference, &everything).expect("draw")
            });
            let pixels = surface.pixels().to_vec();
            // Pixels that differ from a full repaint are what a window would show wrongly.
            let ink = pixels
                .iter()
                .zip(full.pixels())
                .filter(|(a, b)| a != b)
                .count();
            counts.push(ink);
            // How much of the white mark (24 by 24) is covered by the icon.
            let covered = (0..24_u32)
                .flat_map(|y| (0..24_u32).map(move |x| (x, y)))
                .filter(|(x, y)| {
                    let at = ((y * width + x) * 4) as usize;
                    !(pixels[at] > 240 && pixels[at + 1] > 240 && pixels[at + 2] > 240)
                })
                .count();
            shown.push(covered);
        }
        assert!(
            counts.iter().all(|count| *count == 0),
            "bytes differing from a full repaint, per frame: {counts:?}"
        );
        let (least, most) = (shown.iter().min().unwrap(), shown.iter().max().unwrap());
        assert!(
            *least * 10 >= *most * 6,
            "the icon is not whole in every frame; pixels per frame: {shown:?}"
        );
    }
    /// A status block changes around its icon all the time; the turn must not start over.
    #[test]
    fn a_turning_node_keeps_its_clock_when_what_is_around_it_changes() {
        let rect = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let icon = moving_row(Motion::default()).children[0].children[0]
            .origin
            .public_id();
        for (name, change) in [
            ("identical rebuild", 0),
            ("root background", 1),
            ("sibling width", 2),
            ("icon foreground", 3),
            ("the row around the icon", 4),
        ] {
            let row = Arc::new(Mutex::new(moving_row(spin_motion(1, true))));
            let shared = row.clone();
            let view: lgui::application::AppView = Arc::new(move |_| {
                group(rect)
                    .layout(LayoutSpec::Flex(FlexLayout::default()))
                    .child(render(shared.lock().unwrap().clone(), Arc::new(|_, _| {})))
            });
            let mut session = lgui::session::UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            session.advance(16.0);
            session.advance(234.0);
            session.render_view(&view, rect, UiScale::ONE);
            let angle = |session: &lgui::session::UiSession| {
                session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.public_id.as_deref() == Some(icon.as_ref()))
                    .and_then(|node| node.compositing_layer)
                    .map(|layer| layer.transform.rotation_degrees_f32())
            };
            let before = angle(&session);
            let mut next = (*moving_row(spin_motion(1, true))).clone();
            match change {
                1 => next.properties.background = Some(0x445566),
                2 => {
                    let mut sibling = (*next.children[1]).clone();
                    sibling.properties.width = Some(30.0);
                    next.children[1] = Arc::new(sibling);
                }
                3 => {
                    let mut mark = (*next.children[0]).clone();
                    let mut child = (*mark.children[0]).clone();
                    child.properties.foreground = Some(0x111111);
                    mark.children[0] = Arc::new(child);
                    next.children[0] = Arc::new(mark);
                }
                4 => {
                    let mut mark = (*next.children[0]).clone();
                    mark.properties.background = Some(0xffffff);
                    next.children[0] = Arc::new(mark);
                }
                _ => {}
            }
            *row.lock().unwrap() = Arc::new(next);
            session.render_view(&view, rect, UiScale::ONE);
            assert_eq!(angle(&session), before, "{name}: the turn started over");
            assert_eq!(before, Some(90.0), "{name}");
        }
    }
    #[test]
    fn a_relative_text_input_keeps_focus_and_utf8_state_when_moved() {
        let mut initial = (*node(Kind::TextInput)).clone();
        initial.attachment = Attachment::Relative {
            target: "input.start".into(),
            relation: Relation::Before,
            order: 0,
        };
        let node = Arc::new(Mutex::new(Arc::new(initial)));
        let read = node.clone();
        let seen = Arc::new(Mutex::new(vec![]));
        let events = seen.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            let events = events.clone();
            let rect = UiRect::new(0.0, 0.0, 600.0, 28.0);
            group(rect)
                .child(
                    group(rect)
                        .layout(LayoutSpec::Flex(FlexLayout::default()))
                        .child(group(UiRect::new(0.0, 0.0, 100.0, 28.0)).public_id("input.start"))
                        .child(group(UiRect::new(0.0, 0.0, 100.0, 28.0)).public_id("input.end")),
                )
                .child(layer(
                    rect,
                    vec![read.lock().unwrap().clone()],
                    Arc::new(move |_, payload| events.lock().unwrap().push(payload)),
                ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 28.0);
        session.render_view(&view, viewport, UiScale::ONE);
        pointer(&mut session, &context, Point::new(100.0, 14.0));
        input(&mut session, &context, InputEvent::TextInput("中😀".into()));
        session.render_view(&view, viewport, UiScale::ONE);
        Arc::make_mut(&mut node.lock().unwrap()).attachment = Attachment::Relative {
            target: "input.end".into(),
            relation: Relation::After,
            order: 0,
        };
        session.render_view(&view, viewport, UiScale::ONE);
        input(&mut session, &context, InputEvent::TextInput("续".into()));
        session.render_view(&view, viewport, UiScale::ONE);
        let seen = seen.lock().unwrap();
        let text = seen
            .iter()
            .rev()
            .find_map(|event| {
                if let Payload::Input(text) = event {
                    Some(text)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(text.contains("中😀"));
        assert!(
            text.contains("续"),
            "moving the input lost its focus or state"
        );
        let range = seen
            .iter()
            .rev()
            .find_map(|event| {
                if let Payload::Selection(range) = event {
                    Some(range)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(
            range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
        );
    }

    #[test]
    fn relative_widgets_move_between_arbitrary_native_nodes_without_losing_identity() {
        let mut widget = (*node(Kind::Button)).clone();
        widget.attachment = Attachment::Relative {
            target: "demo.anchor".into(),
            relation: Relation::Before,
            order: 10,
        };
        let public = widget.origin.public_id();
        let widgets = Arc::new(Mutex::new(vec![Arc::new(widget)]));
        let reader = widgets.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let viewport = UiRect::new(0.0, 0.0, 800.0, 28.0);
        let view: lgui::application::AppView = Arc::new(move |_| {
            let seen = seen.clone();
            group(viewport)
                .child(
                    group(viewport)
                        .layout(LayoutSpec::Flex(FlexLayout {
                            justify: Justify::End,
                            ..Default::default()
                        }))
                        .child(
                            text(
                                UiRect::new(0.0, 0.0, 100.0, 28.0),
                                "Anchor",
                                theme::mono(theme::c().text, 12.0),
                            )
                            .public_id("demo.anchor"),
                        )
                        .child(
                            text(
                                UiRect::new(0.0, 0.0, 100.0, 28.0),
                                "End",
                                theme::mono(theme::c().text, 12.0),
                            )
                            .public_id("demo.end"),
                        ),
                )
                .child(layer(
                    viewport,
                    reader.lock().unwrap().clone(),
                    Arc::new(move |origin, payload| seen.lock().unwrap().push((origin, payload))),
                ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let original = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.public_id.as_deref() == Some(public.as_ref()))
            .unwrap()
            .id
            .clone();
        let bounds = session.tree().public_node(&public).unwrap().bounds;
        let anchor = session.tree().public_node("demo.anchor").unwrap().bounds;
        assert_eq!(bounds.right, anchor.left);
        pointer(
            &mut session,
            &context,
            Point::new((bounds.left + bounds.right) / 2.0, 14.0),
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        Arc::make_mut(&mut widgets.lock().unwrap()[0]).attachment = Attachment::Relative {
            target: "demo.end".into(),
            relation: Relation::After,
            order: 0,
        };
        session.render_view(&view, viewport, UiScale::ONE);
        let moved = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.public_id.as_deref() == Some(public.as_ref()))
            .unwrap();
        assert_eq!(moved.id, original);
        let bounds = session.tree().public_node(&public).unwrap().bounds;
        assert_eq!(
            bounds.left,
            session.tree().public_node("demo.end").unwrap().bounds.right
        );
        pointer(
            &mut session,
            &context,
            Point::new((bounds.left + bounds.right) / 2.0, 14.0),
        );
        assert_eq!(events.lock().unwrap().len(), 2);
        widgets.lock().unwrap().clear();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(session.tree().public_node(&public).is_none());
    }

    #[test]
    fn relative_widgets_wait_for_targets_and_rebind_after_native_reconstruction() {
        let mut widget = (*node(Kind::Button)).clone();
        widget.attachment = Attachment::Relative {
            target: "demo.optional".into(),
            relation: Relation::Before,
            order: 0,
        };
        let widget = Arc::new(widget);
        let public = widget.origin.public_id();
        let visible = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let visibility = visible.clone();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let view: lgui::application::AppView = Arc::new(move |_| {
            let mut native = group(viewport).layout(LayoutSpec::Flex(FlexLayout::default()));
            if visibility.load(std::sync::atomic::Ordering::Relaxed) {
                native = native
                    .child(group(UiRect::new(0.0, 0.0, 100.0, 28.0)).public_id("demo.optional"));
            }
            group(viewport).child(native).child(layer(
                viewport,
                vec![widget.clone()],
                Arc::new(|_, _| {}),
            ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        let mut original = None;
        for show in [false, true, false, true] {
            visible.store(show, std::sync::atomic::Ordering::Relaxed);
            session.render_view(&view, viewport, UiScale::ONE);
            let native = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.public_id.as_deref() == Some(public.as_ref()))
                .unwrap();
            if let Some(original) = &original {
                assert_eq!(&native.id, original);
            } else {
                original = Some(native.id.clone());
            }
            if show {
                assert_eq!(
                    session.tree().public_node(&public).unwrap().bounds.right,
                    session
                        .tree()
                        .public_node("demo.optional")
                        .unwrap()
                        .bounds
                        .left
                );
            } else {
                let parent = session
                    .tree()
                    .node(native.parent.as_ref().unwrap())
                    .unwrap();
                assert_eq!(parent.visibility, Visibility::Collapsed);
                assert_ne!(
                    session
                        .tree()
                        .hit_test(Point::new(60.0, 14.0))
                        .map(|hit| hit.id),
                    Some(native.id.clone())
                );
            }
        }
    }
    #[test]
    fn auto_text_slack_keeps_a_ten_pixel_gap_without_clipping_words() {
        use lgui::assets::RenderResources;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        let rect = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for words in ["Status: Ready", "Indexing a workspace", "状态：就绪 😀"] {
            let natural = crate::ui::tabs::measure(words, theme::SMALL, 400);
            let mut body = (*node(Kind::Text)).clone();
            body.origin.id = "body".into();
            body.origin.node = 2;
            body.properties.id = "body".into();
            body.properties.text = Some(words.into());
            body.properties.width = None;
            let public = body.origin.public_id();
            let mut after = (*node(Kind::Block)).clone();
            after.origin.id = "after".into();
            after.origin.node = 3;
            after.properties.id = "after".into();
            after.properties.text = None;
            after.properties.width = Some(24.0);
            after.properties.background = None;
            let next = after.origin.public_id();
            let mut row = (*node(Kind::Row)).clone();
            row.properties.text = None;
            row.properties.width = None;
            row.properties.background = None;
            row.properties.gap = 10.0;
            row.children = vec![Arc::new(body), Arc::new(after)];
            let row = Arc::new(row);
            let view: lgui::application::AppView = Arc::new(move |_| {
                group(rect)
                    .layout(LayoutSpec::Flex(FlexLayout::default()))
                    .child(render(row.clone(), Arc::new(|_, _| {})))
            });
            // Independent native reference: a measured, clipped slot with a wider
            // paragraph painting area, followed by the normal layout gap. Matching
            // the clip also matches subpixel coverage at fractional display scales.
            let reference: lgui::application::AppView = Arc::new(move |_| {
                group(rect)
                    .layout(LayoutSpec::Flex(FlexLayout {
                        gap: 10.0,
                        ..Default::default()
                    }))
                    .child(
                        group(UiRect::new(0.0, 0.0, natural, 28.0))
                            .size_constraints(SizeConstraints::new(
                                Dimension::Points(natural),
                                Dimension::Points(28.0),
                            ))
                            .overflow(Overflow::Clip)
                            .child(text(
                                UiRect::new(0.0, 0.0, natural + crate::ui::tabs::TEXT_MARGIN, 28.0),
                                words,
                                theme::mono(Color::WHITE, theme::SMALL),
                            )),
                    )
                    .child(group(UiRect::new(0.0, 0.0, 24.0, 28.0)))
            });
            for factor in [1.0, 1.25, 1.5, 2.0] {
                let scale = UiScale::new(factor);
                let mut session = lgui::session::UiSession::new();
                let actual = session.render_view(&view, rect, scale);
                let bounds = session.tree().public_node(&public).unwrap().bounds;
                let after = session.tree().public_node(&next).unwrap().bounds;
                assert!(
                    (bounds.width() - natural).abs() < 0.1,
                    "{words}: paint slack widened layout"
                );
                assert!(
                    (after.left - bounds.right - 10.0).abs() < 0.1,
                    "{words}: incorrect gap"
                );
                let mut expected_session = lgui::session::UiSession::new();
                let expected = expected_session.render_view(&reference, rect, scale);
                let viewport = PhysicalRect::new(
                    0,
                    0,
                    (600.0 * factor).ceil() as i32,
                    (28.0 * factor).ceil() as i32,
                );
                let damage = [viewport];
                let frame =
                    FrameInfo::new(viewport, &damage, scale, FrameReason::SceneChange, true);
                let mut actual_pixels = SkiaSoftwareSurface::new_opaque(4 << 20);
                let mut expected_pixels = SkiaSoftwareSurface::new_opaque(4 << 20);
                lgui::assets::backend::with_render_resources(
                    &context,
                    RenderResources::new(),
                    || {
                        actual_pixels
                            .draw(&actual.scene.project_to_physical(scale), &frame)
                            .unwrap();
                        expected_pixels
                            .draw(&expected.scene.project_to_physical(scale), &frame)
                            .unwrap();
                    },
                );
                assert!(
                    actual_pixels.pixels() == expected_pixels.pixels(),
                    "{words}, scale {factor}: text differs from the complete native paragraph; actual text {:?}, reference text {:?}, first differing bytes {:?}",
                    session
                        .tree()
                        .nodes()
                        .iter()
                        .filter(|node| node.text.is_some())
                        .map(|node| (node.text.as_deref(), node.layout_rect))
                        .collect::<Vec<_>>(),
                    expected_session
                        .tree()
                        .nodes()
                        .iter()
                        .filter(|node| node.text.is_some())
                        .map(|node| (node.text.as_deref(), node.layout_rect))
                        .collect::<Vec<_>>(),
                    actual_pixels
                        .pixels()
                        .iter()
                        .zip(expected_pixels.pixels())
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .take(12)
                        .map(|(index, (a, b))| (
                            index / 4 % viewport.width() as usize,
                            index / 4 / viewport.width() as usize,
                            *a,
                            *b
                        ))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    #[test]
    fn a_shared_region_reuses_one_native_container_across_contexts_and_owners() {
        use crate::features::extensions::ui::{FlatNode, Model};
        let rect = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let candidate = |context: &str, priority| {
            let value = node(Kind::Text);
            FlatNode {
                node: 1,
                parent: None,
                properties: value.properties.clone(),
                motion: Default::default(),
                attachment: Attachment::Shared {
                    key: "example.shared".into(),
                    contexts: Some(vec![context.into()]),
                    priority,
                    placement: Box::new(Attachment::Relative {
                        target: "native.anchor".into(),
                        relation: Relation::Before,
                        order: 10,
                    }),
                },
            }
        };
        let mut model = Model::default();
        model.begin_session(1);
        model.replace(1, "first".into(), 1, vec![candidate("file-a", 2)]);
        model.replace(1, "second".into(), 1, vec![candidate("file-a", 1)]);
        model.replace(1, "third".into(), 1, vec![candidate("file-b", 99)]);
        let model = Arc::new(Mutex::new(model));
        let read = model.clone();
        let context = Arc::new(Mutex::new(String::from("file-a")));
        let active = context.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            group(rect)
                .child(
                    group(rect)
                        .layout(LayoutSpec::Flex(FlexLayout::default()))
                        .child(group(UiRect::new(0.0, 0.0, 60.0, 28.0)).public_id("native.anchor")),
                )
                .child(layer(
                    rect,
                    read.lock()
                        .unwrap()
                        .roots_for(Some(&active.lock().unwrap())),
                    Arc::new(|_, _| {}),
                ))
        });
        let app = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &app,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        let region = "shared-ui.14:example.shared";
        let container = |session: &lgui::session::UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .filter(|node| node.public_id.as_deref() == Some(region))
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        session.render_view(&view, rect, UiScale::ONE);
        let original = container(&session);
        assert_eq!(original.len(), 1);
        assert!(session.tree().public_node(region).unwrap().bounds.width() >= 120.0);
        model.lock().unwrap().remove(1, "first");
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(
            container(&session),
            original,
            "removing the creator retains the shared native container"
        );
        *context.lock().unwrap() = "file-b".into();
        session.render_view(&view, rect, UiScale::ONE);
        assert_eq!(
            container(&session),
            original,
            "switching context changes private content, not the region"
        );
        *context.lock().unwrap() = "unrelated".into();
        session.render_view(&view, rect, UiScale::ONE);
        assert!(container(&session).is_empty());
    }
    #[test]
    fn progress_and_pointer_repaints_preserve_the_loading_icon() {
        let rect = UiRect::new(0.0, 0.0, 600.0, 24.0);
        let mut initial = (*moving_row(spin_motion(1, true))).clone();
        let icon = initial.children[0].children[0].clone();
        let public = icon.origin.public_id();
        let mut body = (*initial.children[1]).clone();
        body.properties.kind = Kind::Text;
        body.properties.width = None;
        body.properties.height = Some(24.0);
        body.properties.text = Some("Starting".into());
        initial.children = vec![icon, Arc::new(body)];
        initial.properties.height = Some(24.0);
        initial.attachment = Attachment::Relative {
            target: "statusbar.cursor-position".into(),
            relation: Relation::Before,
            order: 100,
        };
        let shared = Arc::new(Mutex::new(Arc::new(initial)));
        let read = shared.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            group(rect)
                .child(
                    group(rect)
                        .layout(LayoutSpec::Flex(FlexLayout {
                            justify: Justify::End,
                            ..Default::default()
                        }))
                        .child(
                            group(UiRect::new(0.0, 0.0, 60.0, 24.0))
                                .public_id("statusbar.cursor-position"),
                        ),
                )
                .child(layer(
                    rect,
                    vec![read.lock().unwrap().clone()],
                    Arc::new(|_, _| {}),
                ))
        });
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let mut session = lgui::session::UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let angle = |session: &lgui::session::UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.public_id.as_deref() == Some(public.as_ref()))
                .unwrap()
                .compositing_layer
                .unwrap()
                .transform
                .rotation_degrees_f32()
        };
        for index in 1..60 {
            session.advance(16.0);
            let before = angle(&session);
            let mut next = (**shared.lock().unwrap()).clone();
            let mut body = (*next.children[1]).clone();
            body.properties.text = Some(format!("Indexing {} files: {}%", index * 127, index));
            next.children[1] = Arc::new(body);
            *shared.lock().unwrap() = Arc::new(next);
            session.render_view(&view, rect, UiScale::ONE);
            assert_eq!(angle(&session), before, "progress {index} reset the spin");
            let bounds = session.tree().public_node(&public).unwrap().bounds;
            for point in [
                Point::new(bounds.left + 7.0, bounds.top + 7.0),
                Point::new(1.0, 1.0),
            ] {
                input(
                    &mut session,
                    &context,
                    InputEvent::PointerMove(PointerData::mouse(point)),
                );
                session.render_view(&view, rect, UiScale::ONE);
                assert_eq!(angle(&session), before, "pointer {index} reset the spin");
            }
        }
    }
    #[test]
    fn inline_paths_remain_visible_while_rotating_without_a_registered_icon() {
        use lgui::assets::RenderResources;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        let rect = UiRect::new(0.0, 0.0, 60.0, 24.0);
        let context = lgui::application::ApplicationContext::empty(Default::default());
        for factor in [1.0, 1.25, 1.5, 2.0] {
            let mut icon = (*node(Kind::Icon)).clone();
            icon.properties.width = Some(14.0);
            icon.properties.height = Some(14.0);
            icon.properties.background = None;
            icon.properties.foreground = Some(0x818cf8);
            icon.properties.icon =
                Some("svg-path:20x20:M10 2A8 8 0 0 0 2 10H4A6 6 0 0 1 10 4Z".into());
            icon.motion = spin_motion(1, true);
            let public = icon.origin.public_id();
            let icon = Arc::new(icon);
            let view: lgui::application::AppView = Arc::new(move |_| {
                group(rect)
                    .layout(LayoutSpec::Flex(FlexLayout {
                        padding: EdgeInsets::all(5.0),
                        ..Default::default()
                    }))
                    .child(render(icon.clone(), Arc::new(|_, _| {})))
            });
            let scale = UiScale::new(factor);
            let mut session = lgui::session::UiSession::new();
            let viewport = PhysicalRect::new(0, 0, (60.0 * factor) as i32, (24.0 * factor) as i32);
            let mut surface = SkiaSoftwareSurface::new_opaque(4 << 20);
            let mut previous = Vec::new();
            let mut changed = false;
            for index in 0..60 {
                if index > 0 {
                    session.advance(16.0);
                }
                let commit = session.render_view(&view, rect, scale);
                let bounds = session.tree().public_node(&public).unwrap().bounds;
                assert_eq!((bounds.width(), bounds.height()), (14.0, 14.0));
                let dirty = [viewport];
                let frame = FrameInfo::new(viewport, &dirty, scale, FrameReason::SceneChange, true);
                lgui::assets::backend::with_render_resources(
                    &context,
                    RenderResources::new(),
                    || {
                        surface
                            .draw(&commit.scene.project_to_physical(scale), &frame)
                            .unwrap();
                    },
                );
                let pixels = surface.pixels();
                let ink = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[0] > 30 || p[1] > 30 || p[2] > 30)
                    .count();
                assert!(
                    ink >= (4.0 * factor * factor) as usize,
                    "scale {factor}, frame {index}: path vanished ({ink} pixels)"
                );
                if !previous.is_empty() && pixels != previous {
                    changed = true;
                }
                previous = pixels.to_vec();
            }
            assert!(
                changed,
                "scale {factor}: rotation must change visible pixels"
            );
        }
    }
    #[test]
    fn small_status_icons_are_centered_in_the_text_row_with_or_without_a_layer() {
        let viewport = UiRect::new(0.0, 0.0, 600.0, 28.0);
        let context = lgui::application::ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for revision in [0, 1] {
            let child = |id: &str, handle, kind, width, height, value: Option<&str>| {
                let mut child = (*node(kind)).clone();
                child.origin.id = id.into();
                child.origin.node = handle;
                child.properties.id = id.into();
                child.properties.width = width;
                child.properties.height = Some(height);
                child.properties.text = value.map(str::to_owned);
                child.properties.background = None;
                child
            };
            let mut icon = child("status-icon", 3, Kind::Icon, Some(14.0), 14.0, None);
            icon.properties.icon = Some("check".into());
            icon.motion.revision = revision;
            let icon_id = icon.origin.public_id();
            let mut row = (*node(Kind::Row)).clone();
            row.properties.width = None;
            row.properties.height = Some(24.0);
            row.properties.text = None;
            let row_id = row.origin.public_id();
            let label = child(
                "status-text",
                5,
                Kind::Text,
                None,
                24.0,
                Some("gopls Language Server"),
            );
            let label_id = label.origin.public_id();
            row.children = vec![
                Arc::new(child("leading", 2, Kind::Text, Some(10.0), 24.0, Some(""))),
                Arc::new(icon),
                Arc::new(child("gap", 4, Kind::Text, Some(6.0), 24.0, Some(""))),
                Arc::new(label),
                Arc::new(child("trailing", 6, Kind::Text, Some(10.0), 24.0, Some(""))),
            ];
            let row = Arc::new(row);
            let view: lgui::application::AppView = Arc::new(move |_| {
                group(viewport)
                    .layout(LayoutSpec::Flex(FlexLayout::default()))
                    .child(render(row.clone(), Arc::new(|_, _| {})))
            });
            for factor in [1.0, 1.25, 1.5, 2.0] {
                let mut session = lgui::session::UiSession::new();
                session.render_view(&view, viewport, UiScale::new(factor));
                let bounds = |id: &str| session.tree().public_node(id).unwrap().bounds;
                let row = bounds(&row_id);
                let icon = bounds(&icon_id);
                let label = bounds(&label_id);
                assert_eq!(icon.height(), 14.0, "icon size is retained");
                assert_eq!(icon.width(), 14.0);
                assert_eq!(
                    icon.top + icon.height() / 2.0,
                    row.top + row.height() / 2.0,
                    "revision {revision}, scale {factor}: icon must be centered vertically"
                );
                assert_eq!(label.top, row.top);
                assert_eq!(icon.left - row.left, 10.0, "leading spacing is retained");
                assert_eq!(label.left - icon.right, 6.0, "text spacing is retained");
            }
        }
    }
}
