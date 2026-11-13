//! Native terminal consumer of provider capabilities and generic project preferences.
use super::{context::AppContext, extensions::host::ExtensionHost};
use crate::features::terminal::{TerminalSize, model::ProvidedTerminal};
use crate::infrastructure::terminal::transport::{Event, Sink, Transport};
use loog_workspace_api::{
    Answer, Capability, Failure, Notification, Operation, RequestId, ScopeId,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};

const INPUT_BUDGET: usize = 256 * 1024;
const INPUT_CHUNK: usize = 16 * 1024;

#[derive(Clone)]
struct Route {
    host: Weak<ExtensionHost>,
    scope: ScopeId,
    handle: Option<u64>,
}
#[derive(Default)]
struct State {
    generation: u64,
    route: Option<Route>,
    request: Option<RequestId>,
    sink: Option<Sink>,
    input: VecDeque<u8>,
    credit: usize,
    size: Option<TerminalSize>,
    sent_size: Option<TerminalSize>,
    scheduled: bool,
}
struct Terminal {
    context: AppContext,
    target: ProvidedTerminal,
    state: Mutex<State>,
}
#[derive(Clone)]
struct Adapter(Arc<Terminal>);

pub(crate) fn transport(context: AppContext, target: ProvidedTerminal) -> Arc<dyn Transport> {
    Arc::new(Adapter(Arc::new(Terminal {
        context,
        target,
        state: Mutex::new(State::default()),
    })))
}

impl Adapter {
    fn post(&self, action: impl FnOnce(Adapter) + Send + 'static) -> bool {
        let Some(application) = self
            .0
            .context
            .services()
            .and_then(|runtime| runtime.application())
        else {
            return false;
        };
        let this = self.clone();
        application.post(move || action(this));
        true
    }
    fn stop_now(&self) {
        let (route, request) = {
            let mut state = self.0.state.lock().unwrap();
            let generation = state.generation + 1;
            let previous = std::mem::replace(
                &mut *state,
                State {
                    generation,
                    ..State::default()
                },
            );
            (previous.route, previous.request)
        };
        if let Some(route) = route
            && let Some(host) = route.host.upgrade()
        {
            if let Some(request) = request {
                host.cancel_resource(request);
            }
            if let Some(handle) = route.handle {
                host.forget_resource_handle(route.scope, Capability::Terminal, handle);
                let _ = host.request_resource(
                    route.scope,
                    Operation::TerminalClose { terminal: handle },
                    Box::new(|_, _| {}),
                );
            }
        }
    }
    fn fail(&self, generation: u64, message: String) {
        let sink = {
            let state = self.0.state.lock().unwrap();
            if state.generation != generation {
                return;
            }
            state.sink.clone()
        };
        self.stop_now();
        if let Some(sink) = sink {
            sink.send(Event::Failed(message));
        }
    }
    fn begin(&self, size: TerminalSize, sink: Sink) {
        self.stop_now();
        let app = self.0.context.get();
        let scope = app
            .explorer
            .resources
            .roots
            .iter()
            .find(|root| root.descriptor.resource == self.0.target.root)
            .filter(|root| root.descriptor.capabilities.contains(&Capability::Terminal))
            .map(|root| root.descriptor.scope);
        let host = self
            .0
            .context
            .services()
            .and_then(|runtime| runtime.extensions());
        let (Some(scope), Some(host)) = (scope, host) else {
            sink.send(Event::Failed(
                "Connect the resource environment before starting its terminal".into(),
            ));
            return;
        };
        let route = Route {
            host: Arc::downgrade(&host),
            scope,
            handle: None,
        };
        let generation = {
            let mut state = self.0.state.lock().unwrap();
            state.route = Some(route.clone());
            state.sink = Some(sink);
            state.size = Some(size);
            state.sent_size = Some(size);
            state.generation
        };
        drop(app);
        let this = self.clone();
        self.0.context.update(|app| {
            super::resource_project::read(
                app,
                &self.0.context,
                scope,
                Box::new(move |app, result| {
                    if this.0.state.lock().unwrap().generation != generation {
                        return;
                    }
                    if let Err(error) = result {
                        this.fail(generation, error);
                        return;
                    }
                    let shell = super::resource_project::project(app, &this.0.target.root)
                        .and_then(|settings| settings.overrides.get("default_shell"))
                        .and_then(|value| serde_json::from_value(value.clone()).ok());
                    let program = match super::resource_project::shell_program(shell) {
                        Ok(program) => program,
                        Err(error) => {
                            this.fail(generation, error.into());
                            return;
                        }
                    };
                    this.launch(generation, route, size, program);
                }),
            )
        });
    }
    fn launch(&self, generation: u64, route: Route, size: TerminalSize, program: Option<String>) {
        let Some(host) = route.host.upgrade() else {
            self.fail(generation, "Terminal provider stopped".into());
            return;
        };
        let scope = route.scope;
        let this = self.clone();
        let operation = Operation::TerminalOpen {
            cwd: self.0.target.cwd.clone(),
            program,
            args: vec![],
            cols: size.cols,
            rows: size.rows,
        };
        match host.request_resource(
            scope,
            operation,
            Box::new(move |_, result| this.opened(generation, route, result)),
        ) {
            Ok(request) => self.0.state.lock().unwrap().request = Some(request),
            Err(error) => self.fail(generation, error.message),
        }
    }
    fn opened(&self, generation: u64, mut route: Route, result: Result<Answer, Failure>) {
        let handle = match result {
            Ok(Answer::Started { handle }) => handle,
            Err(error) => {
                self.fail(generation, error.message);
                return;
            }
            _ => {
                self.fail(
                    generation,
                    "Provider returned an invalid terminal result".into(),
                );
                return;
            }
        };
        let Some(host) = route.host.upgrade() else {
            self.fail(generation, "Terminal provider stopped".into());
            return;
        };
        let sink = {
            let mut state = self.0.state.lock().unwrap();
            if state.generation != generation {
                drop(state);
                let _ = host.request_resource(
                    route.scope,
                    Operation::TerminalClose { terminal: handle },
                    Box::new(|_, _| {}),
                );
                return;
            }
            route.handle = Some(handle);
            state.route = Some(route.clone());
            state.request = None;
            state.sink.clone()
        };
        let weak = Arc::downgrade(&self.0);
        host.observe_resource_handle(
            route.scope,
            Capability::Terminal,
            handle,
            Arc::new(move |event| {
                if let Some(inner) = weak.upgrade() {
                    Adapter(inner).notify(generation, event);
                }
            }),
        );
        if let Some(sink) = sink {
            sink.send(Event::Started);
        }
        self.pump();
    }
    fn notify(&self, generation: u64, event: Result<Notification, Failure>) {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.fail(generation, error.message);
                return;
            }
        };
        let sink = {
            let mut state = self.0.state.lock().unwrap();
            if state.generation != generation {
                return;
            }
            if let Notification::TerminalWritable { bytes, .. } = &event {
                let Some(credit) = state
                    .credit
                    .checked_add(*bytes as usize)
                    .filter(|credit| *credit <= INPUT_BUDGET)
                else {
                    drop(state);
                    self.fail(
                        generation,
                        "Provider terminal input credit exceeds the budget".into(),
                    );
                    return;
                };
                state.credit = credit;
            }
            state.sink.clone()
        };
        match event {
            Notification::TerminalDirectory { path, .. } => {
                if let Some(sink) = sink {
                    sink.send(Event::Directory(path));
                }
            }
            Notification::TerminalOutput { bytes, .. } => {
                if let Some(sink) = sink {
                    sink.send(Event::Output(bytes));
                }
            }
            Notification::TerminalExited { code, .. } => {
                self.stop_now();
                if let Some(sink) = sink {
                    sink.send(Event::Exited(code));
                }
            }
            Notification::TerminalWritable { .. } => self.pump(),
            _ => {}
        }
    }
    fn schedule(&self) {
        {
            let mut state = self.0.state.lock().unwrap();
            if state.scheduled {
                return;
            }
            state.scheduled = true;
        }
        self.post(|this| {
            this.0.state.lock().unwrap().scheduled = false;
            this.pump();
        });
    }
    fn pump(&self) {
        let (generation, route, operation) = {
            let mut state = self.0.state.lock().unwrap();
            if state.request.is_some() {
                return;
            }
            let Some(route) = state.route.clone() else {
                return;
            };
            let Some(handle) = route.handle else { return };
            let operation = if state.size != state.sent_size {
                let Some(size) = state.size else { return };
                state.sent_size = Some(size);
                Operation::TerminalResize {
                    terminal: handle,
                    cols: size.cols,
                    rows: size.rows,
                }
            } else {
                let count = state.input.len().min(state.credit).min(INPUT_CHUNK);
                if count == 0 {
                    return;
                }
                state.credit -= count;
                Operation::TerminalInput {
                    terminal: handle,
                    bytes: state.input.drain(..count).collect(),
                }
            };
            (state.generation, route, operation)
        };
        let Some(host) = route.host.upgrade() else {
            self.fail(generation, "Terminal provider stopped".into());
            return;
        };
        let this = self.clone();
        match host.request_resource(
            route.scope,
            operation,
            Box::new(move |_, result| {
                {
                    let mut state = this.0.state.lock().unwrap();
                    if state.generation != generation {
                        return;
                    }
                    state.request = None;
                }
                match result {
                    Ok(Answer::Ok) => this.pump(),
                    Err(error) => this.fail(
                        generation,
                        format!(
                            "Terminal operation failed; input was not retried: {}",
                            error.message
                        ),
                    ),
                    _ => this.fail(
                        generation,
                        "Provider returned an invalid terminal acknowledgement".into(),
                    ),
                }
            }),
        ) {
            Ok(request) => self.0.state.lock().unwrap().request = Some(request),
            Err(error) => self.fail(generation, error.message),
        }
    }
}

impl Transport for Adapter {
    fn start(&self, size: TerminalSize, sink: Sink) {
        let fallback = sink.clone();
        if !self.post(move |this| this.begin(size, sink)) {
            fallback.send(Event::Failed("Application runtime is unavailable".into()));
        }
    }
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        {
            let mut state = self.0.state.lock().unwrap();
            if state.sink.is_none() {
                return Err("Terminal is not running".into());
            }
            if state.input.len().saturating_add(bytes.len()) > INPUT_BUDGET {
                return Err("Terminal input is full; wait before pasting more text".into());
            }
            state.input.extend(bytes);
        }
        self.schedule();
        Ok(())
    }
    fn resize(&self, size: TerminalSize) {
        self.0.state.lock().unwrap().size = Some(size);
        self.schedule();
    }
    fn stop(&self) {
        self.post(|this| this.stop_now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_terminal_input_is_bounded_and_discarded_when_stopped() {
        let ui = crate::app::test_support::SavingUi::new(crate::app::state::AppState::new());
        let target = ProvidedTerminal {
            root: loog_workspace_api::ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("example.provider", "one").unwrap(),
                "/work",
            )
            .unwrap(),
            cwd: loog_workspace_api::ProviderPath::new("/work").unwrap(),
            name: "Example".into(),
        };
        let adapter = Adapter(Arc::new(Terminal {
            context: ui.state.clone(),
            target,
            state: Mutex::new(State::default()),
        }));
        assert!(adapter.write(b"not started").is_err());
        adapter.0.state.lock().unwrap().sink = Some(Sink(Arc::new(|_| {})));
        adapter.write(&vec![0; INPUT_BUDGET]).unwrap();
        assert!(adapter.write(b"overflow").is_err());
        assert_eq!(adapter.0.state.lock().unwrap().input.len(), INPUT_BUDGET);
        adapter.stop_now();
        assert!(adapter.0.state.lock().unwrap().input.is_empty());
        ui.tasks.run_all();
        assert!(adapter.write(b"stopped").is_err());
    }
}
