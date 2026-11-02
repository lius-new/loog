import test from 'node:test';
import {runtimeGrammarManifestErrors,hostBuildScriptErrors} from './check.mjs';
import assert from 'node:assert/strict';
import {dependencies,stateFields,structFields,violations,verificationManifestErrors} from './check.mjs';
const policy=()=>({exceptions:{},root_state_adapters:[],runtime_bridges:[],state_ownership:{}});

test('workspace, document and pane rules cannot regain I/O, UI, Git or adapter exceptions',()=>{
  for(const file of ['src/domain/workspace.rs','src/domain/document.rs','src/domain/pane_layout.rs']) {
    for(const source of ['use crate::git::DiffRequest;', 'use crate::app::workspace::Workspace;', 'use lgui::prelude::*;', 'use std::{fs, thread};', 'pub struct View { area: UiRect }', 'include!("../model/workspace.rs");']) {
      const declaration=file==='src/domain/workspace.rs'?'pub struct Workspace {}\n':'';
      assert.ok(violations({[file]:declaration+source},policy()).length>0);
    }
    const p=policy();p.exceptions[file]={dependencies:['lgui'],remove_in:'R8',reason:'bad'};
    const declaration=file==='src/domain/workspace.rs'?'pub struct Workspace {}\n':'';
    assert.ok(violations({[file]:declaration+'use lgui::prelude::*;'},p).some(e=>e.includes('domain cannot have migration exceptions')));
  }
});
test('workspace fields cannot evade their ownership register',()=>{
  const p=policy();p.workspace_core_ownership={gone:{owner:'domain/workspace',migrated_in:'R4'}};
  const errors=violations({'src/domain/workspace.rs':'pub struct Workspace<D, K> { pub docs: D, keys: K }'},p);
  assert.ok(errors.some(e=>e.includes('Workspace.docs: no registered owner')));
  assert.ok(errors.some(e=>e.includes('Workspace.gone: obsolete ownership entry')));
});
test('domain verification cannot silently include the application or an I/O adapter',()=>{
  const file='crates/workspace-tests/src/isolated.rs';
  for(const source of ['use lgui::ApplicationHandle;', '#[path = "../../../src/app/mod.rs"] mod app;', 'include!("../../../src/model/workspace.rs");']) {
    assert.ok(violations({[file]:source},policy()).length>0);
  }
  assert.deepEqual(violations({[file]:'#[path = "../../../src/domain/mod.rs"]\npub(crate) mod domain;'},policy()),[]);
});
test('domain verification cannot acquire GUI dependencies or renamed package aliases',()=>{
  const source='[dependencies]\nloom-editor-core = { path = "../editor-core" }\nserde = "1"\nserde_json = "1"\n';
  assert.deepEqual(verificationManifestErrors(source),[]);
  for(const modified of [source+'lgui = "1"\n',source+'[build-dependencies]\nlgui = "1"\n',source.replace('serde = "1"','serde = { package = "lgui", version = "1" }')])assert.ok(verificationManifestErrors(modified).length>0);
});

test('Editor views cannot regain root state, model, Vim machinery or exceptions',()=>{
  for(const file of ['src/ui/editor/view.rs','src/ui/editor/menu.rs','src/ui/editor_binding.rs']) {
    for(const source of ['use crate::app::state::AppState;', 'use crate::model::workspace::Workspace;', 'pub struct Scene { vim: Vim }', 'use crate::state::Root as Alias;', 'include!("../../app/state.rs");']) {
      assert.ok(violations({[file]:source},policy()).some(e=>e.includes('Editor view')));
    }
    const p=policy();p.exceptions[file]={dependencies:['crate::state'],reason:'bad',remove_in:'R8'};
    assert.ok(violations({[file]:'use crate::state::Root;'},p).some(e=>e.includes('cannot have migration exceptions')));
  }
});
test('nested editor fields still require ownership and cannot embed runtime services',()=>{
  const file='src/app/state/editor.rs',source='pub struct EditorState { pub loader: Option<FileLoader>, pub hover: Option<(u32, u32)>, }';
  assert.deepEqual(structFields(source,'EditorState'),['hover','loader']);
  const errors=violations({[file]:source},policy());
  assert.ok(errors.some(e=>e.includes('EditorState.loader: no registered owner')));
  assert.ok(errors.some(e=>e.includes('runtime service')));
  const p=policy();p.editor_state_ownership={gone:{owner:'app/state/editor',migrated_in:'R3'}};
  assert.ok(violations({[file]:'pub struct EditorState {}'},p).some(e=>e.includes('obsolete ownership')));
});

test('new reverse dependencies fail, including grouped imports',()=>{
  for(const source of ['use crate::ui::toast;', 'use crate::{app, ui::{toast}};', 'use lgui::prelude::*;','use std::fs;', 'use std::{fs, thread};', 'use lgui;', 'use unknown_external::Thing;']) {
    assert.ok(violations({'src/domain/new.rs':source},policy()).length>0);
  }
});
test('comments and strings do not masquerade as imports',()=>{
  assert.deepEqual(dependencies('// crate::app\n/* nested /* crate::ui */ comment */ const S: &str = r###"crate::state"###; let s = "lgui::Fake"; use crate::domain::notification;'),['crate::domain']);
});
test('existing exception does not authorize another source file',()=>{
  const p=policy();p.exceptions['src/ui/old.rs']={dependencies:['crate::state'],reason:'legacy',remove_in:'R8'};
  assert.ok(violations({'src/ui/old.rs':'use crate::state::A;','src/ui/new.rs':'use crate::state::A;'},p).some(e=>e.includes('new.rs')));
});
test('new root fields and runtime service fields fail',()=>{
  const source='pub struct AppState {\n pub loader: Option<FileLoader>,\n}\nimpl AppState {}';
  assert.deepEqual(stateFields(source),['loader']);
  const errors=violations({'src/app/state.rs':source},policy());
  assert.ok(errors.some(e=>e.includes('no registered owner')));
  assert.ok(errors.some(e=>e.includes('runtime service')));
});
test('new root view adapters and runtime bridges fail',()=>{
  assert.ok(violations({'src/ui/new.rs':'fn render(s: State<AppState>) { s.runtime(); }'},policy()).length>=2);
});
test('nested search snapshots cannot regain a runtime cancellation handle',()=>{
  const p=policy();p.exceptions['src/search_actions.rs']={dependencies:[],reason:'legacy',remove_in:'R2'};
  assert.ok(violations({'src/search_actions.rs':'pub struct SearchUiState { pub cancel: Option<Arc<AtomicBool>>, }\nimpl SearchUiState {}'},p).some(e=>e.includes('SearchState embeds')));
});
test('obsolete exceptions fail so debt cannot silently persist',()=>{
  const p=policy();p.exceptions['src/ui/old.rs']={dependencies:['crate::state'],reason:'legacy',remove_in:'R8'};
  assert.ok(violations({'src/ui/old.rs':'use crate::domain::notification;'},p).some(e=>e.includes('unused exception')));
});

test('Search feature and view cannot regain root or framework dependencies',()=>{
  for(const file of ['src/features/search/model.rs','src/ui/search/view.rs']) {
    assert.ok(violations({[file]:'use crate::app::state::AppState;'},policy()).length>0);
  }
  assert.ok(violations({'src/features/search/service.rs':'use std::fs;'},policy()).length>0);
});
test('Search does not accept legacy exceptions',()=>{
  const p=policy();p.exceptions['src/features/search/model.rs']={dependencies:['crate::state'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({'src/features/search/model.rs':'use crate::state::Root;'},p).some(e=>e.includes('cannot have migration exceptions')));
});
test('Workspace cannot regain Search-specific snapshot contracts',()=>{
  const p=policy();p.exceptions['src/model/workspace.rs']={dependencies:['crate::features'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({'src/model/workspace.rs':'use crate::features::search::BufferSnapshot;'},p).some(e=>e.includes('generic snapshots')));
});

test('the independent editing core rejects framework, application and I/O dependencies',()=>{
  for(const source of ['use lgui::core::*;', 'use crate::app::state::AppState;', 'use crate::model::workspace::Workspace;', 'use std::{fs, thread};', 'use std::process;', 'use conpty::Process;', 'use unknown_dependency::Thing;']) {
    assert.ok(violations({'crates/editor-core/src/new.rs':source},policy()).length>0);
  }
  assert.deepEqual(violations({'crates/editor-core/src/normal.rs':'use crate::text::EditorMut; use unicode_segmentation::UnicodeSegmentation;'},policy()),[]);
  assert.deepEqual(violations({'src/domain/identity.rs':'pub use loom_editor_core::identity::{FileId, PaneId};'},policy()),[]);
  assert.deepEqual(violations({'crates/editor-core/tests/host.rs':'use loom_editor_core::text::TextBuffer;'},policy()),[]);
});
test('the independent editing core cannot bypass its boundary with exceptions or source inclusion',()=>{
  const file='crates/editor-core/src/escape.rs',p=policy();
  p.exceptions[file]={dependencies:['lgui'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({[file]:'use lgui::ApplicationHandle;'},p).some(e=>e.includes('cannot have migration exceptions')));
  for(const source of ['#[path = "../../../src/app/state.rs"] mod root;', 'include!("../../../src/app/state.rs");']) {
    assert.ok(violations({[file]:source},policy()).some(e=>e.includes('crate boundary')));
  }
});
test('Explorer rules cannot regain root state, GUI, I/O or exceptions',()=>{
  for(const source of ['use crate::state::AppState;','use lgui::prelude::*;','use std::fs;','use std::{process, thread};','use crate::infrastructure::explorer_filesystem::ExplorerFilesystem;','use crate::input::InputState;','pub struct Rows { input: InputState }','fn row(state: State<Tree>) {}']) {
    assert.ok(violations({'src/features/explorer/tree.rs':source},policy()).length>0,source);
  }
  assert.deepEqual(violations({'src/features/explorer/tree.rs':'use crate::features::explorer::DirEntry;\nuse std::path::Path;'},policy()),[]);
  const p=policy();p.exceptions['src/features/explorer/edit.rs']={dependencies:['crate::state'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({'src/features/explorer/edit.rs':'use crate::state::AppState;'},p).some(e=>e.includes('Explorer feature cannot have migration exceptions')));
});
test('Explorer filesystem adapter is limited to its platform services',()=>{
  const file='src/infrastructure/explorer_filesystem.rs';
  assert.deepEqual(violations({[file]:'use crate::features::explorer::DirEntry;\nuse std::fs;\nfn t(){ std::process::Command::new("x"); windows_sys::Win32::Foo; }'},policy()),[]);
  for(const source of ['use crate::state::AppState;','use lgui::prelude::*;','use std::thread;','use crate::app::explorer;'])assert.ok(violations({[file]:source},policy()).length>0,source);
});
test('Explorer state cannot evade its ownership register or expose model fields',()=>{
  const p=policy();p.explorer_ownership={roots:{owner:'features/explorer/model',migrated_in:'R5.2'},gone:{owner:'ui/explorer/state',migrated_in:'R5.2'}};
  const errors=violations({'src/features/explorer/model.rs':'pub struct ExplorerModel { roots: Vec<u8>, extra: bool }','src/ui/explorer/state.rs':'pub struct ExplorerUiState { pub scroll: f32 }'},p);
  assert.ok(errors.some(e=>e.includes('ExplorerModel.extra: no registered owner')));
  assert.ok(errors.some(e=>e.includes('ExplorerUiState.gone: obsolete ownership entry')));
  assert.ok(errors.some(e=>e.includes('ExplorerUiState.scroll: no registered owner')));
  p.explorer_ownership={roots:{owner:'features/explorer/model',migrated_in:'R5.2'}};
  assert.ok(violations({'src/features/explorer/model.rs':'pub struct ExplorerModel { pub roots: Vec<u8> }'},p).some(e=>e.includes('must stay private')));
  assert.deepEqual(violations({'src/features/explorer/model.rs':'pub struct ExplorerModel { roots: Vec<u8> }'},p),[]);
});
test('Explorer view cannot read the application root or keep exceptions',()=>{
  for(const source of ['use crate::state::AppState;','fn f(w: &Workspace) {}'])assert.ok(violations({'src/ui/explorer/view.rs':source},policy()).length>0,source);
  const p=policy();p.exceptions['src/ui/explorer/view.rs']={dependencies:['crate::state'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({'src/ui/explorer/view.rs':'use crate::state::Root;'},p).some(e=>e.includes('Explorer view cannot have migration exceptions')));
});
test('root state cannot re-export Explorer types again',()=>{
  const state='pub use crate::features::explorer::{DirEntry, ExplorerTargetKind};\npub struct AppState { pub explorer: u8 }\nimpl AppState {}';
  const p=policy();p.state_ownership={explorer:{owner:'app composition',remove_in:'composition'}};
  assert.ok(violations({'src/app/state.rs':state},p).some(e=>e.includes('must not be re-exported')));
  const clean='use crate::features::explorer::DirEntry;\npub struct AppState { pub explorer: u8 }\nimpl AppState {}';
  assert.ok(!violations({'src/app/state.rs':clean},p).some(e=>e.includes('must not be re-exported')));
});
test('Git rules cannot regain root state, GUI, processes or exceptions',()=>{
  for(const source of ['use crate::state::AppState;','use lgui::prelude::*;','use std::fs;','use std::process::Command;','fn run() { std::thread::spawn(|| {}); }','use crate::git::GitService;','use crate::infrastructure::ui_dispatch;','fn f(w: &Workspace) {}']) {
    assert.ok(violations({'src/features/git/parser.rs':source},policy()).length>0,source);
  }
  assert.deepEqual(violations({'src/features/git/snapshot.rs':'use crate::features::git::types::RepositoryId;\nuse crate::domain::line_change::LineChange;\nuse loom_editor_core::text::display_width;'},policy()),[]);
  const p=policy();p.exceptions['src/features/git/diff.rs']={dependencies:['std::fs'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({'src/features/git/diff.rs':'use std::fs;'},p).some(e=>e.includes('Git feature cannot have migration exceptions')));
});
test('Git infrastructure is limited to process, file and thread services',()=>{
  const file='src/infrastructure/git/backend.rs';
  assert.deepEqual(violations({[file]:'use crate::features::git::types::*;\nuse std::process::Command;\nuse std::fs;\nuse serde::Deserialize;\nfn f(){ std::thread::spawn(||{}); }'},policy()),[]);
  for(const source of ['use crate::state::AppState;','use lgui::prelude::*;','use crate::app::git;','use crate::ui::git;','fn f(w: &Workspace) {}'])assert.ok(violations({[file]:source},policy()).length>0,source);
  const p=policy();p.exceptions[file]={dependencies:['lgui'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({[file]:'use lgui::prelude::*;'},p).some(e=>e.includes('Git infrastructure must stay free')));
});
test('Git state cannot evade its ownership register or expose model fields',()=>{
  const p=policy();p.git_ownership={history:{owner:'features/git/model',migrated_in:'R6.3'},gone:{owner:'ui/git/state',migrated_in:'R6.3'}};
  const errors=violations({'src/features/git/model.rs':'pub struct GitModel { history: u8, extra: u8 }','src/ui/git/state.rs':'pub struct GitUiState { pub scroll: f32 }'},p);
  assert.ok(errors.some(e=>e.includes('GitModel.extra: no registered owner')));
  assert.ok(errors.some(e=>e.includes('GitUiState.gone: obsolete ownership entry')));
  p.git_ownership={history:{owner:'features/git/model',migrated_in:'R6.3'}};
  assert.ok(violations({'src/features/git/model.rs':'pub struct GitModel { pub history: u8 }'},p).some(e=>e.includes('GitModel fields must stay private')));
  assert.deepEqual(violations({'src/features/git/model.rs':'pub struct GitModel { history: u8 }'},p),[]);
});

test('Git views cannot regain root state, application commands or service access',()=>{
  for(const source of ['use crate::state::AppState;','use crate::app::git;','use crate::infrastructure::git;','fn f(w: &Workspace) {}','use crate::workspace_actions;','use std::thread;']) {
    assert.ok(violations({'src/ui/git/panel.rs':source},policy()).length>0,source);
  }
  assert.deepEqual(violations({'src/ui/git/panel.rs':'use crate::ui::components::state_access::StateAccess; use crate::features::git::GitStoreSnapshot; use lgui::prelude::Element;'},policy()),[]);
});

test('Git views cannot escape through exceptions or source inclusion',()=>{
  const file='src/ui/git/panel.rs',p=policy();p.exceptions[file]={dependencies:['crate::app'],reason:'bad',remove_in:'R8'};
  assert.ok(violations({[file]:'use crate::app::git;'},p).some(e=>e.includes('Git view cannot have migration exceptions')));
  for(const source of ['#[path = "../../app/state.rs"] mod root;', 'include!("../../app/state.rs");'])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('cannot include root sources')));
});

test('root state cannot re-export Git types again',()=>{
  const p=policy();p.state_ownership={git:{owner:'app composition',remove_in:'composition'}};
  const source='pub use crate::features::git::{GitStoreSnapshot, DiffTarget}; pub struct AppState { pub git: u8 }\nimpl AppState {}';
  assert.ok(violations({'src/app/state.rs':source},p).some(e=>e.includes('Git types must not be re-exported')));
});

test('terminal rules reject GUI, root state, runtime resources and direct I/O',()=>{
  const file='src/features/terminal/new.rs';
  for(const source of ['use lgui::core::*;','use crate::state::AppState;','use crate::terminal_session::TerminalController;','use std::{fs,process,thread};','use conpty::Process;','pub struct Runtime { value: Mutex<u32> }','use unknown_external::Thing;','use super::super::super::app::terminal_binding::TerminalTabs;','use std::io::{Read, Write};','use std::{io as stream, env};','let dir = std::env::current_dir();'])assert.ok(violations({[file]:source},policy()).length>0);
  assert.deepEqual(violations({[file]:'use serde::{Serialize, Deserialize}; use vt100::Screen; use crate::features::terminal::ShellKind;'},policy()),[]);
});
test('terminal rules cannot regain exceptions or include runtime sources',()=>{
  const file='src/features/terminal/new.rs',p=policy();
  p.exceptions[file]={dependencies:['lgui'],remove_in:'R7',reason:'bad'};
  assert.ok(violations({[file]:'use lgui::ApplicationHandle;'},p).some(e=>e.includes('cannot have migration exceptions')));
  for(const source of ['include!("../../terminal_session.rs");','#[path = "../../terminal_session.rs"] mod old;'])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('cannot include adapter sources')));
});
test('application layout cannot regain GUI, parent paths, I/O or exceptions',()=>{
  const file='src/domain/application_layout.rs';
  for(const source of ['use lgui::prelude::UiRect;','use super::pane_layout::Axis;','use std::{fs,thread};','include!("../model/application_layout.rs");'])assert.ok(violations({[file]:source},policy()).length>0);
  const p=policy();p.exceptions[file]={dependencies:['lgui'],remove_in:'R8',reason:'bad'};
  assert.ok(violations({[file]:'use lgui::prelude::*;'},p).some(e=>e.includes('domain cannot have migration exceptions')));
  assert.deepEqual(violations({[file]:'use crate::domain::{geometry::Rect, pane_layout::{Axis, Direction}};'},policy()),[]);
});

test('removed terminal and layout paths cannot return as compatibility aliases',()=>{
  for(const file of ['src/model/application_layout.rs','src/terminal_mouse.rs'])assert.ok(violations({[file]:'pub use crate::domain::application_layout::*;'},policy()).some(e=>e.includes('removed R7.1 paths')));
});

test('terminal infrastructure rejects GUI, root/application dependencies and exceptions',()=>{
 const file='src/infrastructure/terminal/session.rs';
 for(const source of ['use lgui::ApplicationHandle;','use crate::app::state::AppState;','use crate::ui::terminal;','use crate::terminal_session::TerminalTabs;','pub use crate::{app::terminal_runtime as runtime, ui::terminal};','use super::super::super::app::terminal_runtime;','include!("../../app/state.rs");','#[path = "../../app/state.rs"] mod old;'])assert.ok(violations({[file]:source},policy()).length>0);
 const p=policy();p.exceptions[file]={dependencies:['lgui'],remove_in:'R7',reason:'bad'};assert.ok(violations({[file]:'use lgui::ApplicationHandle;'},p).some(e=>e.includes('free of root state')));
 assert.deepEqual(violations({[file]:'use crate::features::terminal::ShellKind; use conpty::Process; use std::{fs,process,thread};'},policy()),[]);
});
test('removed terminal system adapters cannot return as aliases',()=>{
 for(const file of ['src/terminal_profiles.rs','src/terminal_directory.rs','src/terminal_program.rs'])assert.ok(violations({[file]:'pub use crate::infrastructure::terminal::*;'},policy()).some(e=>e.includes('removed R7.2 adapters')));
});

test('terminal model and metadata fields require private registered ownership',()=>{
 const file='src/features/terminal/model.rs',p=policy();p.terminal_model_ownership={tabs:{owner:'features/terminal/model:TerminalModel',migrated_in:'R7.3'}};
 assert.ok(violations({[file]:'pub struct TerminalModel { tabs: Vec<TerminalMetadata>, surprise: u64 }'},p).some(e=>e.includes('surprise: no registered owner')));
 assert.ok(violations({[file]:'pub struct TerminalModel { pub tabs: Vec<TerminalMetadata> }'},p).some(e=>e.includes('fields must stay private')));
 assert.ok(violations({[file]:'pub struct TerminalModel {}'},p).some(e=>e.includes('obsolete ownership')));
 p.terminal_metadata_ownership={id:{owner:'features/terminal/model:TerminalMetadata',migrated_in:'R7.3'}};
 assert.ok(violations({[file]:'pub struct TerminalMetadata { pub id: u64 }'},p).some(e=>e.includes('fields must stay private')));
});
test('terminal model cannot hide runtime resources in nested data or aliases',()=>{
 const file='src/features/terminal/model.rs';for(const source of ['type Hidden = Arc<Session>; pub struct TerminalModel { value: Hidden }','pub struct TerminalMetadata { nested: Wrapper<Weak<TerminalRuntime>> }','type Old = TerminalHandle;','use crate::infrastructure::terminal::runtime::TerminalRuntime;'])assert.ok(violations({[file]:source},policy()).length>0);
});
test('removed terminal model path cannot return or be a reexport',()=>{
 assert.ok(violations({'src/terminal_session.rs':'pub use crate::app::terminal_binding::*;'},policy()).some(e=>e.includes('removed R7.3')));
});

test('terminal app snapshots and operation handles cannot become strong owners',()=>{
 for(const [file,name,fields]of [['src/app/terminal_binding.rs','TerminalTabs','model: TerminalModel, runtime: Arc<TerminalRuntime>'],['src/app/terminal_binding.rs','TerminalTabs','model: TerminalModel, runtime: Arc<TerminalRuntime> /* runtime: Weak<TerminalRuntime> */'],['src/app/terminal_binding.rs','TerminalStore','model: State<OwnedSessions>, runtime: Weak<TerminalRuntime>'],['src/app/terminal_binding.rs','TerminalStore','model: State<TerminalModel>, runtime: Weak<TerminalRuntime>, keepalive: Arc<Session>'],['src/infrastructure/terminal/runtime.rs','TerminalHandle','runtime: Arc<TerminalRuntime>, id: u64, shell: ShellKind'],['src/app/terminal_binding.rs','TerminalTab','id:u64,group_id:u64,width_weight:f32,controller:TerminalController,cwd:Option<PathBuf>,purpose:Option<String>,running_program:Option<String>']])assert.ok(violations({[file]:'struct '+name+' { '+fields+' }'},policy()).length>0);
 assert.ok(violations({'src/app/new.rs':'use crate::terminal_session::TerminalTabs;'},policy()).some(e=>e.includes('old terminal composition')));
 assert.ok(violations({'src/main.rs':'mod terminal_session;'},policy()).some(e=>e.includes('old terminal module alias')));
});

test('terminal application commands cannot migrate back into legacy view callbacks',()=>{
  for(const file of ['src/ui/terminal.rs','src/ui/terminal_tab_context_menu.rs']) {
    for(const method of ['add_at','select','split','close','set_purpose','resize_pair','write','paste','terminate','begin_selection','extend_selection','wheel']) {
      assert.ok(violations({[file]:`fn callback() { controller.${method}(target); }`},policy()).some(e=>e.includes('through app commands')));
    }
    assert.ok(violations({[file]:'fn callback() { commands::select(&state, &tabs, id); }'},policy()).some(e=>e.includes('removed R7.6')));
  }
});

test('terminal settings and nested projection views cannot regain root or runtime dependencies',()=>{
  for(const file of ['src/ui/terminal_settings.rs','src/ui/terminal/settings_binding.rs','src/ui/terminal/state.rs']) {
    for(const source of ['use crate::app::state::AppState;', 'use crate::infrastructure::terminal::runtime::TerminalHandle;', 'use crate::key_actions;', 'pub struct View { owner: AppRuntime }']) {
      assert.ok(violations({[file]:source},policy()).some(e=>e.includes('Terminal projection view')));
    }
    const p=policy();p.exceptions[file]={dependencies:['crate::state'],reason:'bad',remove_in:'R8'};
    assert.ok(violations({[file]:'use crate::state::AppState;'},p).some(e=>e.includes('cannot have migration exceptions')));
  }
});

test('terminal layout chrome availability and draft fields remain registered by their owners',()=>{
  for(const [file,name,owner] of [
    ['src/app/state/layout.rs','ApplicationLayoutState','app/state/layout'],
    ['src/app/state/chrome.rs','TerminalChrome','app/state/chrome'],
    ['src/ui/terminal/state.rs','TerminalUiState','ui/terminal/state'],
    ['src/ui/terminal/state.rs','ProfileEditor','ui/terminal/state:ProfileEditor'],
    ['src/features/terminal/availability.rs','TerminalAvailability','features/terminal/availability'],
  ]) {
    assert.ok(violations({[file]:`pub struct ${name} { field: bool }`},policy()).some(e=>e.includes(`${name}.field: no registered owner`)));
    const p=policy();p.terminal_state_ownership={[owner+'.gone']:{owner,migrated_in:'R7.4'}};
    assert.ok(violations({[file]:`pub struct ${name} {}`},p).some(e=>e.includes(`${name}.gone: obsolete ownership entry`)));
    assert.ok(violations({[file]:`pub struct ${name} { controller: TerminalController }`},p).some(e=>e.includes('cannot own runtime services')));
  }
});

test('registered terminal owners cannot disappear by deleting declarations or source files',()=>{
  for(const [file,name,owner] of [
    ['src/app/state/layout.rs','ApplicationLayoutState','app/state/layout'],
    ['src/app/state/chrome.rs','TerminalChrome','app/state/chrome'],
    ['src/ui/terminal/state.rs','TerminalUiState','ui/terminal/state'],
    ['src/ui/terminal/state.rs','ProfileEditor','ui/terminal/state:ProfileEditor'],
    ['src/features/terminal/availability.rs','TerminalAvailability','features/terminal/availability'],
  ]) {
    const p=policy();p.terminal_state_ownership={[owner+'.field']:{owner,migrated_in:'R7.4'}};
    for(const sources of [{}, {[file]:'// declaration removed'}]) {
      assert.ok(violations(sources,p).some(e=>e.includes(name+': registered owner declaration is missing')));
    }
  }
});

test('terminal effects cannot return to the composition shell or lose notification identity',()=>{
  for(const type of ['SustainedProgram','ProgramProbe','ensure_started','save_terminal_state','DirectoryReports'])assert.ok(violations({'src/app/shell.rs':`fn effect() { ${type}(); }`},policy()).some(e=>e.includes('terminal effect behavior')));
  for(const source of ['pub struct Observation { id: u64 }','pub struct Observation { generation: u64 }'])assert.ok(violations({'src/app/terminal_effects.rs':source},policy()).some(e=>e.includes('must carry')));
  assert.deepEqual(violations({'src/app/terminal_effects.rs':'pub struct Observation { id: u64, generation: u64 }'},policy()),[]);
});

test('all terminal views reject root model infrastructure aliases and source inclusion',()=>{
  for(const file of ['src/ui/terminal/view.rs','src/ui/terminal/layout.rs','src/ui/terminal/menu.rs','src/ui/terminal/settings.rs','src/ui/terminal/bindings.rs']) {
    for(const source of ['use crate::app::state::AppState;','use crate::infrastructure::terminal::runtime::TerminalHandle;','use crate::model::workspace::Workspace;','use super::super::Root;','include!("../../app/state.rs");','#[path="../../app/state.rs"] mod hidden;'])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('Terminal projection view')));
    assert.deepEqual(violations({[file]:'use crate::features::terminal::SessionId; fn callback() { binding.run(Command::Select(id)); }'},policy()),[]);
  }
  for(const file of ['src/ui/terminal.rs','src/ui/layout_map.rs','src/ui/terminal_settings.rs','src/ui/terminal_tab_context_menu.rs'])assert.ok(violations({[file]:'fn render() {}'},policy()).some(e=>e.includes('removed R7.6')));
});

test('final terminal gate rejects old declarations and hidden native resources in views',()=>{
  for(const module of ['terminal_session','terminal_mouse','terminal_directory','terminal_profiles','terminal_program'])assert.ok(violations({'src/main.rs':`mod ${module};`},policy()).some(e=>e.includes('old terminal module alias')));
  for(const module of ['layout_map','terminal_settings','terminal_tab_context_menu'])assert.ok(violations({'src/ui/mod.rs':`pub mod ${module};`},policy()).some(e=>e.includes('old terminal UI module aliases')));
  for(const source of ['type Hidden = Arc<RunningProcess>;','pub struct View { resources: Wrapper<Registry> }','use std::{fs as files, process as native};','let cwd = std::env::current_dir();'])assert.ok(violations({'src/ui/terminal/new.rs':source},policy()).some(e=>e.includes('Terminal projection view')));
});

test('settings pure rules reject GUI native services nested resources and ancestor escapes',()=>{
  for(const source of ['use lgui::State;','use std::{fs as native};','std::env::current_dir();','struct Model { nested: Wrapper<AppRuntime> }','use super::super::state::AppState;','include!("adapter.rs");'])assert.ok(violations({'src/features/settings/fake.rs':source},policy()).length>0,source);
});
test('settings native adapters and removed root paths cannot retain application capabilities',()=>{
  for(const file of ['src/infrastructure/settings_filesystem.rs','src/infrastructure/project_settings.rs','src/infrastructure/config_watcher.rs'])assert.ok(violations({[file]:'use crate::app::state::AppState;'},policy()).length>0,file);
  for(const file of ['src/theme.rs','src/fonts.rs','src/file_icons.rs','src/jsonc.rs','src/settings_persistence.rs','src/project_settings.rs','src/config_watcher.rs','src/workspace_actions.rs'])assert.ok(violations({[file]:'fn retained() {}'},policy()).some(e=>e.includes('removed R8.1')),file);
});

test('keymap pure rules reject hidden GUI state native I/O and ancestor includes',()=>{
  for(const source of ['use lgui::KeyboardEvent;','type Alias=Wrapper<AppState>;','use std::{fs as native};','std::env::current_dir();','use super::super::state;','include!("adapter.rs");'])assert.ok(violations({'src/features/keymap/fake.rs':source},policy()).length>0,source);
});
test('settings and keymap views cannot recover root or native capabilities',()=>{
  for(const file of ['src/ui/settings.rs','src/ui/settings_binding.rs','src/ui/keymap_page.rs','src/ui/keymap_binding.rs'])for(const source of ['use crate::app::state::AppState;','struct Frame { hidden: Arc<TerminalHandle> }','std::env::current_dir();'])assert.ok(violations({[file]:source},policy()).length>0,file+source);
});

test('R8 native file and window adapters reject root ownership and composition escapes',()=>{
 for(const file of ['src/infrastructure/file_loader.rs','src/infrastructure/file_watcher.rs','src/infrastructure/document_filesystem.rs','src/infrastructure/file_dialogs.rs','src/infrastructure/window_geometry.rs','src/infrastructure/launch.rs'])for(const source of ['struct Service { hidden: Wrapper<AppState> }','use crate::app::workspace;','use super::super::state::AppState;','include!("application.rs");'])assert.ok(violations({[file]:source},policy()).length>0,file+source);
});

test('chrome views reject root runtime native access and ancestor escapes',()=>{
 for(const file of ['src/ui/tabs.rs','src/ui/diff_editor.rs','src/ui/welcome.rs','src/ui/titlebar.rs','src/ui/statusbar.rs','src/ui/tab_context_menu.rs','src/ui/close_confirmation.rs','src/ui/pane_sash.rs','src/ui/chrome/binding.rs'])for(const source of ['struct Frame { hidden: Wrapper<AppState> }','use crate::app::workspace;','use crate::model::workspace;','std::fs::read(path);','use super::super::state;','include!("application.rs");'])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('Chrome views')),file+source);
});
test('tab UI state must retain explicit field ownership',()=>{assert.ok(violations({'src/ui/chrome/tab_state.rs':'struct UiState { surprise: bool }'},policy()).some(e=>e.includes('TabUiState.surprise')));});

test('value snapshots and UI ports cannot regain runtime access through wrappers',()=>{
 for(const source of ['struct AppState { hidden: Wrapper<Weak<AppRuntime>> }','struct AppState {} impl AppState { fn attach_runtime(){} }','struct AppState { commands: AppContext }'])assert.ok(violations({'src/app/state.rs':source},policy()).some(e=>e.includes('capability-free')));
 assert.ok(violations({'src/ui/chrome/port.rs':'struct Frame { command: Wrapper<AppContext> }'},policy()).some(e=>e.includes('command contexts')));
 for(const source of ['struct AppContext { model: State<AppState>, runtime: Arc<AppRuntime> }','struct AppContext { model: State<AppState>, runtime: Weak<AppRuntime>, extra: Service }'])assert.ok(violations({'src/app/context.rs':source},policy()).some(e=>e.includes('weak runtime')));
});
test('removed R8 aliases and workspace compatibility modules cannot return',()=>{
 for(const source of ['use app::state as state;','mod input;','use app::workspace_commands as workspace_actions;'])assert.ok(violations({'src/main.rs':source},policy()).some(e=>e.includes('removed R8 root aliases')));
 assert.ok(violations({'src/model/workspace.rs':'pub use crate::app::workspace::*;'},policy()).some(e=>e.includes('removed R8 workspace')));
});
test('chrome field owners cannot silently disappear or grow',()=>{
 const p=policy();p.chrome_ownership={ChromeState:['toast']};
 for(const sources of [{},{'src/app/state/chrome.rs':'struct ChromeState {}'},{'src/app/state/chrome.rs':'struct ChromeState {toast:Toast,service:Service}'}])assert.ok(violations(sources,p).some(e=>e.includes('chrome owner')));
});
test('only the built-in language table may be included into the domain',()=>{
  const table='#[cfg(test)]\ninclude!("../../../tests/fixtures/language_registry.rs");';
  const embedded='src/domain/language/embedded.rs';
  assert.deepEqual(violations({[embedded]:'use crate::domain::language::registry::Registry;\n'+table},policy()),[]);
  for(const [file,source] of [
    [embedded,'include!("../../model/workspace.rs");'],
    [embedded,table+'\ninclude_str!("secret.txt");'],
    [embedded,'include!(concat!(env!("OUT_DIR"), "/other.rs"));'],
    ['src/domain/language/registry.rs',table],
    ['src/domain/document.rs',table],
  ])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('domain cannot include adapter sources')),`${file}: ${source}`);
});
test('the language description parser may use serde and no other domain file may',()=>{
  const source='use serde::Deserialize;';
  assert.deepEqual(violations({'src/domain/language/descriptor.rs':source},policy()),[]);
  for(const file of ['src/domain/language/registry.rs','src/domain/document.rs'])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('unregistered dependency serde')));
  assert.ok(violations({'src/domain/language/descriptor.rs':'use lgui::prelude::*;'},policy()).length>0);
  assert.ok(violations({'src/domain/language/descriptor.rs':'use std::fs;'},policy()).length>0);
});
test('domain verification cannot regain a language-source build script',()=>{
  const file='crates/workspace-tests/build.rs';
  const scanner='#[path = "../../build_support/language_assets.rs"]\nmod language_assets;';
  assert.ok(violations({[file]:scanner},policy()).length>0);
  for(const source of [
    '#[path = "../../src/app/mod.rs"]\nmod app;',
    scanner+'\n#[path = "../../src/app/mod.rs"]\nmod app;',
    'include!("../../src/model/workspace.rs");',
  ])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('can only include the production domain entry point')),source);
  assert.ok(violations({'crates/workspace-tests/src/other.rs':scanner},policy()).some(e=>e.includes('can only include the production domain entry point')));
});
test('no syntax file may name grammar crates and only the engine files use the runtime',()=>{
  const grammar='use tree_sitter::Language;';
  assert.deepEqual(violations({'src/features/syntax/grammars.rs':grammar},policy()),[]);
  assert.deepEqual(violations({'src/features/syntax/queries.rs':'use tree_sitter::Query;'},policy()),[]);
  assert.deepEqual(violations({'src/features/syntax/treesitter.rs':'use tree_sitter::Parser;\nuse streaming_iterator::StreamingIterator;'},policy()),[]);
  for(const [file,source] of [
    ['src/features/syntax/grammars.rs','fn f() { tree_sitter_rust::LANGUAGE; }'],
    ['src/features/syntax/grammars.rs','fn f() { tree_sitter_json::LANGUAGE; tree_sitter_md::LANGUAGE; }'],
    ['src/features/syntax/queries.rs','fn f() { tree_sitter_rust::LANGUAGE; }'],
    ['src/features/syntax/treesitter.rs','fn f() { tree_sitter_c_sharp::LANGUAGE; }'],
    ['src/features/syntax/lexer.rs','use tree_sitter::Parser;'],
    ['src/features/syntax/index.rs','fn f() { tree_sitter_javascript::LANGUAGE; }'],
    ['src/features/highlighting/model.rs','use tree_sitter::Query;'],
    ['src/ui/theme.rs','use tree_sitter::Query;'],
    ['src/features/syntax/queries.rs','use streaming_iterator::StreamingIterator;'],
  ])assert.ok(violations({[file]:source},policy()).some(e=>e.includes('unregistered dependency')),`${file}: ${source}`);
});
test('provider models stay pure and generic process ownership stays in infrastructure',()=>{
  assert.deepEqual(violations({'src/features/providers/request.rs':'use serde_json::Value;'},policy()),[]);
  for(const source of ['use lgui::prelude::*;','use std::fs;','use std::process::Command;','use std::thread;','use crate::infrastructure::extensions::Executor;'])assert.ok(violations({'src/features/providers/request.rs':source},policy()).length>0,source);
  assert.ok(violations({'src/features/search/new.rs':'use serde_json::Value;'},policy()).length>0,'serde_json stays out of other features');
  assert.deepEqual(violations({'src/infrastructure/process_job.rs':'use std::process::Command;\nuse windows_sys::Win32::Foundation::CloseHandle;'},policy()),[]);
  assert.ok(violations({'src/infrastructure/lsp/client.rs':'use std::process::Command;'},policy()).length>0,'retired host transport cannot return');
  for(const source of ['use lgui::prelude::*;','use crate::ui::theme;'])assert.ok(violations({'src/infrastructure/process_job.rs':source},policy()).length>0,source);
});

test('language resources stay in infrastructure and test composition cannot enter production',()=>{
  assert.deepEqual(violations({'src/infrastructure/language_packs.rs':'use libloading::Library; use std::fs; use tree_sitter::Language;'},policy()),[]);
  for(const file of ['src/features/syntax/treesitter.rs','src/features/syntax/queries.rs']) {
    for(const source of ['use std::fs;','use libloading::Library;','fn load() { libloading::Library::new("a"); }','use crate::infrastructure::language_packs;']) {
      assert.ok(violations({[file]:source},policy()).length>0, file+source);
    }
  }
  assert.ok(violations({'src/features/syntax/mod.rs':'pub(crate) mod test_support;'},policy()).length>0);
  assert.ok(violations({'src/features/syntax/mod.rs':'#[cfg(all(test, loom_language_pack_tests))]\n#[path = "../../../tests/language-runtime/suites/test_support.rs"]\npub(crate) mod test_support;'},policy()).length>0);
  assert.deepEqual(violations({'src/features/syntax/mod.rs':'mod grammars;'},policy()),[]);
});

test('host dependency sections and workspace cannot reference grammar producers',()=>{
  assert.deepEqual(runtimeGrammarManifestErrors('[dependencies]\ntree-sitter = "0.27"\ntree-sitter-language = "0.1"\n[dev-dependencies]\nloom-editor-core = { path = "crates/editor-core" }'),[]);
  for(const source of [
    '[dev-dependencies]\nloom-grammar-json = { path = "language-packs/grammars/json" }',
    '[build-dependencies]\ntree-sitter-json = "0.24"',
    '[workspace]\nmembers = ["language-packs/*"]',
    '[dependencies]\ntree-sitter-md = "0.5"',
    '[dependencies]\ntree-sitter-json = "0.24"',
    '[dependencies]\ntree-sitter-rust = "0.24"',
    '[dependencies]\nrust_alias = { package = "tree-sitter-rust", version = "0.24" }',
    '[dependencies.rust_alias]\npackage = "tree-sitter-rust"',
    '[target.\'cfg(windows)\'.dependencies]\nloom-grammar-rust = { path = "crates/language-packs/rust" }',
    '[dependencies]\nalias = { path = "crates/language-packs/rust" }',
  ]) assert.ok(runtimeGrammarManifestErrors(source).length>0,source);
});

test('host build script cannot recover language compilation or resource staging',()=>{
  assert.deepEqual(hostBuildScriptErrors('fn main() { println!("cargo:rerun-if-changed=icons/app.ico"); }'),[]);
  for(const source of ['mod language_packages;', 'mod language_assets;', 'stage_packs(root,destination);','read("language-packs/manifests");'])assert.ok(hostBuildScriptErrors(source).length>0,source);
});

test('file encoding attributes stay pure, codec is independent, and UI receives only bindings',()=>{
  assert.deepEqual(violations({'src/domain/encoding.rs':'use serde::{Serialize, Deserialize}; pub struct TextFormat { bom: bool }'},policy()),[]);
  for(const source of ['use encoding_rs::Encoding;','use chardetng::EncodingDetector;','use std::fs;','use crate::features::encoding;'])assert.ok(violations({'src/domain/encoding.rs':source},policy()).length>0);
  assert.deepEqual(violations({'src/features/encoding/mod.rs':'use encoding_rs::Encoding; use crate::domain::encoding::TextFormat;'},policy()),[]);
  for(const source of ['use std::fs;','use crate::app::state::AppState;','use lgui::ApplicationHandle;'])assert.ok(violations({'src/features/encoding/mod.rs':source},policy()).length>0);
  assert.ok(violations({'src/ui/encoding_picker.rs':'use crate::app::context::AppContext;'},policy()).length>0);
});

test('extension model and executor keep their layers apart',()=>{
  // The model has no runtime, file system or GUI; the executor has no GUI or application state.
  for(const source of ['use wasmtime::Engine;','use loom_extension_host::Runtime;','use std::fs;','use lgui::prelude::*;','use crate::app::state::AppState;'])
    assert.ok(violations({'src/features/extensions/manifest.rs':source},policy()).length>0,source);
  for(const source of ['use lgui::prelude::*;','use crate::app::state::AppState;','use wasmtime::Engine;'])
    assert.ok(violations({'src/infrastructure/extensions/mod.rs':source},policy()).length>0,source);
  assert.deepEqual(violations({'src/features/extensions/manifest.rs':'use serde::Deserialize;\nuse crate::features::extensions::trust::Denial;'},policy()),[]);
  assert.deepEqual(violations({'src/infrastructure/extensions/mod.rs':'use loom_extension_host::Runtime;\nuse loom_extension_api::ErrorKind;\nuse std::thread::JoinHandle;'},policy()),[]);
});
