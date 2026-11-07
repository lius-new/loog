# Terminal

## Shells

On Windows, Loog offers installed PowerShell, Command Prompt, Bash (with
Git Bash preferred) and registered WSL distributions. On Linux and macOS, the
system default follows `$SHELL`, then the account's login shell, then `/bin/sh`.

## Custom terminals

Settings → Terminal lets you add, edit and remove custom terminals by name,
executable and launch arguments, and choose a default. Unavailable custom
terminals keep their configuration and are disabled until their path is fixed.
An unavailable saved default is kept, and new terminals use the first available
shell until its path is fixed. Restored terminals whose shell is unavailable
start a fallback shell with a notice, and launch their own shell again the next
time they start after it is repaired. Deleting the default terminal resets the default.

## WSL on Windows

Installed WSL distributions appear automatically as `WSL · Debian`, for example,
in Settings → Terminal and the terminal shell menu. Select a row in Settings to
make that distribution the default. The list refreshes when Loog starts, when
Settings opens and when the terminal shell menu opens. Detection reads the Windows
registry without starting WSL. No executable path or arguments need to be entered.

For custom launch arguments or a specific Linux user, you can still add a custom
terminal with `C:\Windows\System32\wsl.exe` as its executable and arguments such
as `-d Debian --user myuser`.

WSL does not receive Loog's automatic shell integration, so directory changes
inside WSL are not automatically reported to Loog. A localhost proxy warning
from WSL concerns its network configuration and is separate from launching
the terminal.

## Working directories

PowerShell, Command Prompt, Bash, zsh, fish and POSIX shells can report their
working directory to Loog. Custom commands and startup-file arguments take
precedence over automatic integration and may provide their own directory reports.
Each terminal remembers its latest directory after `cd` and restores it when
the application restarts. New tabs and splits inherit the current terminal's
directory. If a saved directory no longer exists, Loog falls back to the first
workspace folder, then the application's working directory. Shell profiles
and custom prompts are loaded without modifying user configuration files.

## Header tools

The terminal header keeps its tools on the right, in three groups separated by
thin rules. The first group acts on sessions: **Stop** (shown only while a
program is running; it sends Ctrl+C), **New terminal** and the shell menu. The
second acts on the terminal itself: **Split**, **Search** and **More**. The last
belongs to the panel: **Maximize** or **Restore**, and a collapse icon. The panel
is still shown and hidden with its own toggle.

On the left, a compact **Terminal / Commits** switch chooses the bottom panel's
content. **Terminal** shows the session count when space allows. **Commits** opens
the existing Git history; switching back keeps terminal sessions and their output.
Each
session is a rectangular tab with full-height separators between groups and an always
visible close button. The selected session uses the theme's contrasting tab
background, including in dark and high-contrast themes. Its
status icon is a spinner while a program runs in it, a quiet dot otherwise.

**Search** opens a find box over the top right of the terminal. Typing searches
the scrollback and the screen, ignoring case, selects the last match and shows
`3/12` style counts. **Enter** or the arrow buttons move to the next or previous
match, wrapping around, and the view scrolls to it. **Escape**, the close button
or the Search button again closes the box and drops the highlight.

**Split** opens a new terminal beside the current one, in the same directory,
under the same header. Their tabs stay adjacent with a short, centered vertical separator
between them. The focused region's active group uses the ordinary selected-tab
background, while its selected session uses the ordinary inactive-tab background
in an inset rounded rectangle. Unfocused regions and unselected groups use the
ordinary tab colors. Hover backgrounds use the same inset shape.
Select or close either session independently. Drag the divider
between their contents to set their widths. Close one and the other takes the
space back.

**Maximize** gives the bottom terminal the editor area's height, down to the
editor's minimum, and the same button restores the previous size. Dragging a
divider ends the maximized state.

## Naming a terminal

**More → Rename Session** gives a terminal a name of your own. The name
replaces the shell's label in the tab, keeps up to 48 characters and returns
with the workspace. Leave it blank to go back to the automatic name. The icon at
the end of that row opens the purpose panel (Development, Server, Testing,
Build, Debugging, Other, or Automatic name), which replaces double-clicking a
tab. A purpose, or the program that is running, still follows the name.

**More → Clear Output** forgets the screen and its scrollback, then asks the
shell to redraw its prompt. A full-screen program is left alone.

## Font size

**Ctrl + +** (or **Ctrl + =**) and **Ctrl + −** adjust the terminal font size
while the terminal has focus. **Ctrl + 0** resets it. On macOS, use Command
instead of Ctrl.

## Scrollback

Terminal panes with history show a faint vertical scrollbar that brightens when
the pointer is in the pane or the thumb is dragged. Drag the thumb to browse
history; it stays bright while dragging outside the pane. Each split terminal
has its own scrollbar. Like the editor, it overlays the text at the right edge
without reserving width or changing the terminal's columns.
