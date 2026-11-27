# Workspace search

Press **Ctrl+Shift+F** (Command+Shift+F on macOS), or open the **Search** bar
under the file tree in the right drawer, to search the contents of every
workspace folder. The section starts collapsed to its bar; opening it takes a
third of the drawer, and dragging the rule above it changes that. The bar shows
the number of matches and, while there is a query, the refresh or stop button and
the clear icon. The search bar
supports match case, whole word and regular expressions. Expand **Filters**
for comma-separated include/exclude globs (`*.rs`, `src/**`, `*.{rs,ts}`), and
options to include hidden or ignored files. Enter or the refresh button runs the
search again; Escape or the stop button stops it. Use the clear icon to clear the
query and results. Active filters show a count and a reset icon. Alt+C, Alt+W,
Alt+R and Alt+F toggle case, whole word, regular expressions and filters.

## Results

Results are grouped by file. Click a match to preview and select it, double-click
to keep its tab, or use Down/Up to navigate results and Enter to open the selected
match. Files already open in Loog use their current buffer, including unsaved
edits. Query and option changes debounce for 200 ms. Search also updates when
workspace buffers or relevant disk files change; ignored build output and
excluded paths do not normally trigger it. Opening other panels, expanding
filters, scrolling, or regaining window focus preserves the current search.
Collapsing the section keeps its results; opening it again shows them.

## What is searched

Search respects `.gitignore`, `.ignore` and `.rgignore` by default, never follows
symbolic links, and always excludes `.git` internals. The first version searches
UTF-8 text files up to 4 MiB and matches within individual lines; binary,
unsupported-encoding and oversized files are skipped with a count in the footer;
expand it for the reason breakdown. Results are limited to 10,000 matches across
5,000 files. Regular expressions use Rust's `regex` syntax (without
lookaround/backreferences). Cross-file replacement is not included.
