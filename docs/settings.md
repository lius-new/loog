# Settings and key bindings

## Configuration files

User settings (`settings.json`), key bindings (`keymap.json`) and the saved
session (`workspace.json`) live in the user configuration folder:

| Platform | Folder |
| --- | --- |
| Windows | `%APPDATA%\Loom` |
| macOS | `~/Library/Application Support/Loom` |
| Linux | `$XDG_CONFIG_HOME/loom`, or `~/.config/loom` |

On Windows, all persistent user data uses `%APPDATA%\Loom` (Roaming), including
installed extensions (including downloaded languages) under `extensions` and your
own highlight queries under `queries`. Loom does not split
user data between the Roaming and Local AppData folders.

A workspace folder can override settings in `.loom/settings.json`; the
`workspace::OpenProjectSettingsFile` action creates and opens it. Appearance
preferences, such as the theme and the interface scale, can only be set in the
user `settings.json`. Choose the theme with **Select Theme** in the command
palette (`Ctrl+Shift+P`), which lists every theme and saves the choice.

## Interface size

Settings → Appearance → **Interface scale** resizes the entire app, including
tabs, sidebars, menus, icons and controls. Use **−** / **+** to choose 80%–200%
in 10 percentage point steps, relative to the operating system's display scale.
**Reset** restores automatic scaling. Changes apply immediately and persist
across restarts. Editor and terminal font sizes can still be adjusted separately.

**Ctrl + +** (or **Ctrl + =**) and **Ctrl + −** adjust the terminal font size
while the terminal has focus, and the editor font size otherwise. **Ctrl + 0**
resets that font's size. On macOS, use Command instead of Ctrl.

The same setting is available in the user `settings.json` as `"ui_scale": 120`.
Remove it or set it to `null` to restore automatic scaling. Like other appearance
preferences, it cannot be overridden by project settings.

## Code fonts

Settings → Editor → **Font weight** adjusts the weight of code text, including
diffs and change previews. Use **−** / **+** to choose 100–900 in steps of 100;
400 is regular and 700 is bold. Changes apply immediately and persist across
restarts. Settings → Terminal has an independent **Font weight** control.

The user `settings.json` accepts the same preferences:

```json
{
  "editor_font": { "weight": 500 },
  "terminal_font": { "weight": 400 }
}
```

Both fonts default to weight 400 when the field is omitted. Numeric weights
outside 100–900 are clamped to that range; values between steps, such as 450,
can be entered in JSON. The appearance depends on the weights available in the
chosen font. Font preferences are user settings and cannot be overridden by a
project's `.loom/settings.json`. Font family, size and line height remain
independent of weight.

## File associations

Loom picks a file's language from the file itself: its name and extension. To
treat other files as a language, add ordered rules to `settings.json` (or to a
workspace folder's `.loom/settings.json`):

```jsonc
"file_associations": [
  { "pattern": "**/*.custom", "language": "rust" },
  { "pattern": "scripts/*.tool", "language": "javascript" }
]
```

`language` is a language name or alias such as `rust`, `csharp`, `javascript`
or `typescript`. Rules are tried in order and the first match wins. A project's
rules apply to files in that folder and come before your own, and both come
before the built-in recognition. Changes apply to open files immediately, and a
rename picks the language for the new name.

Patterns use a small set of wildcards:

- `*` matches any run of characters within one path segment, `?` matches one.
- `**` as a whole segment matches any number of folders, including none.
- A pattern without `/` matches the file name at any depth (`*.custom`).
- A pattern with `/` matches the path below the workspace folder, from its
  start (`docs/**/*.txt`). Files outside every workspace folder only match
  patterns without `/`.
- Case is ignored on Windows and macOS and respected elsewhere.
- `[ ]`, `{ }` and `\` are not supported; a rule using them is skipped.

A rule with an unknown language or an unsupported pattern is skipped and
reported; the other rules still apply.

## Language servers

Implementation and ownership are described in [LSP architecture](architecture/lsp.md).

Loom ships no language servers and does not look for them on your PATH. The
servers it can run are the ones that extensions declare (install one from the
Extensions page: **Browse**, then Install; it brings the runtime it needs), plus
servers you register yourself. A language can have several servers; they all run for a file, and their
diagnostics are shown together. A language that no installed server serves just
has no language-server features.

`language_servers` is an object keyed by server name.

```jsonc
"language_servers": {
  // An extension's server: change how it behaves, never which program it is.
  "rust-analyzer": {
    "enabled": true,
    "args": [],                                    // replaces the default arguments
    "initialization_options": { "checkOnSave": false },
    "settings": { "rust-analyzer": { "check": { "command": "clippy" } } },
    "env": { "RA_LOG": "info" }
  },
  // A server of your own: a full definition. Its `command` must be an absolute
  // path; Loom never searches PATH.
  "my-server": {
    "command": "C:\\tools\\my-server.exe",
    "args": ["--stdio"],
    "languages": [{ "language": "foo", "language_id": "foo" }],
    "root": { "markers": ["foo.toml"], "strategy": "nearest", "fallback": "workspace" },
    "priority": 10
  }
}
```

For an extension's server only `enabled`, `args`, `initialization_options`,
`settings`, `env` and `priority` may be given; its `command`, `languages`, `root` and
`display_name` are the extension's. To run another program, register it under a new
name. An entry with a mistake is reported and skipped; the others still apply.

A server runs once per project folder, found with the project files its definition
names (the nearest or the outermost one), never above the open workspace folder. It
stops a minute after its last file is closed, and a crashed server is restarted
after 1, 2 and 4 seconds before being reported as failed.

Each server instance reports only its own project folder to the server. Opening
several project folders creates separate instances and keeps their document
sessions and results independent.

The language panel shows the server's background tasks. A task marked cancellable
by its server has a **Cancel** button. It stays marked as cancelling until the
server ends it. If cancellation is not acknowledged within five seconds, or a task
reports no progress for five minutes, Loom stops tracking it and records a warning
in the panel log. This does not mean the server finished the work; **Restart
servers** remains available when a server is stuck.

`settings` is what the server gets when it asks for its configuration
(`workspace/configuration`). Changing it tells the running server; it is not
restarted. Changing `args`, `initialization_options` or `env` starts a new instance.

### Languages

```jsonc
"languages": {
  "typescript": {
    "language_servers": ["biome", "typescript-language-server", "..."],
    "formatter": "biome",
    "format_on_save": true,
    "code_actions_on_save": ["source.organizeImports", "source.removeUnusedImports"]
  }
}
```

`language_servers` chooses which servers a language uses and in what order (the
first is preferred); `"..."` stands for the servers not named, in their usual
order, and a list without it leaves the others out. `formatter` names the server
that formats (by default the first that can). `format_on_save` and
`code_actions_on_save` run before the file is written. Action kinds run in list
order, with each request using the text left by the preceding action; lazy
actions are resolved and commands finish before formatting. Each server step
has a one second limit, and the complete pipeline has a two second limit before
the file is saved anyway. Saving continues to target the original document if
you switch tabs while waiting. TypeScript Language Server 6 uses separate kinds
for sorting/combining imports and removing unused imports, as in this example.

### Behaviour

```jsonc
"lsp": {
  "hover_delay_ms": 500,             // how long the pointer rests before hover opens
  "completion": {
    "auto": true,                    // open the list while typing; false = only the key
    "accept_on": "enter_and_tab",    // enter_and_tab | enter | tab
    "words_fallback": true           // complete words of the file without a server
  },
  "inlay_hints": false,
  "semantic_highlighting": "combined",   // combined | off
  "max_document_bytes": 8388608          // larger files are not sent to servers
}
```

These settings are only read from your own `settings.json`: a project's
`.loom/settings.json` cannot choose which program Loom runs.

### Keys

| Key | Action |
| --- | --- |
| Ctrl+Space | Show completions |
| Ctrl+Space in a choice placeholder | Reopen the snippet's choices |
| Ctrl+Shift+Space | Signature help |
| Alt+Up / Alt+Down | Previous / next overload while signature help offers several |
| Ctrl+K Ctrl+I | Hover |
| F12 / Ctrl+F12 / Ctrl+Shift+F12 / Alt+F12 | Definition / implementation / type definition / declaration |
| Shift+F12 | References |
| F2 | Rename |
| Ctrl+. | Code actions |
| Shift+Alt+F | Format file |
| Ctrl+K Ctrl+F | Format selection |
| F8 / Shift+F8 | Next / previous problem |
| Ctrl+Shift+O / Ctrl+T | Symbols in the file / in the workspace |
| Alt+Left / Alt+Right | Back / forward |
| Shift+Alt+Right / Left | Grow / shrink the selection by syntax |

A lightbulb appears in the gutter when the caret or selection rests for 350 ms
and a language server offers an enabled code action. Click it to open the cached
list, or press Ctrl+. to request actions explicitly. Discovery never applies an
edit. The bulb and its menu expire when the selection, open documents,
diagnostics, or server registrations change. Empty, disabled-only, and failed
automatic results stay quiet; explicit menus retain disabled actions and their
reasons. The bulb has its own lane beside line numbers and Git change marks and
keeps the editor's caret and focus when clicked.

Signature help also offers up/down buttons when the call has several overloads.
Switching updates the parameter highlight and documentation without moving the
caret. The selection is retained while help refreshes, including when the server
reorders its signatures; if it disappears, the server's current choice is used.
The buttons keep focus in the editor, and the completion list's Up/Down keys still
move through completions. Customize these bindings in the `SignatureHelp` context
with `editor::PreviousSignature` and `editor::NextSignature`.

Snippet placeholders can offer a list of values. Up/Down and PageUp/PageDown move
through it; Enter, Tab, or a click chooses a value and updates its mirrors in one
undo step. Tab then moves to the next placeholder, and Shift+Tab goes back.
Escape dismisses the list so you can type your own value; Ctrl+Space reopens it
at the current choice. This uses `editor::ShowSnippetChoices` in the
`SnippetChoice` context. The list keeps focus in the editor and its confirmation
keys are independent of `lsp.completion.accept_on`. Undo/Redo ends the snippet
session; Escape outside the list also ends it.

Snippets can transform variables when inserted and placeholder mirrors when Tab
leaves the current placeholder. For example, `${1:name} ${1/(.*)/${1:/upcase}/}`
turns the second `name` into `NAME` on Tab. Capture references, conditional formats,
and common case conversions are supported. Unknown variable names become editable
placeholders; repeated names share their value. Known variables without an
available value use their default or an empty string. Invalid or over-budget
transforms preserve the input value; regular expression behavior is not fully
compatible with ECMAScript UTF-16 semantics.

In Vim's normal mode: `K` hover, `gd` definition, `gD` declaration, `gy` type
definition, `gi` implementation, `gr` references.

## Key bindings

Shortcuts come from a Zed-style keymap. The **Keymap** page (Ctrl + K Ctrl + S,
or Settings → Keyboard → Open Keymap) opens in its own tab and lists every
action with its bindings; filter it by action, keystroke or context. Its
**Edit keymap.json** button opens your `keymap.json` in the
[configuration folder](#configuration-files). Saved changes apply immediately;
entries with mistakes are reported and skipped.

```jsonc
[
  {
    "context": "Editor",                     // where the bindings apply
    "bindings": {
      "alt-up": "editor::MoveToBeginning",   // keystrokes → action
      "secondary-y": null,                   // null disables a default binding
      "secondary-k secondary-d": ["pane::ActivateItem", 0] // sequences, arguments
    }
  }
]
```

- **Keystrokes** join modifiers with `-`: `ctrl`, `alt`, `shift`, `cmd`, and
  `secondary` (Command on macOS, Ctrl elsewhere). Separate a sequence with spaces.
- **Contexts**: `Workspace` (always), `Editor`, `Terminal`, `Input`, `Menu`,
  `TrustPrompt`, `CloseConfirmation`, `CloneRepository`. Combine them with `&&`, `||`, `!` and
  `>` (ancestor), and test attributes such as `extension == rs` or `os == windows`.
- **Precedence**: a binding in a more specific context wins (Editor over
  Workspace); within one context, later bindings — yours — win.

The default bindings are in [`assets/keymaps/default.json`](../assets/keymaps/default.json).
`LOOM_LOG_KEYS=1` in a debug build prints how each keystroke resolved.

## Markdown reading

Use `"markdown": {"font_size": 16, "line_height": 22, "max_width": 960}` in user
or project settings. See [Markdown reading](markdown.md) for ranges, actions
and resource policy.

## Indentation and line endings

Click **Spaces/Tab** in the status bar to choose two spaces, four spaces or tabs
for this open document. Tab display width is a separate choice (2, 4 or 8).
These choices last until the document closes. The menu also opens user and
project defaults. Settings JSON accepts:

```json
{
  "editor": {
    "indent_size": 4,
    "tab_width": 8,
    "use_tabs": false,
    "auto_pairs": true,
    "new_file_line_ending": "lf"
  },
  "editor_languages": {
    "go": { "use_tabs": true },
    "python": { "indent_size": 4 }
  }
}
```

Both objects are allowed in `.loom/settings.json`. Widths accept 1–16.
Effective precedence, highest first: a temporary document choice; project
language overrides; project editor overrides; matching `.editorconfig` rules;
user language overrides; user editor defaults. Unspecified fields inherit.
EditorConfig supports directory inheritance with `root=true`, file globs,
`indent_style`, `indent_size` (including `tab`), `tab_width`, `end_of_line`
(`lf`/`crlf`) and `unset`. Changes inside watched workspace folders refresh in
the background. Reopen files outside a watched folder to reread their rules.

Enter carries indentation and opens an indented middle line between brackets;
typing a closer on an indented empty line removes one indentation step. Tab and
Shift-Tab affect touched lines, preserving selection direction. A selection
ending at the next line's start excludes that line.

Click **LF**, **CRLF** or **Mixed** to normalize a file's newlines. Conversion is
one undo step, follows selections in split views, and keeps the final-newline
shape, character encoding and BOM. It changes the buffer; Save and Save All
write the converted bytes. Empty and single-line documents can choose future
newlines without adding text. Enter, paste, completion snippets and formatting
respect the document's convention. Mixed files retain their actual text until
an explicit conversion. Opening an existing file never normalizes its content.

The default and EditorConfig `end_of_line` apply only to documents that have not
contained a line break. Existing line breaks establish the convention. A manual
choice persists in the restored session, including empty documents. Closing and
reopening from disk detects the saved bytes again. A clean external reload
redetects disk line endings; external changes to unsaved text keep the existing
conflict flow. Git's automatic line-ending settings are independent.
