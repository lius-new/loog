import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Lexical gate, including cfg(test). It is intentionally not a Rust type resolver.
export function tokens(source) {
  const out=[];
  for(let i=0;i<source.length;) {
    if(/\s/.test(source[i])) {i++;continue;}
    if(source.startsWith('//',i)) {i=source.indexOf('\n',i);if(i<0)break;continue;}
    if(source.startsWith('/*',i)) {
      i+=2;let depth=1;
      while(i<source.length&&depth) {
        if(source.startsWith('/*',i)){depth++;i+=2;}
        else if(source.startsWith('*/',i)){depth--;i+=2;} else i++;
      }
      if(depth)throw Error('Unclosed block comment');
      continue;
    }
    const raw=/^(?:b|c)?r(#{0,255})"/.exec(source.slice(i));
    if(raw){const end=source.indexOf('"'+raw[1],i+raw[0].length);if(end<0)throw Error('Unclosed raw string');i=end+1+raw[1].length;continue;}
    const str=/^(?:b|c)?"/.exec(source.slice(i));
    if(str){i+=str[0].length;while(i<source.length){if(source[i]==='\\'){i+=2;continue;}if(source[i++]==='"')break;}continue;}
    const char=/^(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\])'/u.exec(source.slice(i));
    if(char){i+=char[0].length;continue;}
    const word=/^[a-zA-Z_][a-zA-Z_0-9]*/.exec(source.slice(i));
    if(word){out.push(word[0]);i+=word[0].length;continue;}
    if(source.startsWith('::',i)){out.push('::');i+=2;continue;}
    out.push(source[i++]);
  }
  return out;
}

export function dependencies(source) {
  const ts=tokens(source), found=new Set();
  for(let i=0;i<ts.length;i++) {
    if(ts[i]==='use'&&/^[a-zA-Z_]\w*$/.test(ts[i+1]??'')&&!['crate','std','super','self'].includes(ts[i+1]))found.add(ts[i+1]);
    if(ts[i]==='crate'&&ts[i+1]==='::') {
      if(ts[i+2]==='{') {
        let level=0;
        for(let j=i+2;j<ts.length;j++) {
          if(ts[j]==='{'){level++;continue;}
          if(ts[j]==='}'&&--level===0)break;
          if(level===1&&(ts[j-1]==='{'||ts[j-1]===',')&&/^[a-zA-Z_]\w*$/.test(ts[j]))found.add('crate::'+ts[j]);
        }
      } else if(/^[a-zA-Z_]\w*$/.test(ts[i+2]??''))found.add('crate::'+ts[i+2]);
    }
    if(['lgui','image','notify','conpty','portable_pty','windows_sys','libc','libloading','resvg'].includes(ts[i])&&ts[i+1]==='::')found.add(ts[i]);
    if(/^tree_sitter\w*$/.test(ts[i])&&ts[i+1]==='::')found.add(ts[i]);
    if(ts[i]==='std'&&ts[i+1]==='::'&&['fs','process','thread','net'].includes(ts[i+2]))found.add('std::'+ts[i+2]);
    if(ts[i]==='std'&&ts[i+1]==='::'&&ts[i+2]==='{') {
      let depth=1;
      for(let j=i+3;j<ts.length&&depth;j++) {
        if(ts[j]==='{')depth++;
        else if(ts[j]==='}')depth--;
        else if(depth===1&&['fs','process','thread','net'].includes(ts[j])&&(ts[j-1]==='{'||ts[j-1]===','))found.add('std::'+ts[j]);
      }
    }
  }
  return [...found].sort();
}


function terminalStandardServices(ts) {
  const forbidden=['io','env'];
  for(let i=0;i<ts.length;i++) {
    if(ts[i]!=='std'||ts[i+1]!=='::')continue;
    if(forbidden.includes(ts[i+2]))return true;
    if(ts[i+2]!=='{')continue;
    let depth=1;
    for(let j=i+3;j<ts.length&&depth;j++) {
      if(ts[j]==='{')depth++;
      else if(ts[j]==='}')depth--;
      else if(depth===1&&forbidden.includes(ts[j])&&(ts[j-1]==='{'||ts[j-1]===','))return true;
    }
  }
  return false;
}

export function layer(file) {
  if(file.startsWith('crates/workspace-tests/'))return 'domain-verification';
  if(file.startsWith('crates/editor-core/'))return 'core';
  for(const l of ['app','domain','features','ui','infrastructure'])if(file.startsWith('src/'+l+'/'))return l;
  return 'legacy';
}

export function allowed(file, dependency) {
  const own=layer(file), root=dependency.replace('crate::','');
  if(own==='domain-verification')return ['domain','loom_editor_core','serde','serde_json'].includes(root);
  if(file.startsWith('crates/editor-core/tests/')&&root==='loom_editor_core')return true;
  if(own==='core')return ['identity','text','normal','commands','vim','buffer','change','editor','history','lines','selection','command','ex','exec','insert','key','motion','object','register','search','regex','unicode_segmentation','unicode_width'].includes(root);
  if(root==='loom_editor_core')return true;
  if(own==='app'||file==='src/main.rs')return true;
  if(own==='domain')return root==='domain'||(['src/domain/encoding.rs','src/domain/workspace_session.rs','src/domain/window_geometry.rs','src/domain/language/descriptor.rs'].includes(file)&&['serde','serde_json'].includes(root));
  if(file==='src/features/markdown.rs'||file.startsWith('src/features/markdown/'))return ['features','domain','unicode_segmentation'].includes(root)||(file==='src/features/markdown/parser.rs'&&root==='pulldown_cmark');
  if(file.startsWith('src/features/keymap/'))return ['features','domain','serde_json','input','Movement'].includes(root);
  if(file.startsWith('src/infrastructure/lsp/'))return ['infrastructure','features','domain','serde_json','std::process','std::thread','requests'].includes(root)||((file==='src/infrastructure/lsp/client_tests.rs'||file==='src/infrastructure/lsp/manager_tests.rs')&&['std::fs','ring','tar','flate2'].includes(root))||(file==='src/infrastructure/lsp/job.rs'&&['windows_sys','libc'].includes(root));
  if(file==='src/infrastructure/window_geometry.rs')return ['infrastructure','domain','lgui','windows_sys','std::process'].includes(root);
  if(file==='src/infrastructure/document_filesystem.rs')return ['infrastructure','features','domain','std::fs','std::process','windows_sys'].includes(root);
  if(file.startsWith('src/features/encoding/'))return ['features','domain','encoding_rs','chardetng'].includes(root);
  if(file==='src/infrastructure/editorconfig.rs')return ['infrastructure','domain','std::fs','globset','loom_editor_core'].includes(root);
  if(file==='src/infrastructure/edit_journal.rs')return ['infrastructure','std::fs','std::process'].includes(root);
  if(file==='src/infrastructure/file_watcher.rs')return ['infrastructure','notify','std::fs','std::thread'].includes(root);
  if(file==='src/infrastructure/query_watcher.rs')return ['infrastructure','notify','std::fs','std::thread'].includes(root);
  if(file==='src/infrastructure/lsp_watch.rs')return ['infrastructure','features','serde_json','std::fs','std::thread'].includes(root);
  if(file==='src/features/language_service/watch.rs'&&root==='regex')return true;
  if(file==='src/ui/theme_package.rs'&&root==='serde_json')return true;
  if(file==='src/features/language_service/snippet/transform.rs'&&root==='fancy_regex')return true;
  if(file==='src/infrastructure/file_drop.rs')return ['infrastructure','domain','lgui','windows_sys'].includes(root);
  if(file==='src/infrastructure/syntax_worker.rs'||file==='src/infrastructure/syntax_worker_tests.rs'||file==='src/infrastructure/syntax_worker_bench.rs'||file==='src/infrastructure/syntax_worker_retained.rs')return ['infrastructure','features','domain','std::thread'].includes(root);
  if(file==='src/infrastructure/language_packs.rs'||file==='src/infrastructure/language_packs_unit_tests.rs'||file==='src/infrastructure/language_queries_tests.rs'||file==='src/infrastructure/language_standard.rs')return ['infrastructure','features','domain','libloading','serde','serde_json','tree_sitter','tree_sitter_language','std::fs'].includes(root)||(file.endsWith('_tests.rs')&&root==='std::process');
  if(file==='src/infrastructure/language_catalog.rs'||file==='src/infrastructure/language_catalog_tests.rs')return ['infrastructure','features','domain','ring','serde','serde_json','toml','tar','flate2','ureq','std::fs','std::thread','std::process','loom_editor_core'].includes(root);
  if(file==='src/infrastructure/lsp_install.rs'||file==='src/infrastructure/lsp_install_tests.rs')return ['infrastructure','features','domain','serde_json','ring','tar','flate2','ureq','std::fs','std::thread','std::process'].includes(root);
  if(file==='src/infrastructure/lsp_catalog.rs'||file==='src/infrastructure/lsp_catalog_tests.rs')return ['infrastructure','features','domain','serde','serde_json','toml','std::fs'].includes(root);
  if(file==='src/infrastructure/keymap_filesystem.rs')return ['infrastructure','features','std::fs'].includes(root);
  if(file.startsWith('src/features/settings/'))return ['features','domain','serde','serde_json'].includes(root);
  if(['src/infrastructure/settings_filesystem.rs','src/infrastructure/project_settings.rs'].includes(file))return ['infrastructure','features','domain','std::fs'].includes(root);
  if(file==='src/infrastructure/config_watcher.rs')return ['infrastructure','notify','std::fs','std::thread'].includes(root);
  if(file.startsWith('src/features/terminal/'))return ['features','domain','serde','vt100'].includes(root);
  if(file.startsWith('src/features/explorer/'))return ['features','domain'].includes(root);
  if(file.startsWith('src/features/git/'))return ['features','domain'].includes(root);
  if(file.startsWith('src/infrastructure/extensions/'))return ['infrastructure','features','domain','loom_extension_api','loom_extension_host','std::fs','std::thread','std::process','std::os','ignore','globset','ureq','serde_json'].filter(x=>x!=='serde_json'||file.endsWith('ports.rs')).includes(root)||(file==='src/infrastructure/extensions/trust_store.rs'&&root==='std::process')||(file==='src/infrastructure/extensions/http.rs'&&root==='std::net');
  if(file.startsWith('src/features/extensions/'))return ['features','domain','serde','serde_json'].includes(root);
  if(file.startsWith('src/features/update/'))return ['features','domain','serde','serde_json'].includes(root);
  if(file==='src/infrastructure/update.rs')return ['infrastructure','features','domain','base64','ureq','ring','serde','serde_json','windows_sys','std::fs','std::process','std::thread'].includes(root);
  if(file==='src/infrastructure/explorer_filesystem.rs')return ['features','domain','std::fs','std::process','windows_sys'].includes(root);
  if(file.startsWith('src/features/search/'))return ['features','domain','globset','regex','unicode_segmentation'].includes(root);
  if(file.startsWith('src/infrastructure/search_filesystem'))return ['features','domain','ignore','std::fs','std::process'].includes(root);
  if(file.startsWith('src/infrastructure/terminal/'))return ['infrastructure','features','domain','base64','ring','vt100','conpty','portable_pty','windows_sys','libc','std::fs','std::process','std::thread'].includes(root);
  if(file.startsWith('src/infrastructure/git/'))return ['infrastructure','features','domain','serde','std::fs','std::process','std::thread'].includes(root);
  if(file==='src/infrastructure/workspace_filesystem.rs')return ['infrastructure','domain','std::fs'].includes(root);
  if(file==='src/infrastructure/session_filesystem.rs')return ['infrastructure','domain','serde','serde_json','std::fs','std::process'].includes(root);
  if(['src/ui/editor/view.rs','src/ui/components/text.rs'].includes(file)&&root==='unicode_width')return true;
  if(file==='src/ui/explorer/context_menu.rs'&&root==='unicode_width')return true;
  if(file==='src/ui/outline/view.rs'&&root==='unicode_width')return true;
  if(file==='src/features/outline/lsp.rs')return ['features','domain','serde_json'].includes(root);
  if(file==='src/infrastructure/markdown_worker.rs')return ['infrastructure','domain','features','std::thread'].includes(root);
  if(file==='src/infrastructure/markdown_resources.rs')return ['infrastructure','features','image','resvg','ureq','std::fs'].includes(root);
  if(file==='src/infrastructure/image_filesystem.rs')return ['infrastructure','domain','image','resvg','ring','std::fs','skia_safe'].includes(root);
  if(file==='src/infrastructure/gif_stream.rs')return ['infrastructure','domain','image','skia_safe'].includes(root);
  if(file==='src/ui/markdown/native.rs')return ['ui','features','lgui','skia_safe','unicode_segmentation','unicode_width'].includes(root);
  if(own==='ui')return ['ui','domain','features','theme','fonts','file_icons','unicode_segmentation'].includes(root)||dependency==='lgui';
  // Native parsers: only the grammar table names grammar crates; the engine files use the runtime.
  if(file==='src/features/syntax/grammars.rs')return ['features','domain','tree_sitter'].includes(root);
  if(file==='src/features/syntax/queries.rs'||file==='src/features/syntax/treesitter_tests.rs'||file==='src/features/syntax/treesitter_bench.rs')return ['features','domain','tree_sitter'].includes(root);
  if(['src/features/syntax/treesitter.rs','src/features/syntax/injections.rs','src/features/syntax/inspect.rs'].includes(file))return ['features','domain','tree_sitter','streaming_iterator'].includes(root);
  if(file==='src/features/syntax/layers.rs')return ['features','domain','tree_sitter'].includes(root);
  if(file.startsWith('src/features/language_service/'))return ['features','domain','serde_json'].includes(root);
  if(own==='features')return ['features','domain','input'].includes(root);
  if(own==='infrastructure')return ['infrastructure','domain'].includes(root)||dependency==='lgui'||dependency==='std::thread';
  return own==='legacy'&&root==='domain';
}

export function structFields(source, name) {
  const ts=tokens(source), start=ts.findIndex((t,i)=>t==='struct'&&ts[i+1]===name);
  if(start<0)throw Error(`Missing ${name} declaration`);
  let depth=0;const fields=[];
  for(let i=start+2;i<ts.length;i++) {
    if(ts[i]==='{'){depth++;continue;}
    if(ts[i]==='}'&&--depth===0)break;
    if(depth===1&&ts[i+1]===':'&&/^[a-zA-Z_]\w*$/.test(ts[i]))fields.push(ts[i]);
  }
  return fields.sort();
}
export function stateFields(source) { return structFields(source, 'AppState'); }

export function verificationManifestErrors(source) {
  const block=source.split(/^\[dependencies\]\s*$/m)[1]?.split(/^\[/m)[0]??'';
  const names=[...block.matchAll(/^([a-zA-Z0-9_-]+)\s*=/gm)].map(m=>m[1]).sort();
  const expected=['loom-editor-core','serde','serde_json'].sort();
  if(JSON.stringify(names)!==JSON.stringify(expected)||/^\[[^\]]*dependencies[^\]]*\]$/m.test(source.replace(/^\[dependencies\]\s*$/m,''))||/\bpackage\s*=/.test(block))return ['Workspace domain verification may depend only on editor-core, serde and serde_json'];
  return [];
}

export function violations(sources, policy) {
  const problems=[];
  for(const [file,source] of Object.entries(sources)) {
    const approved=policy.exceptions[file]?.dependencies??[];
    const deps=dependencies(source);
    for(const dep of deps)if(!allowed(file,dep)&&!approved.includes(dep))problems.push(`${file}: unregistered dependency ${dep}`);
    for(const dep of approved)if(!deps.includes(dep))problems.push(`${file}: remove unused exception ${dep}`);
    const ts=tokens(source);
    if(file==='src/features/syntax/mod.rs' && /mod\s+test_support/.test(source)) {
      problems.push('Native syntax fixtures must be composed in the independent verification workspace');
    }
    if(file.startsWith('src/') && /#\[path\s*=\s*"[^"]*tests\/language-runtime\//.test(source)) {
      problems.push(file+': host modules must not reference external native test files');
    }
    if(file.startsWith('src/')&&/#\s*\[\s*path\s*=\s*"[^"]*tests[\\/]markdown-p0[\\/]/.test(source))problems.push(file+': production sources must not import Markdown P0 prototypes');
    const markdownRules=file==='src/features/markdown.rs'||file.startsWith('src/features/markdown/');
    const markdownView=file==='src/ui/markdown.rs'||file.startsWith('src/ui/markdown/')||file==='src/ui/markdown_binding.rs';
    if(markdownRules||markdownView) {
      if(policy.exceptions[file]||terminalStandardServices(ts)||ts.some(t=>['AppState','AppContext','AppRuntime','Workspace','ApplicationHandle','FileLoader','TerminalHandle','Child','State','Mutex','RwLock'].includes(t))||deps.some(d=>['crate::app','crate::model','crate::infrastructure','std::fs','std::process','std::thread','std::net'].includes(d)))problems.push(file+': Markdown rules/views require narrow values and semantic ports without root/native capabilities');
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super')||/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Markdown modules cannot include or escape to adapter sources');
    }
    if(markdownRules&&ts.some(t=>['UiRect','UiId','FileId','PaneId','ViewId','DocumentStamp','KeyboardEvent'].includes(t)))problems.push(file+': Markdown syntax models must remain independent of GUI and workspace identities');
    if(layer(file)==='ui'&&ts.includes('AppContext'))problems.push(file+': UI cannot receive application command contexts');
    if(file==='src/app/state.rs'&&ts.some(t=>['attach_runtime','runtime','AppRuntime','AppContext','Weak'].includes(t)))problems.push(file+': AppState must remain capability-free');
    if(file==='src/app/context.rs') {const fields=structFields(source,'AppContext');if(JSON.stringify(fields)!==JSON.stringify(['model','runtime'])||!ts.join(' ').match(/runtime : Weak < AppRuntime >/))problems.push(file+': command context must carry only model and weak runtime');}
    if(file==='src/main.rs'&&ts.some((t,i)=>['state','input','background_ui','workspace_actions','workspace_persistence','session_restore','key_actions','file_loading','file_watcher','window_geometry','launch','svg_icons','vim'].includes(t)&&((ts[i-1]==='as')||(ts[i-1]==='mod'))))problems.push(file+': removed R8 root aliases');
    if(file==='src/model/workspace.rs')problems.push(file+': removed R8 workspace compatibility path');

    if(file.startsWith('src/ui/chrome/')||['welcome','workspace_home','titlebar','statusbar','tabs','tab_context_menu','close_confirmation','pane_sash','diff_editor'].some(n=>file==='src/ui/'+n+'.rs')) {
      if(policy.exceptions[file]||terminalStandardServices(ts)||ts.some(t=>['AppState','AppRuntime','ApplicationHandle','TerminalHandle','TerminalController','FileLoader'].includes(t))||deps.some(d=>['std::fs','std::process','std::thread'].includes(d))||deps.some(d=>d.startsWith('crate::app')||d.startsWith('crate::model')||d==='crate::state')||ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super')||/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>t==='include'&&ts[i+1]==='!'))problems.push(file+': Chrome views must use value frames and semantic ports');
    }
    if(file==='src/ui/chrome/tab_state.rs') {const fields=structFields(source,'UiState'),known=Object.keys(policy.tabs_ui_ownership??{});for(const f of fields)if(!known.includes(f))problems.push('TabUiState.'+f+': unregistered owner');for(const f of known)if(!fields.includes(f))problems.push('TabUiState.'+f+': stale registration');}

    if(file.startsWith('src/features/keymap/')) {
      if(policy.exceptions[file]||terminalStandardServices(ts)||ts.some(t=>['AppState','AppRuntime','Workspace','State','ApplicationHandle','KeyboardEvent','LogicalKey','NamedKey','KeyState','KeyModifiers','InputState','UiRect','TerminalHandle','TerminalController','TerminalRuntime','FileLoader','Child'].includes(t)))problems.push(file+': Keymap rules must be root GUI/native independent and have no exceptions');
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super')||/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_bytes'].includes(t)&&ts[i+1]==='!')||(ts.some((t,i)=>t==='include_str'&&ts[i+1]==='!')&&(file!=='src/features/keymap/keymap_file.rs'||!source.includes('include_str!("../../../assets/keymaps/default.json")'))))problems.push(file+': Keymap rules cannot include or escape to adapter sources');
    }
    for(const [f,name,register,privateFields] of [['src/features/settings/model.rs','Model','settings_model_ownership',true],['src/features/settings/model.rs','Values','settings_values_ownership',false],['src/features/keymap/model.rs','Model','keymap_model_ownership',true]])if(file===f) {
      const fields=structFields(source,name),known=Object.keys(policy[register]??{});for(const field of fields)if(!known.includes(field))problems.push(name+'.'+field+': unregistered R8 model field');for(const field of known)if(!fields.includes(field))problems.push(name+'.'+field+': stale R8 model registration');
      if(privateFields) {const body=source.slice(source.indexOf('struct '+name)).split('}')[0];if(/\bpub\b/.test(body.slice(body.indexOf('{'))))problems.push(file+': R8 model storage must be private');}
    }
    if(['src/ui/settings.rs','src/ui/settings_binding.rs','src/ui/keymap_page.rs','src/ui/keymap_binding.rs'].includes(file)) {
      if(policy.exceptions[file]||terminalStandardServices(ts)||ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','TerminalController','TerminalHandle','TerminalRuntime','RunningProcess','Registry'].includes(t))||deps.some(d=>['crate::app','crate::state','crate::infrastructure','crate::key_actions','crate::workspace_actions','crate::model'].includes(d)))problems.push(file+': R8 settings/keymap views require narrow frames and commands without native resources');
    }
    if(['src/file_loading.rs','src/file_watcher.rs','src/window_geometry.rs','src/launch.rs','src/key_actions.rs'].includes(file))problems.push(file+': removed R8 application/native composition path must not be restored');
    if(['src/infrastructure/file_loader.rs','src/infrastructure/file_watcher.rs','src/infrastructure/document_filesystem.rs','src/infrastructure/file_dialogs.rs','src/infrastructure/window_geometry.rs','src/infrastructure/launch.rs'].includes(file)) {
      if(policy.exceptions[file]||ts.some(t=>['AppState','AppRuntime','Workspace','InputState','ApplicationHandle','State'].includes(t))||deps.some(d=>['crate::app','crate::state','crate::model','crate::ui','crate::workspace_persistence'].includes(d)))problems.push(file+': R8 native adapters cannot own root/application/UI state or exceptions');
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super')||/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': R8 native adapters cannot include application sources');
    }
    const settingsRules=file.startsWith('src/features/settings/');
    const settingsNative=['src/infrastructure/settings_filesystem.rs','src/infrastructure/project_settings.rs','src/infrastructure/config_watcher.rs'].includes(file);
    if(settingsRules||settingsNative) {
      if(policy.exceptions[file]||ts.some(t=>['AppState','AppRuntime','Workspace','State','ApplicationHandle','InputState','UiRect','KeyboardEvent','TerminalController','FileLoader'].includes(t)))problems.push(file+': Settings rules/adapters must stay free of root state, GUI and exceptions');
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super')||/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Settings rules/adapters cannot include or escape to application sources');
    }
    if(settingsRules&&terminalStandardServices(ts))problems.push(file+': Settings rules cannot access standard I/O or environment services');
    if(['src/theme.rs','src/fonts.rs','src/file_icons.rs','src/jsonc.rs','src/settings_persistence.rs','src/project_settings.rs','src/config_watcher.rs','src/workspace_actions.rs'].includes(file))problems.push(file+': removed R8.1 root path must not be restored');
    if(deps.includes('crate::terminal_session'))problems.push(file+': old terminal composition path is removed');
    if(file==='src/main.rs'&&ts.some((t,i)=>t==='mod'&&['terminal_session','terminal_mouse','terminal_directory','terminal_profiles','terminal_program'].includes(ts[i+1])))problems.push(file+': old terminal module alias is removed');
    if(file==='src/ui/mod.rs'&&ts.some((t,i)=>t==='mod'&&['layout_map','terminal_settings','terminal_tab_context_menu'].includes(ts[i+1])))problems.push(file+': old terminal UI module aliases are removed');
    if(file==='src/app/shell.rs'&&ts.some(t=>['SustainedProgram','ProgramProbe','ensure_started','save_terminal_state','DirectoryReports'].includes(t)))problems.push(file+': terminal effect behavior belongs to app/terminal_effects');
    if(file==='src/app/terminal_effects.rs'&&ts.some((t,i)=>t==='struct'&&ts[i+1]==='Observation')) {
      const fields=structFields(source,'Observation');
      for(const field of ['id','generation'])if(!fields.includes(field))problems.push('Terminal observation must carry '+field);
    }
    if(file==='src/terminal_session.rs')problems.push(file+': removed R7.3 model/runtime composition must not be restored');
    if(['src/terminal_profiles.rs','src/terminal_directory.rs','src/terminal_program.rs'].includes(file))problems.push(file+': removed R7.2 adapters must not be restored');
    if(file.startsWith('src/infrastructure/terminal/')) {
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super'&&ts[i+3]==='::'&&ts[i+4]==='super'))problems.push(file+': Terminal infrastructure cannot escape to root through parent paths');
      if(policy.exceptions[file]||ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','UiRect','InputState','State'].includes(t)))problems.push(file+': Terminal infrastructure must stay free of root state, GUI and exceptions');
      if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Terminal infrastructure cannot include application sources');
    }
    if(['src/model/application_layout.rs','src/terminal_mouse.rs'].includes(file))problems.push(file+': removed R7.1 paths must not be restored');
    if(['src/terminal_session.rs','src/terminal_profiles.rs','src/terminal_directory.rs','src/terminal_program.rs'].includes(file)&&/\bpub(?:\s*\([^)]*\))?\s+use\s+crate\s*::\s*features\s*::\s*terminal\b/.test(ts.join(' ')))problems.push(file+': terminal types must use their feature paths, not legacy re-exports');
    if(layer(file)==='domain-verification') {
      if(policy.exceptions[file])problems.push(`${file}: domain verification cannot have migration exceptions`);
      // The domain's language assets are generated at build time, so this package's
      // build script may share the one scanner that builds them, and nothing else.
      if(ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!')||(/#\s*\[\s*path\s*=/.test(source)&&(file!=='crates/workspace-tests/src/isolated.rs'||!/^#\[path = "\.\.\/\.\.\/\.\.\/src\/domain\/mod\.rs"\]$/m.test(source))))problems.push(`${file}: domain verification can only include the production domain entry point`);
    }
    if(layer(file)==='core') {
      if(policy.exceptions[file])problems.push(`${file}: editing core cannot have migration exceptions`);
      if(ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','KeyboardEvent','Keystroke','UiRect','InputState'].includes(t)))problems.push(`${file}: editing core must not depend on application/platform state`);
      if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(`${file}: editing core cannot include sources outside its crate boundary`);
    }
    if(file==='src/app/terminal_binding.rs'||file==='src/infrastructure/terminal/runtime.rs') {
      const contracts=file==='src/app/terminal_binding.rs'?[['TerminalStore',['model','runtime']],['TerminalTabs',['model','runtime']],['TerminalTab',['id','group_id','width_weight','controller','cwd','title','purpose','running_program']]]:[['TerminalHandle',['runtime','id','shell']]];
      for(const [name,expected]of contracts){
        if(!ts.some((t,i)=>t==='struct'&&ts[i+1]===name))continue;
        const actual=structFields(source,name);if(actual.length!==expected.length||actual.some(f=>!expected.includes(f)))problems.push(name+': unexpected snapshot/handle ownership field');
        const start=ts.findIndex((t,i)=>t==='struct'&&ts[i+1]===name),open=ts.indexOf('{',start);let end=open+1,depth=1;while(end<ts.length&&depth){if(ts[end]==='{')depth++;if(ts[end]==='}')depth--;end++;}const body=ts.slice(open+1,end-1).join(' ');
        if(name==='TerminalTab'){if(!/controller\s*:\s*TerminalHandle\b/.test(body)||/\b(?:Arc|Box|TerminalController|TerminalRuntime)\b/.test(body))problems.push(name+': projection must use weak session handles');continue;}
        if(name==='TerminalStore'&&!/model\s*:\s*State\s*<\s*TerminalModel\s*>/.test(body))problems.push(name+': State must contain the pure terminal model');
        if(name==='TerminalTabs'&&!/model\s*:\s*TerminalModel\b/.test(body))problems.push(name+': snapshot must contain pure model values');
        if(!/runtime\s*:\s*Weak\s*<\s*TerminalRuntime\s*>/.test(body))problems.push(name+': runtime access must stay weak');
      }
    }
    if(file==='src/features/terminal/model.rs') {
      for(const [name,register]of [['TerminalModel','terminal_model_ownership'],['TerminalMetadata','terminal_metadata_ownership']]) {
        if(!ts.some((t,i)=>t==='struct'&&ts[i+1]===name))continue;
        const fields=structFields(source,name),registered=Object.keys(policy[register]??{});
        for(const field of fields)if(!registered.includes(field))problems.push(name+'.'+field+': no registered owner');
        for(const field of registered)if(!fields.includes(field))problems.push(name+'.'+field+': obsolete ownership entry');
        const start=ts.findIndex((t,i)=>t==='struct'&&ts[i+1]===name);let depth=0;
        for(let i=start+2;i<ts.length;i++){if(ts[i]==='{'){depth++;continue;}if(ts[i]==='}'&&--depth===0)break;if(depth===1&&ts[i]==='pub'){problems.push(name+' fields must stay private');break;}}
        for(const [field,entry]of Object.entries(policy[register]??{}))if(!entry.owner||!entry.migrated_in)problems.push(name+'.'+field+': incomplete ownership entry');
      }
      if(ts.some(t=>['Arc','Weak','Box','TerminalHandle','TerminalRuntime','TerminalController','TerminalInner','RunningProcess','Registry'].includes(t)))problems.push(file+': terminal metadata cannot embed or alias runtime ownership');
    }
    if(file.startsWith('src/features/terminal/')) {
      if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super'))problems.push(file+': Terminal rules cannot escape through ancestor paths');
      if(terminalStandardServices(ts))problems.push(file+': Terminal rules cannot access standard I/O or environment services');
      if(ts.some(t=>['AppState','AppRuntime','Workspace','State','ApplicationHandle','KeyboardEvent','KeyModifiers','InputState','UiRect','TerminalController','TerminalInner','RunningProcess','FileLoader','AtomicBool','AtomicU64','Mutex','RwLock','Command','Child'].includes(t)))problems.push(file+': Terminal rules must be framework/root/runtime independent');
      if(policy.exceptions[file])problems.push(file+': Terminal rules cannot have migration exceptions');
      if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Terminal rules cannot include adapter sources');
    }
    if(['src/ui/terminal.rs','src/ui/terminal_settings.rs','src/ui/terminal_tab_context_menu.rs','src/ui/layout_map.rs'].includes(file))problems.push(file+': removed R7.6 terminal view path must not be restored');
    if(file==='src/ui/terminal_settings.rs'||file.startsWith('src/ui/terminal/')) {
        if(ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','TerminalController','TerminalHandle','TerminalRuntime','TerminalInner','RunningProcess','Registry'].includes(t))||deps.some(d=>['crate::app','crate::state','crate::infrastructure','crate::key_actions','crate::model','crate::workspace_actions'].includes(d)))problems.push(file+': Terminal projection view must use narrow state and commands');
        if(policy.exceptions[file])problems.push(file+': Terminal projection view cannot have migration exceptions');
        if(terminalStandardServices(ts)||deps.some(d=>['std::fs','std::process','std::thread','std::net'].includes(d)))problems.push(file+': Terminal projection view cannot perform native I/O or environment work');
        if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Terminal projection view cannot include root sources');
        if(ts.some((t,i)=>t==='super'&&ts[i+1]==='::'&&ts[i+2]==='super'))problems.push(file+': Terminal projection view cannot escape through ancestor paths');
    }
    if(['src/ui/terminal.rs','src/ui/terminal_tab_context_menu.rs'].includes(file)) {
        const production=source.split(/#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+tests\b/)[0];
        const pt=tokens(production);
        for(let i=0;i<pt.length-2;i++)if(pt[i]==='.'&&['add','add_at','select','split','close','set_purpose','resize_pair','write','paste','terminate','begin_selection','extend_selection','set_mouse_position','wheel'].includes(pt[i+1])&&pt[i+2]==='(')problems.push(file+': Terminal behavior must execute through app commands: '+pt[i+1]);
    }
    if(file.startsWith('src/features/search/')&&ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','InputState','UiRect'].includes(t)))problems.push(file+': Search feature must be framework/root independent');
    if(file.startsWith('src/ui/search/')&&ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle'].includes(t)))problems.push(file+': Search view must use projections and commands');
    if(file.startsWith('src/ui/editor/')||file==='src/ui/editor_binding.rs') {
      if(ts.some(t=>['AppState','AppRuntime','Workspace','StateRoot','ApplicationHandle','Vim'].includes(t))||deps.some(d=>['crate::app','crate::state','crate::model','crate::git','crate::editor'].includes(d)))problems.push(file+': Editor view must use pane projections and commands');
      if(policy.exceptions[file])problems.push(file+': Editor view cannot have migration exceptions');
      if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Editor view cannot include root sources');
    }
    if(file.startsWith('src/features/explorer/')&&ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','InputState','UiRect','State'].includes(t)))problems.push(file+': Explorer feature must be framework/root independent');
    if(file.startsWith('src/ui/explorer/')&&ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle'].includes(t)))problems.push(file+': Explorer view must use projections and commands');
    if(file.startsWith('src/ui/explorer/')&&policy.exceptions[file])problems.push(file+': Explorer view cannot have migration exceptions');
    if(file.startsWith('src/features/git/')&&ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','InputState','UiRect','State','Command','Child'].includes(t)))problems.push(file+': Git feature must be framework/root/process independent');
    if(file.startsWith('src/features/git/')&&policy.exceptions[file])problems.push(file+': Git feature cannot have migration exceptions');
    if(file.startsWith('src/ui/git/')) {
      if(ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','StateRoot'].includes(t))||deps.some(d=>['crate::app','crate::state','crate::model','crate::infrastructure','crate::editor','crate::workspace_actions'].includes(d)))problems.push(file+': Git view must use projections, frame inputs and commands');
      if(policy.exceptions[file])problems.push(file+': Git view cannot have migration exceptions');
      if(/#\s*\[\s*path\s*=/.test(source)||ts.some((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!'))problems.push(file+': Git view cannot include root sources');
    }
    if(file.startsWith('src/infrastructure/git/')&&(policy.exceptions[file]||ts.some(t=>['AppState','AppRuntime','Workspace','ApplicationHandle','UiRect'].includes(t))))problems.push(file+': Git infrastructure must stay free of root state, GUI and exceptions');
    if(file.startsWith('src/features/explorer/')&&policy.exceptions[file])problems.push(file+': Explorer feature cannot have migration exceptions');
    if((file.startsWith('src/features/search/')||file.startsWith('src/ui/search/'))&&policy.exceptions[file])problems.push(file+': Search cannot have migration exceptions');
    if(file==='src/model/workspace.rs'&&deps.some(d=>['crate::search','crate::search_actions','crate::features'].includes(d)))problems.push('Workspace must expose generic snapshots, not Search DTOs');
    if(layer(file)==='domain'&&(ts.some(t=>['super','AppState','AppRuntime'].includes(t))||/#\s*\[\s*path\s*=/.test(source)))problems.push(`${file}: domain must not escape through parent modules or root/runtime types`);
    if(layer(file)==='domain') {
      if(policy.exceptions[file])problems.push(`${file}: domain cannot have migration exceptions`);
      if(ts.some(t=>['UiRect','Color','ApplicationHandle','FileLoader','InputState'].includes(t)))problems.push(`${file}: domain must not contain framework or runtime types`);
      // One exception: metadata-only unit fixtures behind cfg(test) are
      // not runtime resources. Any other include stays forbidden.
      const includes=ts.filter((t,i)=>['include','include_str','include_bytes'].includes(t)&&ts[i+1]==='!').length;
      const generatedTable=file==='src/domain/language/embedded.rs'&&includes===1&&/#\[cfg\(test\)\]\s*include!\s*\(\s*"\.\.\/\.\.\/\.\.\/tests\/fixtures\/language_registry\.rs"\s*\)\s*;/.test(source);
      if(includes>0&&!generatedTable)problems.push(`${file}: domain cannot include adapter sources`);
    }
    if(layer(file)==='ui'&&ts.includes('AppState')&&!policy.root_state_adapters.includes(file))problems.push(`${file}: new root-state adapter`);
    if(/\.\s*runtime\s*\(/.test(ts.join(' '))&&!(policy.framework_runtime_access??policy.runtime_bridges??[]).includes(file))problems.push(`${file}: unregistered runtime bridge`);
  }
  for(const file of policy.root_state_adapters)if(!sources[file]||!tokens(sources[file]).includes('AppState'))problems.push(`${file}: obsolete root-state adapter entry`);
  for(const file of (policy.framework_runtime_access??policy.runtime_bridges??[]))if(!sources[file]||!/\.\s*runtime\s*\(/.test(tokens(sources[file]).join(' ')))problems.push(`${file}: obsolete runtime bridge entry`);
  for(const file of Object.keys(policy.exceptions))if(!sources[file])problems.push(`${file}: obsolete exception entry`);
  for(const [name,known]of Object.entries(policy.chrome_ownership??{})) {const source=sources['src/app/state/chrome.rs'];if(!source||!tokens(source).includes(name)){problems.push(name+': chrome owner declaration missing');continue;}const actual=structFields(source,name);for(const f of actual)if(!known.includes(f))problems.push(name+'.'+f+': unregistered chrome owner');for(const f of known)if(!actual.includes(f))problems.push(name+'.'+f+': stale chrome owner');}
  const state=sources['src/app/state.rs'];
  if(state) {
    const actual=stateFields(state),registered=Object.keys(policy.state_ownership).sort();
    for(const field of actual)if(!registered.includes(field))problems.push(`AppState.${field}: no registered owner`);
    for(const field of registered)if(!actual.includes(field))problems.push(`AppState.${field}: obsolete ownership entry`);
    const body=state.slice(state.indexOf('pub struct AppState'),state.indexOf('\nimpl AppState'));
    if(/\b(?:FileLoader|ApplicationHandle|PollControl|AppContext|AppRuntime|TerminalRuntime|Weak)\b/.test(tokens(body).join(' ')))problems.push('AppState embeds a runtime service');
  }
  const search=sources['src/ui/search/state.rs']??sources['src/search_actions.rs'];
  if(search) {
    const start=search.indexOf('pub struct SearchUiState'),end=search.indexOf('\nimpl SearchUiState',start);
    if(start>=0&&/\b(?:AtomicBool|ApplicationHandle|FileLoader|AppRuntime)\b/.test(tokens(search.slice(start,end)).join(' ')))problems.push('SearchState embeds a runtime service');
  }
  const editor=sources['src/app/state/editor.rs'];
  if(editor) {
    const actual=structFields(editor,'EditorState'),registered=Object.keys(policy.editor_state_ownership??{});
    for(const field of actual)if(!registered.includes(field))problems.push(`EditorState.${field}: no registered owner`);
    for(const field of registered)if(!actual.includes(field))problems.push(`EditorState.${field}: obsolete ownership entry`);
    if(tokens(editor).some(t=>['FileLoader','ApplicationHandle','PollControl','AppRuntime','AtomicBool'].includes(t)))problems.push('EditorState embeds a runtime service');
  }
  for(const [field,entry] of Object.entries(policy.editor_state_ownership??{}))if(!entry.owner||!entry.migrated_in)problems.push(`EditorState.${field}: incomplete ownership entry`);
  const workspace=sources['src/domain/workspace.rs'];
  if(workspace) {
    const actual=structFields(workspace,'Workspace'),registered=Object.keys(policy.workspace_core_ownership??{});
    for(const field of actual)if(!registered.includes(field))problems.push(`Workspace.${field}: no registered owner`);
    for(const field of registered)if(!actual.includes(field))problems.push(`Workspace.${field}: obsolete ownership entry`);
  }
  for(const [file,name,owner,register] of [['src/features/explorer/model.rs','ExplorerModel','features/explorer/model','explorer_ownership'],['src/ui/explorer/state.rs','ExplorerUiState','ui/explorer/state','explorer_ownership'],['src/features/git/model.rs','GitModel','features/git/model','git_ownership'],['src/ui/git/state.rs','GitUiState','ui/git/state','git_ownership']]) {
    const source=sources[file];
    if(!source)continue;
    const actual=structFields(source,name),registered=Object.entries(policy[register]??{}).filter(([,e])=>e.owner?.startsWith(owner)).map(([k])=>k);
    for(const field of actual)if(!registered.includes(field))problems.push(`${name}.${field}: no registered owner`);
    for(const field of registered)if(!actual.includes(field))problems.push(`${name}.${field}: obsolete ownership entry`);
  }
  for(const [file,name,owner] of [
    ['src/app/state/layout.rs','ApplicationLayoutState','app/state/layout'],
    ['src/app/state/chrome.rs','TerminalChrome','app/state/chrome'],
    ['src/app/state.rs','TerminalPanelState','app/state:TerminalPanelState'],
    ['src/ui/terminal/state.rs','TerminalUiState','ui/terminal/state'],
    ['src/ui/terminal/state.rs','ProfileEditor','ui/terminal/state:ProfileEditor'],
    ['src/features/terminal/availability.rs','TerminalAvailability','features/terminal/availability'],
  ]) {
    const registered=Object.entries(policy.terminal_state_ownership??{}).filter(([,e])=>e.owner===owner).map(([k])=>k.slice(owner.length+1));
    const declared=sources[file]&&tokens(sources[file]).some((t,i,ts)=>t==='struct'&&ts[i+1]===name);
    if(!declared) {
      if(registered.length)problems.push(name+': registered owner declaration is missing');
      continue;
    }
    const actual=structFields(sources[file],name);
    for(const field of actual)if(!registered.includes(field))problems.push(name+'.'+field+': no registered owner');
    for(const field of registered)if(!actual.includes(field))problems.push(name+'.'+field+': obsolete ownership entry');
    const body=file==='src/app/state.rs' ? tokens(sources[file].slice(sources[file].indexOf('pub struct TerminalPanelState'),sources[file].indexOf('pub struct TerminalPanelState')+sources[file].slice(sources[file].indexOf('pub struct TerminalPanelState')).indexOf('}'))).join(' ') : tokens(sources[file]).join(' ');
    if(/\b(?:TerminalController|TerminalHandle|TerminalRuntime|AppRuntime|FileLoader|PollControl|ApplicationHandle)\b/.test(body))problems.push(name+': terminal state cannot own runtime services');
  }
  for(const [field,entry]of Object.entries(policy.terminal_state_ownership??{}))if(!entry.owner||!entry.migrated_in)problems.push('TerminalState.'+field+': incomplete ownership entry');
  const rootState=sources['src/app/state.rs'];
  if(rootState&&/\bpub\s+use\s+crate\s*::\s*features\s*::\s*explorer\b/.test(tokens(rootState).join(' ')))problems.push('Explorer types must not be re-exported from the root state');
  if(rootState&&/\bpub\s+use\s+crate\s*::\s*features\s*::\s*git\b/.test(tokens(rootState).join(' ')))problems.push('Git types must not be re-exported from the root state');
  for(const [file,name] of [['src/features/explorer/model.rs','ExplorerModel'],['src/features/git/model.rs','GitModel']]) {
    const model=sources[file];
    if(!model)continue;
    const ts=tokens(model),start=ts.findIndex((t,i)=>t==='struct'&&ts[i+1]===name);
    let depth=0;
    for(let i=start+2;start>=0&&i<ts.length;i++){if(ts[i]==='{'){depth++;continue;}if(ts[i]==='}'&&--depth===0)break;if(depth===1&&ts[i]==='pub'){problems.push(name+' fields must stay private');break;}}
  }
  for(const [field,entry] of Object.entries(policy.explorer_ownership??{}))if(!entry.owner||!entry.migrated_in)problems.push(`Explorer.${field}: incomplete ownership entry`);
  for(const [field,entry] of Object.entries(policy.git_ownership??{}))if(!entry.owner||!entry.migrated_in)problems.push(`Git.${field}: incomplete ownership entry`);
  for(const [field,entry] of Object.entries(policy.workspace_core_ownership??{}))if(!entry.owner||!entry.migrated_in)problems.push(`Workspace.${field}: incomplete ownership entry`);
  for(const [field,entry] of Object.entries(policy.state_ownership))if(!entry.owner||!entry.remove_in)problems.push(`AppState.${field}: incomplete ownership entry`);
  for(const [file,entry] of Object.entries(policy.exceptions))if(!entry.reason||!entry.remove_in)problems.push(`${file}: incomplete migration exception`);
  return problems;
}

export function readSources(root) {
  const result={};
  function visit(dir){for(const entry of fs.readdirSync(path.join(root,dir),{withFileTypes:true})){const file=dir+'/'+entry.name;if(entry.isDirectory())visit(file);else if(file.endsWith('.rs'))result[file]=fs.readFileSync(path.join(root,file),'utf8');}}
  visit('src');
  if(fs.existsSync(path.join(root,'crates/editor-core')))visit('crates/editor-core');
  if(fs.existsSync(path.join(root,'crates/workspace-tests')))visit('crates/workspace-tests');
  return result;
}

export function hostBuildScriptErrors(source) {
  return /language[_-](?:packs|packages|assets)|(?:assets|language-packs)[\/\\](?:grammars|queries|manifests)|stage_packs/.test(source)
    ? ['Host build script must not build or stage language resources'] : [];
}

export function runtimeGrammarManifestErrors(source) {
  let section='';
  const errors=[];
  const external=name => name.startsWith('loom-grammar-') ||
    (name.startsWith('tree-sitter-') && name!=='tree-sitter-language');
  for(const raw of source.split(/\r?\n/)) {
    const line=raw.replace(/#.*$/,'').trim();
    if(line.startsWith('['))section=line.slice(1,line.lastIndexOf(']'));
    const runtime=/(?:^|\.)(?:dev-|build-)?dependencies(?:\.|$)/.test(section);
    if(section==='workspace' && /language-packs|loom-grammar/.test(line))errors.push('Host workspace must not include language package sources');
    if(!runtime)continue;
    const names=[line.match(/^([\w-]+)\s*=/)?.[1],
      ...[...line.matchAll(/\bpackage\s*=\s*"([^"]+)"/g)].map(m=>m[1]),
      section.match(/dependencies\.([\w-]+)$/)?.[1]].filter(Boolean);
    if(names.some(external) || /\bpath\s*=\s*"[^"]*language-packs\//.test(line)) {
      errors.push('Loom build, development and runtime dependencies must not reference language grammars');
    }
  }
  return errors;
}

if(process.argv[1]&&path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
  const policy=JSON.parse(fs.readFileSync(new URL('./policy.json',import.meta.url),'utf8'));
  const sources=readSources(root),errors=violations(sources,policy);
  errors.push(...runtimeGrammarManifestErrors(fs.readFileSync(path.join(root,'Cargo.toml'),'utf8')));
  errors.push(...hostBuildScriptErrors(fs.readFileSync(path.join(root,'build.rs'),'utf8')));
  const verificationManifest=path.join(root,'crates/workspace-tests/Cargo.toml');
  if(fs.existsSync(verificationManifest))errors.push(...verificationManifestErrors(fs.readFileSync(verificationManifest,'utf8')));
  if(errors.length){console.error(errors.join('\n'));process.exitCode=1;}
  else console.log(`Architecture gate passed: ${Object.keys(sources).length} source files; ${Object.keys(policy.state_ownership).length} root, ${Object.keys(policy.editor_state_ownership??{}).length} editor and ${Object.keys(policy.workspace_core_ownership??{}).length} workspace owned fields; ${Object.keys(policy.exceptions).length} tracked legacy exception files.`);
}
