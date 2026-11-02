//! Runtime widget descriptions composed through the normal retained LGUI tree.
use crate::features::extensions::ui::{Kind, Node, Origin, Payload};
use crate::ui::theme;
use lgui::core::*;
use lgui::prelude::panel;
use lgui::services::ServicesContextExt;
use std::sync::Arc;
pub type Sink = Arc<dyn Fn(Origin, Payload) + Send + Sync>;
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
    fn a_runtime_text_input_emits_committed_utf8_text_and_selection() {
        let node = node(Kind::TextInput);
        let seen = Arc::new(Mutex::new(vec![]));
        let events = seen.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            let events = events.clone();
            group(UiRect::new(0.0, 0.0, 600.0, 28.0))
                .layout(LayoutSpec::Flex(FlexLayout::default()))
                .child(render(
                    node.clone(),
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
}
