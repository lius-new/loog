//! Placeholder entries the Extensions page shows until the extension manager
//! feeds it.
use crate::ui::extensions_binding::{
    Block, Contribution, Entry, Frame, Installed, LogLevel, Market, Notice, Release, Runtime,
    Table, ThemeCard, Update,
};

fn s(value: &str) -> String {
    value.to_owned()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| s(value)).collect()
}

fn rows<const N: usize>(values: &[[&str; N]]) -> Vec<[String; N]> {
    values.iter().map(|row| row.map(|cell| s(cell))).collect()
}

fn releases(values: &[(&str, &str, &str)]) -> Vec<Release> {
    values
        .iter()
        .map(|(version, date, notes)| Release {
            version: s(version),
            date: s(date),
            notes: s(notes),
        })
        .collect()
}

fn location(id: &str) -> String {
    format!("%LOCALAPPDATA%\\Loom\\extensions\\{id}")
}

fn runtime(summary: &str, log: &[(LogLevel, &str)]) -> Runtime {
    Runtime {
        summary: s(summary),
        log: log.iter().map(|(level, text)| (*level, s(text))).collect(),
    }
}

fn commands(values: &[[&str; 3]]) -> Contribution {
    Contribution::Table(Table {
        icon: "command",
        title: "Commands",
        total: values.len(),
        head: ["Command", "Identifier", "Shortcut"],
        mono: [false, true, true],
        rows: rows(values),
        more: 0,
    })
}

fn features(values: &[[&str; 3]]) -> Contribution {
    Contribution::Table(Table {
        icon: "extension-code",
        title: "Language features",
        total: values.len(),
        head: ["Feature", "Applies to", "What it does"],
        mono: [false, false, false],
        rows: rows(values),
        more: 0,
    })
}

fn weave() -> Entry {
    Entry {
        id: s("acme.weave"),
        name: s("Acme Weave"),
        publisher: s("Acme"),
        version: s("0.4.1"),
        tagline: s("Language support for the Weave language."),
        updated: s("2026-10-12"),
        icon: "extension-code",
        icon_image: None,
        tint: 0xa78bfa,
        categories: strings(&["Languages", "Snippets"]),
        readme_document: None,
        changelog_document: None,
        readme: vec![
            Block::Heading(s("Write Weave with confidence")),
            Block::Paragraph(s(
                "Weave gets highlighting, snippets and language intelligence while you type. Open any .wv or .weave file and it is ready; nothing to configure.",
            )),
            Block::Code(strings(&[
                "weave greeting(name) {",
                "  emit \"Hello, \" + name  // snippet: emit",
                "}",
            ])),
            Block::Heading(s("What you get")),
            Block::Bullets(strings(&[
                "Syntax highlighting through a Tree-sitter grammar, including nested string templates.",
                "12 snippets for everyday patterns such as weave, emit and match.",
                "Language server (weave-ls) for completion, hover information, diagnostics and go to definition.",
            ])),
            Block::Heading(s("Good to know")),
            Block::Paragraph(s(
                "The language server is started by Loom when you open a Weave file in a trusted folder. Without it you still keep highlighting and snippets.",
            )),
        ],
        contributions: vec![
            Contribution::Table(Table {
                icon: "extension-code",
                title: "Languages",
                total: 2,
                head: ["Language", "Identifier", "File types"],
                mono: [false, true, true],
                rows: rows(&[
                    ["Weave", "weave", ".wv, .weave"],
                    ["Weave Template", "weave-template", ".wvt"],
                ]),
                more: 0,
            }),
            Contribution::Table(Table {
                icon: "extension-code",
                title: "Grammars",
                total: 1,
                head: ["Grammar", "Module", "Queries"],
                mono: [false, true, false],
                rows: rows(&[["weave", "tree-sitter-weave.wasm", "Highlights, injections"]]),
                more: 0,
            }),
            Contribution::Table(Table {
                icon: "extension-snippet",
                title: "Snippets",
                total: 12,
                head: ["Prefix", "Description", ""],
                mono: [true, false, false],
                rows: rows(&[
                    ["weave", "Declare a weave", ""],
                    ["emit", "Emit a value", ""],
                    ["match", "Match expression", ""],
                    ["loop", "Loop with a counter", ""],
                ]),
                more: 8,
            }),
            Contribution::Table(Table {
                icon: "terminal",
                title: "Language servers",
                total: 1,
                head: ["Server", "Languages", "Project marker"],
                mono: [true, false, true],
                rows: rows(&[["weave-ls", "Weave, Weave Template", "Weave.toml"]]),
                more: 0,
            }),
        ],
        changelog: releases(&[
            ("0.4.1", "2026-10-12", "Fixed hover on multi-line strings."),
            (
                "0.4.0",
                "2026-09-28",
                "Added the match snippet and template highlighting.",
            ),
            ("0.3.0", "2026-09-02", "First language server release."),
        ]),
        installed: Some(Installed {
            enabled: true,
            location: location("acme.weave"),
            update: Some(Update {
                version: s("0.5.0"),
                notes: s("Go to definition across files."),
            }),
            error: None,
            warning: None,
            runtime: runtime(
                "It starts when something it provides is needed, and stops when you disable it.",
                &[
                    (
                        LogLevel::Info,
                        "Ready. The server starts when a Weave file is opened.",
                    ),
                    (
                        LogLevel::Info,
                        "weave-ls started for D:\\work\\site (pid 18420).",
                    ),
                ],
            ),
        }),
        market: Some(Market {
            downloads: s("48K"),
            rating: s("4.7"),
            license: s("MIT"),
        }),
    }
}

fn selection_tools() -> Entry {
    Entry {
        id: s("example.tools"),
        name: s("Selection Tools"),
        publisher: s("Example"),
        version: s("0.1.0"),
        tagline: s("Small tools for everyday editing."),
        updated: s("2026-10-20"),
        icon: "extension-tools",
        icon_image: None,
        tint: 0x60a5fa,
        categories: strings(&["Editing"]),
        readme_document: None,
        changelog_document: None,
        readme: vec![
            Block::Heading(s("Small things, within reach")),
            Block::Paragraph(s(
                "Transform the text you have selected, find forgotten TODO comments and get quick context when you hover a word.",
            )),
            Block::Heading(s("Using it")),
            Block::Bullets(strings(&[
                "Select text and run Uppercase Selection. The change is one undo step.",
                "TODO comments are underlined as warnings in every Rust file.",
                "Hover any identifier for the size of the file and your position in it.",
            ])),
            Block::Heading(s("Add a shortcut")),
            Block::Paragraph(s("Commands can be bound in keymap.json:")),
            Block::Code(strings(&[
                "[{ \"context\": \"Editor\", \"bindings\": {",
                "  \"ctrl-alt-u\": [\"extension::Run\", \"example.tools.uppercase\"] } }]",
            ])),
        ],
        contributions: vec![
            commands(&[
                [
                    "Uppercase Selection",
                    "example.tools.uppercase",
                    "Ctrl+Alt+U",
                ],
                ["Check TODO Comments", "example.tools.check", "not bound"],
                ["Cancel Check", "example.tools.cancel", "not bound"],
            ]),
            features(&[
                ["Diagnostics", "Rust", "Underlines TODO comments"],
                ["Hover", "Rust", "Size and caret position"],
            ]),
        ],
        changelog: releases(&[("0.1.0", "2026-10-20", "First release.")]),
        installed: Some(Installed {
            enabled: true,
            location: location("example.tools"),
            update: None,
            error: None,
            warning: None,
            runtime: runtime(
                "It starts when something it provides is needed, and stops when you disable it.",
                &[
                    (LogLevel::Info, "Activated for Rust."),
                    (LogLevel::Info, "Selection edit applied as one change."),
                    (
                        LogLevel::Warning,
                        "TODO provider: 2 results fell outside the text and were skipped.",
                    ),
                ],
            ),
        }),
        market: None,
    }
}

fn night() -> Entry {
    Entry {
        id: s("acme.night"),
        name: s("Acme Night"),
        publisher: s("Acme"),
        version: s("1.2.0"),
        tagline: s("A quieter palette for late hours."),
        updated: s("2026-09-30"),
        icon: "extension-theme",
        icon_image: None,
        tint: 0xfbbf24,
        categories: strings(&["Themes"]),
        readme_document: None,
        changelog_document: None,
        readme: vec![
            Block::Heading(s("Soft contrast, warm accents")),
            Block::Paragraph(s(
                "Two color themes made for long sessions. Choose them in Settings → Theme. They change colors only; your editor behaves exactly as before.",
            )),
        ],
        contributions: vec![Contribution::Themes(vec![
            ThemeCard {
                name: s("Acme Night"),
                rejected: None,
                background: 0x15120f,
                foreground: 0xe8dccb,
                keyword: 0xf2a65a,
                function: 0x8fb7e8,
                string: 0x9ccf8a,
                comment: 0x7d7266,
                number: 0xe58f65,
            },
            ThemeCard {
                name: s("Acme Dusk"),
                rejected: Some(s(
                    "Not loaded: themes/dusk.json uses an unknown color “bgg”.",
                )),
                background: 0,
                foreground: 0,
                keyword: 0,
                function: 0,
                string: 0,
                comment: 0,
                number: 0,
            },
        ])],
        changelog: releases(&[
            ("1.2.0", "2026-09-30", "Added Acme Dusk (not valid yet)."),
            ("1.0.0", "2026-08-14", "First release."),
        ]),
        installed: Some(Installed {
            enabled: true,
            location: location("acme.night"),
            update: None,
            error: None,
            warning: Some(Notice {
                title: s("One of the two themes could not be loaded."),
                body: s("The rest works. Details are under Contributions."),
            }),
            runtime: runtime(
                "This extension only provides themes. It has nothing to run.",
                &[
                    (LogLevel::Info, "Theme “Acme Night” available."),
                    (
                        LogLevel::Warning,
                        "themes/dusk.json: unknown color “bgg”. This theme was not loaded.",
                    ),
                ],
            ),
        }),
        market: Some(Market {
            downloads: s("12K"),
            rating: s("4.9"),
            license: s("MIT"),
        }),
    }
}

fn lint() -> Entry {
    Entry {
        id: s("acme.lint"),
        name: s("Acme Lint"),
        publisher: s("Acme"),
        version: s("0.9.3"),
        tagline: s("Catch issues before they settle in."),
        updated: s("2026-10-03"),
        icon: "check",
        icon_image: None,
        tint: 0xfb7185,
        categories: strings(&["Linters"]),
        readme_document: None,
        changelog_document: None,
        readme: vec![
            Block::Heading(s("Checks when you save")),
            Block::Paragraph(s(
                "Acme Lint runs your project’s linter every time you save and shows what it finds next to your code, with fixes where they are safe.",
            )),
            Block::Bullets(strings(&[
                "Runs on save, only for files that changed.",
                "Shows each finding with its rule name.",
                "Applies safe fixes as one undo step.",
            ])),
        ],
        contributions: vec![features(&[
            [
                "Save participant",
                "All languages",
                "Runs the linter, applies safe fixes",
            ],
            ["Task", "—", "Runs acme-lint on the workspace folder"],
        ])],
        changelog: releases(&[
            ("0.9.3", "2026-10-03", "Rule names in diagnostics."),
            ("0.9.0", "2026-09-10", "Fix on save."),
        ]),
        installed: Some(Installed {
            enabled: true,
            location: location("acme.lint"),
            update: None,
            error: Some(Notice {
                title: s("This extension could not start."),
                body: s(
                    "It was built for an older interface that Loom no longer offers. Update the extension to a version built for this Loom, then reload. Your files are not affected.",
                ),
            }),
            warning: None,
            runtime: runtime(
                "It starts when something it provides is needed, and stops when you disable it.",
                &[
                    (
                        LogLevel::Error,
                        "Could not start: the extension imports acme:legacy/host, which this version of Loom does not provide.",
                    ),
                    (LogLevel::Info, "Stage: link · attempt 3"),
                ],
            ),
        }),
        market: None,
    }
}

fn notes() -> Entry {
    Entry {
        id: s("acme.notes"),
        name: s("Quick Notes"),
        publisher: s("Acme"),
        version: s("2.0.0"),
        tagline: s("A place for the passing thought."),
        updated: s("2026-07-21"),
        icon: "extension-note",
        icon_image: None,
        tint: 0x34d399,
        categories: strings(&["Productivity"]),
        readme_document: None,
        changelog_document: None,
        readme: vec![
            Block::Heading(s("Notes that stay with the project")),
            Block::Paragraph(s(
                "Capture an idea without leaving the editor. Notes are stored per folder, so each project keeps its own.",
            )),
        ],
        contributions: vec![commands(&[
            ["New Note", "acme.notes.new", "Ctrl+Alt+N"],
            ["Open Notes", "acme.notes.open", "not bound"],
        ])],
        changelog: releases(&[("2.0.0", "2026-07-21", "Notes are stored per folder.")]),
        installed: Some(Installed {
            enabled: false,
            location: location("acme.notes"),
            update: None,
            error: None,
            warning: None,
            runtime: runtime(
                "It starts when something it provides is needed, and stops when you disable it.",
                &[(LogLevel::Info, "Disabled.")],
            ),
        }),
        market: None,
    }
}

/// An extension only the marketplace lists.
#[allow(clippy::too_many_arguments)]
fn offered(
    id: &str,
    name: &str,
    publisher: &str,
    version: &str,
    tagline: &str,
    icon: &'static str,
    tint: u32,
    categories: &[&str],
    market: (&str, &str, &str),
    updated: &str,
    readme: Vec<Block>,
    contributions: Vec<Contribution>,
    changelog: Vec<Release>,
) -> Entry {
    Entry {
        id: s(id),
        name: s(name),
        publisher: s(publisher),
        version: s(version),
        tagline: s(tagline),
        updated: s(updated),
        icon,
        icon_image: None,
        tint,
        categories: strings(categories),
        readme_document: None,
        changelog_document: None,
        readme,
        contributions,
        changelog,
        installed: None,
        market: Some(Market {
            downloads: s(market.0),
            rating: s(market.1),
            license: s(market.2),
        }),
    }
}

fn marketplace(installed: &[Entry]) -> Vec<Entry> {
    let mut listed = vec![
        offered(
            "ridge.bracket",
            "Bracket Colors",
            "Ridge",
            "1.4.2",
            "Color matching brackets by depth.",
            "extension-code",
            0x38bdf8,
            &["Editing"],
            ("210K", "4.8", "Apache-2.0"),
            "2026-10-18",
            vec![
                Block::Heading(s("See nesting at a glance")),
                Block::Paragraph(s(
                    "Every pair of brackets gets its own color, so you can tell which one closes which, even in deeply nested code.",
                )),
                Block::Code(strings(&["map(|x| call(x, [1, (2, 3)]))"])),
                Block::Bullets(strings(&[
                    "Works with every language Loom highlights.",
                    "Choose how many colors cycle.",
                ])),
            ],
            vec![
                commands(&[["Toggle Bracket Colors", "ridge.bracket.toggle", "not bound"]]),
                features(&[["Highlighting", "All languages", "Colors brackets by depth"]]),
            ],
            releases(&[
                ("1.4.2", "2026-10-18", "Faster on very long lines."),
                ("1.4.0", "2026-09-02", "Configurable color cycle."),
            ]),
        ),
        offered(
            "north.blame",
            "Git Blame Lens",
            "North",
            "0.8.0",
            "Who changed this line, and why.",
            "git-branch",
            0xfb923c,
            &["Source control"],
            ("96K", "4.5", "MIT"),
            "2026-10-09",
            vec![
                Block::Heading(s("Authorship where you read")),
                Block::Paragraph(s(
                    "Shows the author, date and commit message of the current line at the end of the line. Click it to open the commit.",
                )),
            ],
            vec![
                commands(&[["Toggle Blame", "north.blame.toggle", "Ctrl+Alt+B"]]),
                features(&[["Hover", "All languages", "Commit details for the line"]]),
            ],
            releases(&[("0.8.0", "2026-10-09", "Shows the commit message.")]),
        ),
        offered(
            "tidy.markdown",
            "Markdown Tables",
            "Tidy",
            "2.1.0",
            "Align and edit Markdown tables.",
            "extension-snippet",
            0x34d399,
            &["Markdown"],
            ("33K", "4.6", "MIT"),
            "2026-08-30",
            vec![
                Block::Heading(s("Tables that stay tidy")),
                Block::Paragraph(s(
                    "Format a table with one command and move between cells with Tab.",
                )),
            ],
            vec![commands(&[
                ["Format Table", "tidy.markdown.format", "not bound"],
                ["Next Cell", "tidy.markdown.next", "Tab"],
            ])],
            releases(&[("2.1.0", "2026-08-30", "Tab moves between cells.")]),
        ),
        offered(
            "ridge.lint",
            "Rust Clippy Lens",
            "Ridge",
            "0.3.1",
            "Clippy findings with one-click fixes.",
            "check",
            0xf472b6,
            &["Linters", "Rust"],
            ("8K", "4.2", "MIT"),
            "2026-10-11",
            vec![
                Block::Heading(s("Clippy in the editor")),
                Block::Paragraph(s(
                    "Shows clippy lints on save and applies the suggested fix.",
                )),
            ],
            vec![features(&[[
                "Save participant",
                "Rust",
                "Runs clippy, applies safe fixes",
            ]])],
            releases(&[("0.3.1", "2026-10-11", "Fixed a path with spaces.")]),
        ),
    ];
    // The listing of an extension that is already installed carries its
    // marketplace figures and the newest version on offer.
    for have in installed {
        let Some(market) = &have.market else { continue };
        let mut entry = have.clone();
        entry.market = Some(market.clone());
        if let Some(update) = have.installed.as_ref().and_then(|i| i.update.as_ref()) {
            entry.version = update.version.clone();
            entry.updated = s("2026-10-26");
            entry.changelog.insert(
                0,
                Release {
                    version: update.version.clone(),
                    date: s("2026-10-26"),
                    notes: update.notes.clone(),
                },
            );
        }
        listed.insert(0, entry);
    }
    listed
}

pub fn frame() -> Frame {
    let installed = vec![weave(), selection_tools(), night(), lint(), notes()];
    let marketplace = marketplace(&installed);
    Frame {
        installed,
        marketplace,
    }
}
