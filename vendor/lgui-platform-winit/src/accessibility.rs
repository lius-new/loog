use std::collections::HashMap;

use accesskit::{Action, ActionData, Node, NodeId, Rect, Role, Toggled, Tree, TreeId, TreeUpdate};
use accesskit_winit::Adapter;
use winit::{
    event_loop::{ActiveEventLoop, EventLoopProxy},
    window::Window,
};

use lgui_core::core::{
    SemanticAction, SemanticInput, SemanticNode, SemanticRole, SemanticUpdate, UiId, UiScale,
};

use crate::WinitUserEvent;

pub(super) struct AccessibilityState {
    adapter: Adapter,
    ids: SemanticIds,
    needs_full: bool,
}

impl AccessibilityState {
    pub(super) fn new(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<WinitUserEvent>,
    ) -> Self {
        Self {
            adapter: Adapter::with_event_loop_proxy(event_loop, window, proxy),
            ids: SemanticIds::default(),
            needs_full: true,
        }
    }

    pub(super) fn process_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        self.adapter.process_event(window, event);
    }

    pub(super) fn request_full(&mut self) {
        self.needs_full = true;
    }

    pub(super) fn publish(
        &mut self,
        update: &SemanticUpdate,
        tree: &lgui_core::core::HostTree,
        focus: Option<UiId>,
        scale: UiScale,
    ) {
        let full;
        let update = if self.needs_full {
            full = lgui_core::backend::full_semantic_update(tree, focus);
            &full
        } else {
            update
        };
        let tree_update = self.ids.tree_update(update, scale);
        self.adapter.update_if_active(move || tree_update);
        self.needs_full = false;
    }

    pub(super) fn resolve(&self, id: NodeId) -> Option<UiId> {
        self.ids.ui_by_access.get(&id).cloned()
    }
}

#[derive(Default)]
struct SemanticIds {
    next: u64,
    access_by_ui: HashMap<UiId, NodeId>,
    ui_by_access: HashMap<NodeId, UiId>,
    root: Option<NodeId>,
}

impl SemanticIds {
    fn id_for(&mut self, id: &UiId) -> NodeId {
        if let Some(id) = self.access_by_ui.get(id) {
            return *id;
        }
        self.next = self.next.max(1);
        let access_id = NodeId(self.next);
        self.next += 1;
        self.access_by_ui.insert(id.clone(), access_id);
        self.ui_by_access.insert(access_id, id.clone());
        access_id
    }

    fn tree_update(&mut self, update: &SemanticUpdate, scale: UiScale) -> TreeUpdate {
        if self.root.is_none() {
            if let Some(root) = update.nodes.iter().find(|node| node.parent.is_none()) {
                self.root = Some(self.id_for(&root.id));
            }
        }
        let root = self.root.unwrap_or(NodeId(0));
        let nodes = update
            .nodes
            .iter()
            .map(|semantic| {
                let id = self.id_for(&semantic.id);
                (id, self.access_node(semantic, scale))
            })
            .collect();
        let focus = update
            .focus
            .as_ref()
            .map(|id| self.id_for(id))
            .unwrap_or(root);
        TreeUpdate {
            nodes,
            tree: update.full.then(|| Tree::new(root)),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    fn access_node(&mut self, semantic: &SemanticNode, scale: UiScale) -> Node {
        let value = &semantic.semantics;
        let mut node = Node::new(role(value.role));
        if let Some(name) = &value.name {
            node.set_label(name.clone());
        }
        if let Some(description) = &value.description {
            node.set_description(description.clone());
        }
        if let Some(text) = &value.value {
            node.set_value(text.clone());
        }
        if let Some(numeric) = value.numeric_value {
            node.set_numeric_value(numeric);
        }
        if let Some(minimum) = value.numeric_min {
            node.set_min_numeric_value(minimum);
        }
        if let Some(maximum) = value.numeric_max {
            node.set_max_numeric_value(maximum);
        }
        if let Some(step) = value.numeric_step {
            node.set_numeric_value_step(step);
        }
        if value.state.disabled {
            node.set_disabled();
        }
        if value.state.selected {
            node.set_selected(true);
        }
        if value.state.read_only {
            node.set_read_only();
        }
        if value.state.required {
            node.set_required();
        }
        if value.state.busy {
            node.set_busy();
        }
        if value.state.hidden {
            node.set_hidden();
        }
        if let Some(checked) = value.state.checked {
            node.set_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            });
        }
        if let Some(expanded) = value.state.expanded {
            node.set_expanded(expanded);
        }
        for action in &value.actions {
            node.add_action(access_action(*action));
        }
        node.set_children(
            semantic
                .children
                .iter()
                .map(|id| self.id_for(id))
                .collect::<Vec<_>>(),
        );
        if !value.relationships.labelled_by.is_empty() {
            node.set_labelled_by(
                value
                    .relationships
                    .labelled_by
                    .iter()
                    .map(|id| self.id_for(id))
                    .collect::<Vec<_>>(),
            );
        }
        if !value.relationships.described_by.is_empty() {
            node.set_described_by(
                value
                    .relationships
                    .described_by
                    .iter()
                    .map(|id| self.id_for(id))
                    .collect::<Vec<_>>(),
            );
        }
        if !value.relationships.controls.is_empty() {
            node.set_controls(
                value
                    .relationships
                    .controls
                    .iter()
                    .map(|id| self.id_for(id))
                    .collect::<Vec<_>>(),
            );
        }
        if !value.relationships.owns.is_empty() {
            node.set_owns(
                value
                    .relationships
                    .owns
                    .iter()
                    .map(|id| self.id_for(id))
                    .collect::<Vec<_>>(),
            );
        }
        let bounds = scale.physical_rect_outward(semantic.bounds);
        node.set_bounds(Rect {
            x0: bounds.left as f64,
            y0: bounds.top as f64,
            x1: bounds.right as f64,
            y1: bounds.bottom as f64,
        });
        node
    }
}

pub(super) fn semantic_input(
    action: accesskit::ActionRequest,
    target: UiId,
) -> Option<SemanticInput> {
    match action.action {
        Action::Click => Some(SemanticInput::Click(target)),
        Action::Focus => Some(SemanticInput::Focus(target)),
        Action::Blur => Some(SemanticInput::Blur(target)),
        Action::SetValue => match action.data {
            Some(ActionData::Value(value)) => Some(SemanticInput::SetValue {
                target,
                value: value.into(),
            }),
            Some(ActionData::NumericValue(value)) => Some(SemanticInput::SetValue {
                target,
                value: value.to_string(),
            }),
            _ => None,
        },
        Action::Increment => Some(SemanticInput::Action {
            target,
            action: SemanticAction::Increment,
        }),
        Action::Decrement => Some(SemanticInput::Action {
            target,
            action: SemanticAction::Decrement,
        }),
        Action::ScrollIntoView => Some(SemanticInput::Action {
            target,
            action: SemanticAction::ScrollIntoView,
        }),
        Action::ScrollUp => Some(SemanticInput::Action {
            target,
            action: SemanticAction::ScrollUp,
        }),
        Action::ScrollDown => Some(SemanticInput::Action {
            target,
            action: SemanticAction::ScrollDown,
        }),
        Action::ScrollLeft => Some(SemanticInput::Action {
            target,
            action: SemanticAction::ScrollLeft,
        }),
        Action::ScrollRight => Some(SemanticInput::Action {
            target,
            action: SemanticAction::ScrollRight,
        }),
        Action::SetTextSelection => Some(SemanticInput::Action {
            target,
            action: SemanticAction::SetTextSelection,
        }),
        _ => None,
    }
}

fn role(role: SemanticRole) -> Role {
    match role {
        SemanticRole::Generic => Role::GenericContainer,
        SemanticRole::Window => Role::Window,
        SemanticRole::Group => Role::Group,
        SemanticRole::Label => Role::Label,
        SemanticRole::Text => Role::TextRun,
        SemanticRole::Image => Role::Image,
        SemanticRole::Button => Role::Button,
        SemanticRole::Link => Role::Link,
        SemanticRole::Navigation => Role::Navigation,
        SemanticRole::List => Role::List,
        SemanticRole::ListItem => Role::ListItem,
        SemanticRole::Tree => Role::Tree,
        SemanticRole::TreeItem => Role::TreeItem,
        SemanticRole::Table => Role::Table,
        SemanticRole::Row => Role::Row,
        SemanticRole::Cell => Role::Cell,
        SemanticRole::CheckBox => Role::CheckBox,
        SemanticRole::Switch => Role::Switch,
        SemanticRole::RadioButton => Role::RadioButton,
        SemanticRole::TextInput => Role::TextInput,
        SemanticRole::PasswordInput => Role::PasswordInput,
        SemanticRole::SearchInput => Role::SearchInput,
        SemanticRole::ComboBox => Role::ComboBox,
        SemanticRole::ListBoxOption => Role::ListBoxOption,
        SemanticRole::Slider => Role::Slider,
        SemanticRole::ProgressIndicator => Role::ProgressIndicator,
        SemanticRole::Dialog => Role::Dialog,
        SemanticRole::Alert => Role::Alert,
        SemanticRole::Tab => Role::Tab,
        SemanticRole::TabList => Role::TabList,
        SemanticRole::TabPanel => Role::TabPanel,
        SemanticRole::Menu => Role::Menu,
        SemanticRole::MenuItem => Role::MenuItem,
        SemanticRole::ScrollView => Role::ScrollView,
        SemanticRole::Custom => Role::Unknown,
    }
}

fn access_action(action: SemanticAction) -> Action {
    match action {
        SemanticAction::Click => Action::Click,
        SemanticAction::Focus => Action::Focus,
        SemanticAction::Blur => Action::Blur,
        SemanticAction::SetValue => Action::SetValue,
        SemanticAction::Increment => Action::Increment,
        SemanticAction::Decrement => Action::Decrement,
        SemanticAction::ScrollIntoView => Action::ScrollIntoView,
        SemanticAction::ScrollUp => Action::ScrollUp,
        SemanticAction::ScrollDown => Action::ScrollDown,
        SemanticAction::ScrollLeft => Action::ScrollLeft,
        SemanticAction::ScrollRight => Action::ScrollRight,
        SemanticAction::SetTextSelection => Action::SetTextSelection,
    }
}
