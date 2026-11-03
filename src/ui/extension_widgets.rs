//! Runtime widget descriptions composed through the normal retained LGUI tree.
use crate::features::extensions::ui::{Attachment, Kind, Node, Origin, Payload, Relation};
use crate::ui::theme;
use lgui::core::*;
use lgui::prelude::panel;
use lgui::services::ServicesContextExt;
use std::sync::Arc;
pub type Sink = Arc<dyn Fn(Origin, Payload) + Send + Sync>;

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
                    let key = node.origin.public_id();
                    let attachment = node.attachment.clone();
                    let widget = render(node, sink.clone());
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
#[derive(Clone)]
struct Props {
    node: Arc<Node>,
    sink: Sink,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node
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
        let width = p.width.or(match p.kind {
            Kind::TextInput => Some(120.0),
            Kind::Icon => Some(16.0),
            _ => None,
        });
        let rect = UiRect::new(0.0, 0.0, width.unwrap_or(0.0), height);
        let color = p.foreground.map(Color).unwrap_or(theme::c().text);
        let style = TextStyle::new(color, theme::SMALL, 400);
        let panel_style = p
            .background
            .map(Color)
            .map(VisualStyle::filled)
            .unwrap_or_default();
        let mut element = match p.kind {
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
        element
            .public_id(node.origin.public_id())
            .size_constraints(SizeConstraints::new(
                width.map(Dimension::Points).unwrap_or(Dimension::Content),
                Dimension::Points(height),
            ))
            .visibility(if p.visible {
                Visibility::Visible
            } else {
                Visibility::Collapsed
            })
            .overflow(Overflow::Clip)
    })
    .key(key.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::extensions::ui::Properties;
    use std::sync::Mutex;
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
}
