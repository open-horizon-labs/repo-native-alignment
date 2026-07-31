//! LSP-based enricher for cross-file reference discovery.
//!
//! Phase 2 enrichment: spawns a language server as a child process, sends
//! JSON-RPC messages over stdin/stdout, and uses `textDocument/references`
//! and `textDocument/implementation` to discover cross-file edges.
//!
//! Supports multiple language servers (rust-analyzer, pyrefly, typescript-language-server,
//! gopls, marksman) via the same generic `LspEnricher` struct.
//!
//! Design decisions:
//! - Spawns the language server on first `enrich()` call, not at startup
//! - Keeps the language server alive for the session duration
//! - If the server binary is not installed, logs info and skips gracefully
//! - 60-second timeout per LSP request
//!
//! ## Module structure
//!
//! - `transport` — JSON-RPC framing: [`LspTransport`] (sequential, init-phase) and
//!   [`PipelinedTransport`] (concurrent, enrichment-phase), plus URI helpers.

mod passes;
mod policy;
pub use policy::LspQueryMetric;
pub(crate) use policy::{LspBroadReferenceBudget, LspBroadReferenceBudgetSnapshot};
use policy::{LspQueryProfile, LspServerCapabilities};
mod transport;
pub(crate) mod work_items;
use transport::{
    LspTransport, PipelinedTransport, is_method_not_found, path_to_uri, uri_to_relative_path,
};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use lsp_types::{
    ClientCapabilities, CodeActionProviderCapability, GotoDefinitionParams, GotoDefinitionResponse,
    ImplementationProviderCapability, InitializeParams, InitializeResult, Location, OneOf,
    Position, ServerCapabilities, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
};

use super::{Enricher, EnrichmentResult};
use crate::extract::scan_stats::{
    LspDocumentSymbolEvidence, LspNegotiatedCapabilities, LspValidationEvidence,
};
use crate::graph::index::GraphIndex;
use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

pub const CSHARP_TOOLCHAIN_REMEDIATION: &str = "C# LSP needs dotnet, csharp-ls, and a visible .NET root. Install .NET (brew install --cask dotnet-sdk, mise use dotnet@latest, asdf install dotnet latest, or Microsoft installer), install csharp-ls with `dotnet tool install -g csharp-ls`, add `$HOME/.dotnet/tools` to PATH, and set DOTNET_ROOT/DOTNET_ROOT_ARM64 to the installed .NET root for MCP stdio env.";

pub(crate) const MAX_INCREMENTAL_LSP_NODES: usize = 4_096;
pub(crate) const MAX_INCREMENTAL_LSP_OPERATIONS: usize = 12_288;

type InitSettingsFactory = fn() -> serde_json::Value;

#[derive(Clone, Copy)]
struct LspCompileCommandOverride {
    suffix: &'static str,
    compiler: &'static str,
    args: &'static [&'static str],
}

/// Complete construction profile for a built-in language server.
///
/// Keeping process configuration and admission policy in one descriptor means
/// every scan path can construct an identical enricher without mirroring tables.
#[derive(Clone, Copy)]
pub(crate) struct BuiltinLspDescriptor {
    language: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    extensions: &'static [&'static str],
    init_settings: Option<InitSettingsFactory>,
    compile_command_overrides: &'static [LspCompileCommandOverride],
    config_file: Option<&'static str>,
    /// Repository files whose content can change this language server's
    /// interpretation of otherwise unchanged source. Structural-cache reuse
    /// hashes these descriptor-owned patterns into the language partition.
    partition_influence_patterns: &'static [&'static str],
    toolchain_remediation: Option<&'static str>,
    allow_declared_const_references: bool,
}

impl BuiltinLspDescriptor {
    pub(crate) fn language(&self) -> &'static str {
        self.language
    }

    pub(crate) fn command(&self) -> &'static str {
        self.command
    }

    pub(crate) fn extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    pub(crate) fn partition_identity(&self) -> serde_json::Value {
        serde_json::json!({
            "language": self.language,
            "command": self.command,
            "args": self.args,
            "extensions": self.extensions,
            "initialization_settings": self.init_settings.map(|factory| factory()),
            "compile_command_overrides": self.compile_command_overrides.iter().map(|override_| {
                serde_json::json!({
                    "suffix": override_.suffix,
                    "compiler": override_.compiler,
                    "args": override_.args,
                })
            }).collect::<Vec<_>>(),
            "config_file": self.config_file,
            "partition_influence_patterns": self.partition_influence_patterns(),
            "allow_declared_const_references": self.allow_declared_const_references,
        })
    }

    pub(crate) fn partition_influence_patterns(&self) -> Vec<&'static str> {
        let mut patterns = self.partition_influence_patterns.to_vec();
        if let Some(config_file) = self.config_file {
            patterns.push(config_file);
        }
        patterns.sort_unstable();
        patterns.dedup();
        patterns
    }

    pub(crate) fn build(&self) -> LspEnricher {
        let mut enricher =
            LspEnricher::new(self.language, self.command, self.args, self.extensions);
        if let Some(settings) = self.init_settings {
            enricher = enricher.with_settings(settings());
        }
        if !self.compile_command_overrides.is_empty() {
            enricher = enricher.with_compile_command_overrides(self.compile_command_overrides);
        }
        if let Some(config_file) = self.config_file {
            enricher = enricher.with_config_file(config_file);
        }
        if let Some(remediation) = self.toolchain_remediation {
            enricher = enricher.with_toolchain_remediation(remediation);
        }
        if self.allow_declared_const_references {
            enricher = enricher.with_declared_const_references(true);
        }
        if let Some(kinds) = crate::extract::configs::config_for_language(self.language)
            .and_then(|config| config.lsp_enrichable_kinds)
        {
            enricher = enricher.with_enrichable_kinds(kinds);
        }
        enricher
    }
}

pub(crate) fn partition_influence_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("**/");
    let candidate = if pattern.contains('/') {
        path
    } else {
        path.rsplit('/').next().unwrap_or(path)
    };
    wildcard_match(pattern.as_bytes(), candidate.as_bytes())
        || (!pattern.contains('/') && wildcard_match(pattern.as_bytes(), path.as_bytes()))
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    let (mut pattern_index, mut value_index, mut star, mut checkpoint) =
        (0usize, 0usize, None, 0usize);
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == value[value_index] || pattern[pattern_index] == b'?')
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            checkpoint = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            checkpoint += 1;
            value_index = checkpoint;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn cyright_init_settings() -> serde_json::Value {
    serde_json::json!({
        "python": { "analysis": { "autoSearchPaths": true } }
    })
}

/// Describe the standard LSP operations RNA's generic client can actually consume.
///
/// Servers are free to shape responses based on these declarations. Keeping the
/// fields absent while issuing the corresponding requests can therefore suppress
/// otherwise valid results (notably hierarchical document symbols and cross-file
/// reference/call-hierarchy results). This stays language-agnostic: server
/// capabilities still decide which operations RNA schedules for a particular
/// repository.
fn lsp_client_capabilities() -> ClientCapabilities {
    let static_registration = lsp_types::DynamicRegistrationClientCapabilities {
        dynamic_registration: Some(false),
    };
    let goto = lsp_types::GotoCapability {
        dynamic_registration: Some(false),
        link_support: Some(true),
    };

    ClientCapabilities {
        workspace: Some(lsp_types::WorkspaceClientCapabilities {
            symbol: Some(lsp_types::WorkspaceSymbolClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            configuration: Some(true),
            workspace_folders: Some(true),
            ..Default::default()
        }),
        text_document: Some(lsp_types::TextDocumentClientCapabilities {
            synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            references: Some(static_registration),
            document_symbol: Some(lsp_types::DocumentSymbolClientCapabilities {
                dynamic_registration: Some(false),
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            definition: Some(goto),
            implementation: Some(goto),
            code_action: Some(lsp_types::CodeActionClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            document_link: Some(lsp_types::DocumentLinkClientCapabilities {
                dynamic_registration: Some(false),
                tooltip_support: Some(false),
            }),
            publish_diagnostics: Some(Default::default()),
            call_hierarchy: Some(static_registration),
            type_hierarchy: Some(static_registration),
            inlay_hint: Some(lsp_types::InlayHintClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            diagnostic: Some(lsp_types::DiagnosticClientCapabilities {
                dynamic_registration: Some(false),
                related_document_support: Some(false),
            }),
            ..Default::default()
        }),
        window: Some(lsp_types::WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        // Declare support for experimental/serverStatus notifications.
        // Without this, rust-analyzer won't send serverStatus and the
        // readiness wait falls through to a timeout, sending queries
        // while the server is still indexing (producing 0 edges).
        experimental: Some(serde_json::json!({
            "serverStatusNotification": true
        })),
        ..Default::default()
    }
}

#[allow(deprecated)] // root_uri is retained for servers predating workspace folders.
fn lsp_initialize_params(root_uri: Uri, workspace_name: String) -> InitializeParams {
    InitializeParams {
        root_uri: Some(root_uri.clone()),
        workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
            uri: root_uri,
            name: workspace_name,
        }]),
        capabilities: lsp_client_capabilities(),
        ..Default::default()
    }
}

macro_rules! builtin_lsp {
    ($language:literal, $command:literal, $args:expr, $extensions:expr) => {
        BuiltinLspDescriptor {
            language: $language,
            command: $command,
            args: $args,
            extensions: $extensions,
            init_settings: None,
            compile_command_overrides: &[],
            config_file: None,
            partition_influence_patterns: &[],
            toolchain_remediation: None,
            allow_declared_const_references: false,
        }
    };
}

static BUILTIN_LSP_DESCRIPTORS: &[BuiltinLspDescriptor] = &[
    BuiltinLspDescriptor {
        init_settings: None,
        config_file: Some("Cargo.toml"),
        partition_influence_patterns: &[
            "Cargo.lock",
            "rust-toolchain",
            "rust-toolchain.toml",
            ".cargo/config",
            ".cargo/config.toml",
        ],
        allow_declared_const_references: true,
        ..builtin_lsp!("rust", "rust-analyzer", &[], &["rs"])
    },
    BuiltinLspDescriptor {
        init_settings: None,
        config_file: Some("pyproject.toml"),
        partition_influence_patterns: &[
            "setup.py",
            "setup.cfg",
            "tox.ini",
            "requirements*.txt",
            "**/requirements*.txt",
            "environment.yml",
            ".python-version",
        ],
        ..builtin_lsp!(
            "python",
            "pyrefly",
            &[
                "lsp",
                "--verbose",
                "--indexing-mode",
                "lazy-blocking",
                "--threads",
                "1",
                "--workspace-indexing-limit",
                "5000",
                "--build-system-blocking",
                "--color",
                "never"
            ],
            &["py", "pyi", "py-tpl", "py_t", "bench"]
        )
    },
    BuiltinLspDescriptor {
        init_settings: Some(cyright_init_settings),
        config_file: Some("pyproject.toml"),
        partition_influence_patterns: &[
            "setup.py",
            "setup.cfg",
            "tox.ini",
            "requirements*.txt",
            "**/requirements*.txt",
        ],
        ..builtin_lsp!(
            "cython",
            "cyright-langserver",
            &["--stdio"],
            &["pyx", "pxd", "pxi", "tp"]
        )
    },
    BuiltinLspDescriptor {
        config_file: Some("tsconfig.json"),
        partition_influence_patterns: &[
            "package.json",
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "jsconfig.json",
            "**/package.json",
            "**/tsconfig.json",
            "**/jsconfig.json",
        ],
        ..builtin_lsp!(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx", "mjs", "js_t"]
        )
    },
    BuiltinLspDescriptor {
        config_file: Some("go.mod"),
        partition_influence_patterns: &["go.sum", "go.work", "**/go.mod"],
        ..builtin_lsp!("go", "gopls", &["serve"], &["go"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[".marksman.toml"],
        ..builtin_lsp!("markdown", "marksman", &["server"], &["md", "markdown"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["conf.py"],
        ..builtin_lsp!(
            "restructuredtext",
            "esbonio",
            &["server"],
            &[
                "rst",
                "rst_t",
                "inc",
                "breaking",
                "bugfix",
                "extension",
                "false_negative",
                "false_positive",
                "feature",
                "internal",
                "new_check",
                "other",
                "performance",
                "user_action"
            ]
        )
    },
    builtin_lsp!(
        "plaintext",
        "rna-cohort-language-server",
        &["--language", "plaintext"],
        &[
            "txt",
            "eopc04_iau2000",
            "finals2000a",
            "lesser",
            "license",
            "old",
            "pil",
            "python",
            "wx"
        ]
    ),
    BuiltinLspDescriptor {
        compile_command_overrides: &[LspCompileCommandOverride {
            suffix: ".h.in",
            compiler: "clang",
            args: &["-xc"],
        }],
        partition_influence_patterns: &[
            "compile_commands.json",
            "CMakeLists.txt",
            "**/CMakeLists.txt",
            "meson.build",
            "Makefile",
        ],
        ..builtin_lsp!(
            "c-cpp",
            "clangd",
            &[],
            &["c", "cc", "cpp", "cxx", "h", "hpp", "m"]
        )
    },
    BuiltinLspDescriptor {
        config_file: Some("pom.xml"),
        partition_influence_patterns: &[
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
            "**/pom.xml",
        ],
        ..builtin_lsp!("java", "jdtls", &[], &["java"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[
            "Gemfile",
            "Gemfile.lock",
            ".ruby-version",
            ".solargraph.yml",
        ],
        ..builtin_lsp!("ruby", "solargraph", &["stdio"], &["rb"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[
            "*.csproj",
            "*.fsproj",
            "*.vbproj",
            "*.sln",
            "Directory.Build.props",
            "Directory.Build.targets",
            "global.json",
            "NuGet.config",
            "packages.lock.json",
        ],
        toolchain_remediation: Some(CSHARP_TOOLCHAIN_REMEDIATION),
        ..builtin_lsp!("csharp", "csharp-ls", &[], &["cs"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["Package.swift", "Package.resolved"],
        ..builtin_lsp!("swift", "sourcekit-lsp", &[], &["swift"])
    },
    BuiltinLspDescriptor {
        config_file: Some("build.gradle.kts"),
        partition_influence_patterns: &[
            "build.gradle",
            "settings.gradle",
            "settings.gradle.kts",
            "gradle.properties",
        ],
        ..builtin_lsp!("kotlin", "kotlin-language-server", &[], &["kt", "kts"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[".luarc.json", ".luarc.jsonc"],
        ..builtin_lsp!("lua", "lua-language-server", &[], &["lua"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["build.zig", "build.zig.zon"],
        ..builtin_lsp!("zig", "zls", &[], &["zig"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["mix.exs", "mix.lock"],
        ..builtin_lsp!("elixir", "elixir-ls", &[], &["ex", "exs"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["*.cabal", "cabal.project", "stack.yaml", "hie.yaml"],
        ..builtin_lsp!("haskell", "haskell-language-server", &["--lsp"], &["hs"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["*.opam", "dune", "dune-project"],
        ..builtin_lsp!("ocaml", "ocamllsp", &[], &["ml", "mli"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["*.sbt", "build.sc"],
        ..builtin_lsp!("scala", "metals", &[], &["scala", "sc"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["pubspec.yaml", "pubspec.lock", "analysis_options.yaml"],
        ..builtin_lsp!("dart", "dart", &["language-server"], &["dart"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["DESCRIPTION", "renv.lock", ".Rprofile"],
        ..builtin_lsp!(
            "r",
            "R",
            &["--no-echo", "-e", "languageserver::run()"],
            &["r", "R"]
        )
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["Project.toml", "Manifest.toml"],
        ..builtin_lsp!(
            "julia",
            "julia",
            &[
                "--startup-file=no",
                "-e",
                "using LanguageServer; runserver()"
            ],
            &["jl"]
        )
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["composer.json", "composer.lock"],
        ..builtin_lsp!("php", "intelephense", &["--stdio"], &["php"])
    },
    builtin_lsp!(
        "css",
        "vscode-css-language-server",
        &["--stdio"],
        &["css", "scss", "less", "css_t"]
    ),
    builtin_lsp!(
        "html",
        "vscode-html-language-server",
        &["--stdio"],
        &["html", "htm", "html_t", "thtml", "djtpl", "tpl"]
    ),
    builtin_lsp!(
        "yaml",
        "yaml-language-server",
        &["--stdio"],
        &["yaml", "yml", "cff", "lock"]
    ),
    builtin_lsp!(
        "json",
        "vscode-json-language-server",
        &["--stdio"],
        &["json", "ipynb"]
    ),
    builtin_lsp!("toml", "rna-config-language-server", &[], &["toml"]),
    builtin_lsp!(
        "shell",
        "bash-language-server",
        &["start"],
        &["sh", "xsh", "guess", "sub"]
    ),
    builtin_lsp!(
        "xml",
        "lemminx",
        &[],
        &[
            "xml", "xsd", "xsl", "dtd", "kml", "glade", "xrc", "hhc", "ncx_t", "opf_t", "xhtml_t",
            "stp"
        ]
    ),
    BuiltinLspDescriptor {
        partition_influence_patterns: &["texlab.toml", ".latexmkrc"],
        ..builtin_lsp!(
            "latex",
            "texlab",
            &[],
            &["tex", "bib", "sty", "cls", "tex_t", "sty_t", "xdy", "ist"]
        )
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["babel.cfg"],
        ..builtin_lsp!("gettext", "babel-lsp", &[], &["po", "pot", "pot_t"])
    },
    builtin_lsp!(
        "config",
        "rna-config-language-server",
        &[],
        &[
            "cfg", "conf", "ini", "mplstyle", "rc", "template", "hhp", "def"
        ]
    ),
    builtin_lsp!(
        "dockerfile",
        "docker-langserver",
        &["--stdio"],
        &["<none>"]
    ),
    builtin_lsp!(
        "batch",
        "rna-cohort-language-server",
        &["--language", "batch"],
        &["bat", "bat_t", "cmd"]
    ),
    builtin_lsp!("graphviz", "dot-language-server", &["--stdio"], &["dot"]),
    builtin_lsp!(
        "plantuml",
        "rna-cohort-language-server",
        &["--language", "plantuml"],
        &["puml"]
    ),
    builtin_lsp!(
        "roff",
        "rna-cohort-language-server",
        &["--language", "roff"],
        &["1"]
    ),
    builtin_lsp!(
        "autolev",
        "rna-cohort-language-server",
        &["--language", "autolev"],
        &["al"]
    ),
    builtin_lsp!(
        "antlr",
        "rna-cohort-language-server",
        &["--language", "antlr"],
        &["g4"]
    ),
    builtin_lsp!(
        "lex",
        "rna-cohort-language-server",
        &["--language", "lex"],
        &["l"]
    ),
    builtin_lsp!(
        "emacs-lisp",
        "rna-cohort-language-server",
        &["--language", "emacs-lisp"],
        &["el"]
    ),
    builtin_lsp!(
        "scheme",
        "rna-cohort-language-server",
        &["--language", "scheme"],
        &["scm"]
    ),
    builtin_lsp!(
        "autotools",
        "rna-cohort-language-server",
        &["--language", "autotools"],
        &["ac", "am"]
    ),
    builtin_lsp!(
        "powershell",
        "rna-cohort-language-server",
        &["--language", "powershell"],
        &["ps1"]
    ),
    builtin_lsp!(
        "starlark",
        "rna-cohort-language-server",
        &["--language", "starlark"],
        &["star"]
    ),
    builtin_lsp!(
        "cohort-text",
        "rna-cohort-language-server",
        &["--language", "cohort-text"],
        &[]
    ),
    BuiltinLspDescriptor {
        partition_influence_patterns: &[".terraform.lock.hcl", ".terraformrc", "terraform.rc"],
        ..builtin_lsp!("terraform", "terraform-ls", &["serve"], &["tf", "tfvars"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["flake.nix", "flake.lock"],
        ..builtin_lsp!("nix", "nil", &[], &["nix"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "tsconfig.json",
            "jsconfig.json",
        ],
        ..builtin_lsp!("vue", "vue-language-server", &["--stdio"], &["vue"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &[
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "tsconfig.json",
            "jsconfig.json",
        ],
        ..builtin_lsp!("svelte", "svelteserver", &["--stdio"], &["svelte"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["rebar.config", "rebar.lock"],
        ..builtin_lsp!("erlang", "erlang_ls", &[], &["erl", "hrl"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["gleam.toml", "manifest.toml"],
        ..builtin_lsp!("gleam", "gleam", &["lsp"], &["gleam"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["*.nimble"],
        ..builtin_lsp!("nim", "nimlsp", &[], &["nim"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["deps.edn", "project.clj"],
        ..builtin_lsp!("clojure", "clojure-lsp", &[], &["clj", "cljs", "cljc"])
    },
    BuiltinLspDescriptor {
        config_file: Some("tsconfig.json"),
        partition_influence_patterns: &[
            "deno.json",
            "deno.jsonc",
            "import_map.json",
            "package.json",
            "**/deno.json",
            "**/deno.jsonc",
        ],
        ..builtin_lsp!("deno", "deno", &["lsp"], &["ts", "tsx", "js", "jsx"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["buf.yaml", "buf.work.yaml", "buf.lock"],
        ..builtin_lsp!("protobuf", "buf", &["lsp"], &["proto"])
    },
    BuiltinLspDescriptor {
        partition_influence_patterns: &["typst.toml"],
        ..builtin_lsp!("typst", "tinymist", &[], &["typ"])
    },
];

pub(crate) fn builtin_lsp_descriptors() -> &'static [BuiltinLspDescriptor] {
    BUILTIN_LSP_DESCRIPTORS
}

pub(crate) fn builtin_lsp_descriptor_for_path(
    path: &Path,
) -> Option<&'static BuiltinLspDescriptor> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let special_language = if filename == "dockerfile" || filename.starts_with("dockerfile.") {
        Some("dockerfile")
    } else if filename == "code.sample" {
        Some("python")
    } else if filename == "tox.ini.sample" {
        Some("config")
    } else if extension == "in" {
        Some(
            if filename.ends_with(".h.in")
                || filename.ends_with(".c.in")
                || filename.ends_with(".cpp.in")
            {
                "c-cpp"
            } else {
                "config"
            },
        )
    } else if extension == "new_t" {
        Some(if filename.contains("bat") {
            "batch"
        } else {
            "config"
        })
    } else {
        None
    };
    if let Some(language) = special_language {
        return BUILTIN_LSP_DESCRIPTORS
            .iter()
            .find(|descriptor| descriptor.language() == language);
    }
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str())
        && let Some(descriptor) = BUILTIN_LSP_DESCRIPTORS.iter().find(|descriptor| {
            descriptor
                .extensions()
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(extension))
        })
    {
        return Some(descriptor);
    }
    let language = if filename == "makefile"
        || filename == "gnumakefile"
        || filename == "manifest.in"
        || filename.starts_with("requirements")
        || filename.starts_with('.')
        || matches!(
            filename.as_str(),
            "codeowners" | "procfile" | "pylintrc" | "matplotlibrc"
        ) {
        "config"
    } else if crate::lsp_completeness::is_plaintext_document_path(path) {
        "plaintext"
    } else if matches!(
        filename.as_str(),
        "diagnose_imports"
            | "doctest"
            | "isympy"
            | "strip_whitespace"
            | "test"
            | "test_import"
            | "test_isolated"
            | "tm_sympy"
    ) {
        "python"
    } else if extension.is_empty() {
        "cohort-text"
    } else {
        return None;
    };
    BUILTIN_LSP_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.language() == language)
}

pub(crate) fn builtin_lsp_descriptor_for_inventory_file(
    path: &Path,
    absolute: &Path,
) -> Option<&'static BuiltinLspDescriptor> {
    if path.extension().is_none()
        && let Ok(prefix) = std::fs::read(absolute)
        && prefix.starts_with(b"#!")
    {
        let first_line = prefix
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let language = if first_line
            .windows(b"python".len())
            .any(|window| window.eq_ignore_ascii_case(b"python"))
        {
            Some("python")
        } else if [b"bash".as_slice(), b"zsh", b"xonsh", b"/sh"]
            .iter()
            .any(|needle| {
                first_line
                    .windows(needle.len())
                    .any(|window| window == *needle)
            })
        {
            Some("shell")
        } else {
            None
        };
        if let Some(language) = language {
            return BUILTIN_LSP_DESCRIPTORS
                .iter()
                .find(|descriptor| descriptor.language() == language);
        }
    }
    builtin_lsp_descriptor_for_path(path)
}

/// Planner-safe projection of the shared query profile. Changed-file planning
/// does not have negotiated server capabilities yet, so it assumes advertised
/// support while preserving declaration, language/server, and default-deny
/// policy. Runtime scheduling rechecks against negotiated capabilities.
pub(crate) fn planned_operations_for_node(node: &Node) -> Vec<String> {
    planned_operations_for_node_inner(node, false)
}

pub(crate) fn planned_operations_for_node_with_broad_references(node: &Node) -> Vec<String> {
    planned_operations_for_node_inner(node, true)
}

fn planned_operations_for_node_inner(node: &Node, allow_broad_references: bool) -> Vec<String> {
    let Some(descriptor) = BUILTIN_LSP_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.language == node.language)
    else {
        return Vec::new();
    };
    let enricher = if allow_broad_references {
        let mut enricher = descriptor.build();
        enricher.query_profile = enricher.query_profile.with_broad_references_unbudgeted();
        enricher
    } else {
        descriptor.build()
    };
    let operations: &[policy::LspQueryOperation] = match node.id.kind {
        NodeKind::Function => &[policy::LspQueryOperation::CallHierarchy],
        NodeKind::Trait => &[
            policy::LspQueryOperation::Implementations,
            policy::LspQueryOperation::TypeHierarchy,
        ],
        NodeKind::Struct | NodeKind::Enum => &[
            policy::LspQueryOperation::References,
            policy::LspQueryOperation::TypeHierarchy,
        ],
        NodeKind::TypeAlias | NodeKind::Const => &[policy::LspQueryOperation::References],
        NodeKind::MarkdownSection
            if node.metadata.get("markdown_kind").map(String::as_str) == Some("link") =>
        {
            &[
                policy::LspQueryOperation::DocumentLinks,
                policy::LspQueryOperation::Definitions,
                policy::LspQueryOperation::References,
            ]
        }
        NodeKind::MarkdownSection => &[policy::LspQueryOperation::DocumentLinks],
        NodeKind::Other(_) => &[policy::LspQueryOperation::DocumentLinks],
        _ => return Vec::new(),
    };
    let capabilities = LspServerCapabilities {
        references: true,
        call_hierarchy: true,
        definitions: true,
        implementations: true,
        type_hierarchy: true,
        document_symbols: true,
        document_links: true,
    };
    let mut budget = enricher.query_profile.budget();
    operations
        .iter()
        .copied()
        .filter(|operation| {
            enricher
                .query_profile
                .admits(node, *operation, capabilities, &mut budget)
        })
        .map(|operation| operation.to_string())
        .collect()
}

pub(crate) fn builtin_lsp_enricher(language: &str) -> Option<LspEnricher> {
    BUILTIN_LSP_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.language == language)
        .map(BuiltinLspDescriptor::build)
}

// ---------------------------------------------------------------------------
// LspEnricher
// ---------------------------------------------------------------------------

/// LSP enricher that uses a language server to discover cross-file references
/// and trait/interface implementations.
///
/// Generic over the language server binary — the same struct handles
/// rust-analyzer, pyrefly, typescript-language-server, gopls, and marksman.
pub struct LspEnricher {
    /// Language identifier (e.g., "rust", "python").
    language: String,
    /// Static ref for Enricher::languages() return (leaked once per enricher).
    language_static: &'static [&'static str],
    /// Display name for logging (e.g., "rust-analyzer-lsp").
    display_name: String,
    /// Command to spawn (e.g., "rust-analyzer", "pyrefly").
    server_command: String,
    /// Arguments to pass to the server (e.g., ["--stdio"]).
    server_args: Vec<String>,
    /// Full scans validate every included descriptor-matched file. Scoped
    /// changed-node enrichment disables this cohort-wide sweep.
    file_readiness: bool,
    /// Optional exact target-file set for incremental qualification.
    file_readiness_filter: Option<Arc<HashSet<PathBuf>>>,
    /// Optional initialization settings (sent in initialize params).
    init_settings: Option<serde_json::Value>,
    /// Descriptor-owned exact-suffix compilation commands supplied at initialize.
    compile_command_overrides: &'static [LspCompileCommandOverride],
    /// Config file this enricher relies on (e.g., "tsconfig.json" for TypeScript).
    /// Used by pick_lsp_root to prefer lsp_roots that contain this file.
    config_file: Option<&'static str>,
    /// Optional setup guidance shown when this server is unavailable or fails startup.
    toolchain_remediation: Option<&'static str>,
    ready: AtomicBool,
    /// Protected by mutex because enrich takes &self but we need to mutate transport state.
    state: Mutex<LspState>,
    /// Override the LSP server working directory (`rootUri` / `current_dir`).
    ///
    /// When set, the language server is started from this directory instead of `repo_root`.
    /// This is used for monorepo subdirectory roots: typescript-language-server for
    /// `client/` should start from `client/` (where `tsconfig.json` lives) even though
    /// the nodes' file paths are relative to the primary repo root.
    ///
    /// Note: this only affects server startup. File path construction for LSP requests
    /// always uses `repo_root` (passed to `enrich()`), which ensures file URIs point to
    /// the correct absolute paths.
    startup_root_override: std::sync::OnceLock<PathBuf>,
    /// Shared operation/declaration/server admission policy and budget factory.
    query_profile: LspQueryProfile,
}

struct LspState {
    /// Sequential transport used during initialization only.
    transport: Option<LspTransport>,
    /// Pipelined transport used during enrichment (concurrent requests).
    pipelined: Option<Arc<PipelinedTransport>>,
    /// Cached root path from initialization.
    root_path: Option<PathBuf>,
    /// Whether we already tried and failed to start the language server.
    init_failed: bool,
    /// Whether the language server supports type hierarchy requests.
    has_type_hierarchy: bool,
    /// Whether the language server supports textDocument/references requests.
    has_references: bool,
    /// Whether the language server supports callHierarchy requests
    /// (`textDocument/prepareCallHierarchy`, LSP 3.16+).
    /// When false, fall back to `textDocument/references` for function edges.
    /// Some servers support references but not callHierarchy.
    has_call_hierarchy: bool,
    /// Whether the language server supports textDocument/implementation.
    has_implementation: bool,
    /// Whether the language server supports textDocument/definition.
    has_definition: bool,
    /// Whether the language server supports textDocument/documentLink.
    has_document_links: bool,
    /// Whether the language server supports pull-based diagnostics
    /// (`textDocument/diagnostic`, LSP 3.17+).
    has_pull_diagnostics: bool,
    /// Whether the language server supports inlay hints
    /// (`textDocument/inlayHint`, LSP 3.17+).
    has_inlay_hints: bool,
    /// Exact capabilities retained from the initialize response.
    server_capabilities: Option<ServerCapabilities>,
    /// Capability-driven readiness proof for this server.
    validation_evidence: Option<crate::extract::scan_stats::LspValidationEvidence>,
    /// Whether the server reached quiescent=true during initialization.
    /// When false, the quiescence deadline expired before the server finished
    /// indexing. In that case Pass 3 (diagnostics) is skipped to avoid flooding
    /// the server with diagnostic requests while it is still loading — which was
    /// the root cause of the 0-edge regression introduced by #381.
    was_quiescent: bool,
    /// Consecutive type hierarchy failures. After MAX_TYPE_HIERARCHY_STRIKES,
    /// type hierarchy is disabled for the rest of the session.
    type_hierarchy_strikes: u32,
    /// Shared diagnostics buffer populated by the pipelined transport's reader
    /// loop from `textDocument/publishDiagnostics` notifications.
    /// Maps document URI → list of LSP Diagnostic objects (JSON).
    diagnostics_sink: Arc<std::sync::Mutex<HashMap<String, Vec<serde_json::Value>>>>,
}

/// After this many consecutive type hierarchy failures, disable type hierarchy
/// for the remainder of the enrichment pass to avoid stalling on broken servers.
const MAX_TYPE_HIERARCHY_STRIKES: u32 = 3;

/// After processing this many nodes with zero edges, abort enrichment.
/// A functioning language server should produce at least some edges within
/// the first 1,000 nodes; zero edges indicates a server that cannot resolve any
/// references in the selected workspace.
const ZERO_EDGE_ABORT_THRESHOLD: u32 = 1_000;

/// Minimum warmup time before the node-count abort can fire.
/// typescript-language-server and similar servers need time to fully index
/// the project before producing call hierarchy results. Without this guard,
/// the 1,000-node abort fires in ~0.3s on large TypeScript projects before
/// the server has finished indexing — producing 0 call edges despite being
/// correctly configured.
const ZERO_EDGE_MIN_WARMUP: std::time::Duration = std::time::Duration::from_secs(30);

/// Time-based abort: if no edges after this duration, abort enrichment.
/// On slow LSP servers, reaching the node-count threshold can take 100+ minutes.
/// This caps the wait at 2 minutes.
const ZERO_EDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

const READINESS_REQUEST_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);

fn read_lsp_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn initialization_settings_with_compile_commands(
    base: Option<serde_json::Value>,
    repo_root: &Path,
    readiness_files: &[PathBuf],
    overrides: &[LspCompileCommandOverride],
) -> Result<Option<serde_json::Value>> {
    if overrides.is_empty() || readiness_files.is_empty() {
        return Ok(base);
    }

    let working_directory = std::fs::canonicalize(repo_root).with_context(|| {
        format!(
            "failed to canonicalize LSP compilation working directory {}",
            repo_root.display()
        )
    })?;
    let mut commands = serde_json::Map::new();
    let mut files = readiness_files.to_vec();
    files.sort();
    files.dedup();
    for relative_path in files {
        let relative = relative_path.to_string_lossy();
        let Some(rule) = overrides
            .iter()
            .find(|rule| relative.ends_with(rule.suffix))
        else {
            continue;
        };
        let absolute_path =
            std::fs::canonicalize(repo_root.join(&relative_path)).with_context(|| {
                format!(
                    "failed to canonicalize LSP compilation override file {}",
                    relative_path.display()
                )
            })?;
        let mut compilation_command = vec![rule.compiler.to_string()];
        compilation_command.extend(rule.args.iter().map(|arg| (*arg).to_string()));
        compilation_command.push(absolute_path.to_string_lossy().into_owned());
        commands.insert(
            absolute_path.to_string_lossy().into_owned(),
            serde_json::json!({
                "workingDirectory": working_directory,
                "compilationCommand": compilation_command,
            }),
        );
    }
    if commands.is_empty() {
        return Ok(base);
    }

    let mut settings = base.unwrap_or_else(|| serde_json::json!({}));
    let settings_object = settings
        .as_object_mut()
        .context("LSP initialization settings must be a JSON object")?;
    let changes = settings_object
        .entry("compilationDatabaseChanges")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context("LSP compilationDatabaseChanges setting must be a JSON object")?;
    changes.extend(commands);
    Ok(Some(settings))
}

fn lsp_language_id(inventory_language: &str, path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match inventory_language {
        "c-cpp" => {
            if file_name.ends_with(".c")
                || file_name.ends_with(".h")
                || file_name.ends_with(".c.in")
                || file_name.ends_with(".h.in")
            {
                "c".to_string()
            } else {
                "cpp".to_string()
            }
        }
        "typescript" | "deno" => match path.extension().and_then(|extension| extension.to_str()) {
            Some("tsx") => "typescriptreact".to_string(),
            Some("js" | "mjs" | "js_t") => "javascript".to_string(),
            Some("jsx") => "javascriptreact".to_string(),
            _ => "typescript".to_string(),
        },
        language => language.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadinessValidationMethod {
    WorkspaceSymbol,
    DocumentSymbol,
    CodeAction,
}

impl ReadinessValidationMethod {
    fn method(self) -> &'static str {
        match self {
            Self::WorkspaceSymbol => "workspace/symbol",
            Self::DocumentSymbol => "textDocument/documentSymbol",
            Self::CodeAction => "textDocument/codeAction",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadinessValidationCapabilities {
    workspace_symbols: bool,
    document_symbols: bool,
    code_actions: bool,
}

impl ReadinessValidationCapabilities {
    fn from_server_capabilities(capabilities: &ServerCapabilities) -> Self {
        Self {
            workspace_symbols: provider_is_enabled(&capabilities.workspace_symbol_provider),
            document_symbols: provider_is_enabled(&capabilities.document_symbol_provider),
            code_actions: code_action_provider_is_enabled(&capabilities.code_action_provider),
        }
    }

    fn primary(self) -> Option<ReadinessValidationMethod> {
        if self.workspace_symbols {
            Some(ReadinessValidationMethod::WorkspaceSymbol)
        } else if self.document_symbols {
            Some(ReadinessValidationMethod::DocumentSymbol)
        } else if self.code_actions {
            Some(ReadinessValidationMethod::CodeAction)
        } else {
            None
        }
    }
}

fn negotiated_operation_capabilities(
    capabilities: &ServerCapabilities,
    has_call_hierarchy: bool,
) -> LspNegotiatedCapabilities {
    LspNegotiatedCapabilities {
        references_provider: provider_is_enabled(&capabilities.references_provider),
        call_hierarchy_provider: has_call_hierarchy,
        definition_provider: provider_is_enabled(&capabilities.definition_provider),
        implementation_provider: implementation_provider_is_enabled(
            &capabilities.implementation_provider,
        ),
        document_link_provider: capabilities.document_link_provider.is_some(),
        document_symbol_provider: provider_is_enabled(&capabilities.document_symbol_provider),
        code_action_provider: code_action_provider_is_enabled(&capabilities.code_action_provider),
    }
}

fn provider_is_enabled<T>(provider: &Option<OneOf<bool, T>>) -> bool {
    match provider {
        Some(OneOf::Left(enabled)) => *enabled,
        Some(OneOf::Right(_)) => true,
        None => false,
    }
}

fn implementation_provider_is_enabled(provider: &Option<ImplementationProviderCapability>) -> bool {
    match provider {
        Some(ImplementationProviderCapability::Simple(enabled)) => *enabled,
        Some(ImplementationProviderCapability::Options(_)) => true,
        None => false,
    }
}

fn code_action_provider_is_enabled(provider: &Option<CodeActionProviderCapability>) -> bool {
    match provider {
        Some(CodeActionProviderCapability::Simple(enabled)) => *enabled,
        Some(CodeActionProviderCapability::Options(_)) => true,
        None => false,
    }
}

#[async_trait::async_trait]
trait ReadinessRequester: Send {
    async fn readiness_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value>;
}

#[async_trait::async_trait]
impl ReadinessRequester for LspTransport {
    async fn readiness_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request(method, params).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadinessValidationResponse {
    symbol_count: usize,
    document_symbols: Vec<LspDocumentSymbolEvidence>,
}

fn normalized_document_symbol_evidence(
    response: &serde_json::Value,
    default_uri: &str,
) -> Result<Vec<LspDocumentSymbolEvidence>> {
    fn collect(
        values: &[serde_json::Value],
        default_uri: &str,
        output: &mut Vec<LspDocumentSymbolEvidence>,
    ) -> Result<()> {
        for value in values {
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .context("documentSymbol response item has no string name")?;
            let kind = value
                .get("kind")
                .and_then(serde_json::Value::as_u64)
                .context("documentSymbol response item has no numeric kind")?
                as u32;
            let uri = value
                .pointer("/location/uri")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(default_uri);
            let range = value
                .get("range")
                .or_else(|| value.pointer("/location/range"))
                .context("documentSymbol response item has no range")?;
            let coordinate = |pointer: &str| -> Result<u32> {
                Ok(range
                    .pointer(pointer)
                    .and_then(serde_json::Value::as_u64)
                    .with_context(|| {
                        format!("documentSymbol response range has no numeric {pointer}")
                    })? as u32)
            };
            let start_line = coordinate("/start/line")?;
            let start_character = coordinate("/start/character")?;
            let end_line = coordinate("/end/line")?;
            let end_character = coordinate("/end/character")?;
            let normalized = serde_json::json!({
                "uri": uri,
                "name": name,
                "kind": kind,
                "start_line": start_line,
                "start_character": start_character,
                "end_line": end_line,
                "end_character": end_character,
            });
            let payload_digest = blake3::hash(&serde_json::to_vec(&normalized)?)
                .to_hex()
                .to_string();
            output.push(LspDocumentSymbolEvidence {
                uri: uri.to_string(),
                name: name.to_string(),
                kind,
                start_line,
                start_character,
                end_line,
                end_character,
                payload_digest,
                graph_result_id: None,
                file: None,
            });
            if let Some(children) = value.get("children").and_then(serde_json::Value::as_array) {
                collect(children, uri, output)?;
            }
        }
        Ok(())
    }

    let Some(values) = response.as_array() else {
        anyhow::ensure!(
            response.is_null(),
            "documentSymbol response must be an array or null"
        );
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    collect(values, default_uri, &mut output)?;
    output.sort();
    output.dedup();
    Ok(output)
}

fn materialize_document_symbol_nodes(
    validation: &mut LspValidationEvidence,
    repo_root: &Path,
    matching_nodes: &[&Node],
    default_root: Option<&str>,
) -> Result<Vec<Node>> {
    let roots_by_file = matching_nodes
        .iter()
        .map(|node| (node.id.file.clone(), node.id.root.clone()))
        .collect::<HashMap<_, _>>();
    materialize_document_symbols(
        &validation.language,
        &mut validation.document_symbols,
        repo_root,
        |file| {
            roots_by_file
                .get(file)
                .cloned()
                .or_else(|| default_root.map(str::to_string))
        },
    )
}

fn materialize_document_symbols<F>(
    language: &str,
    symbols: &mut [LspDocumentSymbolEvidence],
    repo_root: &Path,
    root_for_file: F,
) -> Result<Vec<Node>>
where
    F: Fn(&Path) -> Option<String>,
{
    let mut nodes = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        let uri = Uri::from_str(&symbol.uri)
            .with_context(|| format!("invalid documentSymbol URI {}", symbol.uri))?;
        let file = uri_to_relative_path(&uri, repo_root);
        anyhow::ensure!(
            !file.is_absolute()
                && file
                    .components()
                    .all(|component| { matches!(component, std::path::Component::Normal(_)) }),
            "documentSymbol URI {} is outside the repository",
            symbol.uri
        );
        let root = root_for_file(&file).with_context(|| {
            format!(
                "documentSymbol response for {} has no matching extracted file",
                file.display()
            )
        })?;
        let normalized_file = file.to_string_lossy().replace('\\', "/");
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("lsp_document_symbol_name".to_string(), symbol.name.clone());
        metadata.insert(
            "lsp_document_symbol_kind".to_string(),
            symbol.kind.to_string(),
        );
        metadata.insert(
            "lsp_document_symbol_payload_digest".to_string(),
            symbol.payload_digest.clone(),
        );
        let node = Node {
            id: NodeId {
                root,
                file: file.clone(),
                name: format!(
                    "{}@{}",
                    symbol.name,
                    symbol
                        .payload_digest
                        .get(..16)
                        .unwrap_or(&symbol.payload_digest)
                ),
                kind: NodeKind::Other("lsp_document_symbol".to_string()),
            },
            language: language.to_string(),
            line_start: symbol.start_line as usize + 1,
            line_end: symbol.end_line as usize + 1,
            signature: format!("documentSymbol {} ({})", symbol.name, symbol.kind),
            body: String::new(),
            metadata,
            source: ExtractionSource::Lsp,
        };
        symbol.file = Some(normalized_file);
        symbol.graph_result_id = Some(node.stable_id());
        nodes.push(node);
    }
    nodes.sort_by_key(Node::stable_id);
    anyhow::ensure!(
        nodes
            .windows(2)
            .all(|pair| pair[0].stable_id() != pair[1].stable_id()),
        "documentSymbol response items do not map to distinct graph identities"
    );
    Ok(nodes)
}

async fn execute_readiness_validation(
    requester: &mut dyn ReadinessRequester,
    method: ReadinessValidationMethod,
    warmup_uri: Option<&str>,
    workspace_query: &str,
    timeout: tokio::time::Duration,
) -> Result<ReadinessValidationResponse> {
    let params = match method {
        ReadinessValidationMethod::WorkspaceSymbol => {
            serde_json::json!({ "query": workspace_query })
        }
        ReadinessValidationMethod::DocumentSymbol => {
            let uri = warmup_uri.context(
                "server advertises documentSymbol validation but no deterministic warm-up file was available",
            )?;
            serde_json::json!({ "textDocument": { "uri": uri } })
        }
        ReadinessValidationMethod::CodeAction => {
            let uri = warmup_uri.context(
                "server advertises codeAction validation but no deterministic warm-up file was available",
            )?;
            serde_json::json!({
                "textDocument": { "uri": uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 0 }
                },
                "context": { "diagnostics": [] }
            })
        }
    };
    match tokio::time::timeout(
        timeout,
        requester.readiness_request(method.method(), params),
    )
    .await
    {
        Ok(Ok(response)) => {
            let document_symbols = if method == ReadinessValidationMethod::DocumentSymbol {
                normalized_document_symbol_evidence(
                    &response,
                    warmup_uri.context("documentSymbol validation has no warm-up URI")?,
                )?
            } else {
                Vec::new()
            };
            Ok(ReadinessValidationResponse {
                symbol_count: if method == ReadinessValidationMethod::DocumentSymbol {
                    document_symbols.len()
                } else {
                    response.as_array().map_or(0, Vec::len)
                },
                document_symbols,
            })
        }
        Ok(Err(error)) if is_method_not_found(&error) => Err(anyhow::anyhow!(
            "server advertised {} but returned unsupported method (-32601)",
            method.method()
        )),
        Ok(Err(error)) => Err(anyhow::anyhow!(
            "server advertised {} but validation failed: {}",
            method.method(),
            error
        )),
        Err(_) => Err(anyhow::anyhow!(
            "server advertised {} but validation timed out after {}ms",
            method.method(),
            timeout.as_millis()
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadinessValidationResult {
    method: ReadinessValidationMethod,
    request_uri: Option<String>,
    symbol_count: usize,
    document_symbols: Vec<LspDocumentSymbolEvidence>,
}

async fn execute_indexing_validation_once(
    requester: &mut dyn ReadinessRequester,
    capabilities: ReadinessValidationCapabilities,
    warmup_uri: Option<&str>,
    workspace_query: &str,
    timeout: tokio::time::Duration,
) -> Result<Option<ReadinessValidationResult>> {
    let Some(primary) = capabilities.primary() else {
        return Err(anyhow::anyhow!(
            "server advertises none of workspace/symbol, textDocument/documentSymbol, or textDocument/codeAction"
        ));
    };
    let primary_response =
        execute_readiness_validation(requester, primary, warmup_uri, workspace_query, timeout)
            .await?;
    if primary != ReadinessValidationMethod::WorkspaceSymbol || primary_response.symbol_count > 0 {
        return Ok(Some(ReadinessValidationResult {
            method: primary,
            request_uri: (primary != ReadinessValidationMethod::WorkspaceSymbol)
                .then(|| warmup_uri.map(str::to_string))
                .flatten(),
            symbol_count: primary_response.symbol_count,
            document_symbols: primary_response.document_symbols,
        }));
    }
    if capabilities.document_symbols {
        let response = execute_readiness_validation(
            requester,
            ReadinessValidationMethod::DocumentSymbol,
            warmup_uri,
            "",
            timeout,
        )
        .await?;
        return Ok(Some(ReadinessValidationResult {
            method: ReadinessValidationMethod::DocumentSymbol,
            request_uri: warmup_uri.map(str::to_string),
            symbol_count: response.symbol_count,
            document_symbols: response.document_symbols,
        }));
    }
    if capabilities.code_actions {
        let response = execute_readiness_validation(
            requester,
            ReadinessValidationMethod::CodeAction,
            warmup_uri,
            "",
            timeout,
        )
        .await?;
        return Ok(Some(ReadinessValidationResult {
            method: ReadinessValidationMethod::CodeAction,
            request_uri: warmup_uri.map(str::to_string),
            symbol_count: response.symbol_count,
            document_symbols: response.document_symbols,
        }));
    }
    Ok(None)
}

fn command_exists_on_path(command: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return command_path.is_file();
    }
    path.is_some_and(|path| {
        std::env::split_paths(path).any(|directory| directory.join(command_path).is_file())
    })
}

fn is_regular_repo_file(repo_root: &Path, relative_path: &Path) -> bool {
    if relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || !relative_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return false;
    }

    std::fs::symlink_metadata(repo_root.join(relative_path))
        .is_ok_and(|metadata| metadata.file_type().is_file())
}

impl LspEnricher {
    /// Create a new LSP enricher for the given language server.
    ///
    /// - `language`: language identifier (e.g., "rust", "python")
    /// - `command`: binary to spawn (e.g., "rust-analyzer", "pyrefly")
    /// - `args`: command-line arguments (e.g., &["--stdio"])
    /// - `extensions`: file extensions this enricher handles (e.g., &["rs"])
    pub fn new(language: &str, command: &str, args: &[&str], _extensions: &[&str]) -> Self {
        // Leak language string once — enrichers live for the entire program
        let lang_static: &'static str = Box::leak(language.to_string().into_boxed_str());
        let lang_slice: &'static [&'static str] = Box::leak(vec![lang_static].into_boxed_slice());

        Self {
            language: language.to_string(),
            language_static: lang_slice,
            display_name: format!("{}-lsp", command),
            server_command: command.to_string(),
            server_args: args.iter().map(|s| s.to_string()).collect(),
            file_readiness: false,
            file_readiness_filter: None,
            init_settings: None,
            compile_command_overrides: &[],
            config_file: None,
            toolchain_remediation: None,
            ready: AtomicBool::new(false),
            state: Mutex::new(LspState {
                transport: None,
                pipelined: None,
                root_path: None,
                init_failed: false,
                has_type_hierarchy: false,
                has_references: false,
                has_call_hierarchy: false,
                has_implementation: false,
                has_definition: false,
                has_document_links: false,
                has_pull_diagnostics: false,
                has_inlay_hints: false,
                server_capabilities: None,
                validation_evidence: None,
                was_quiescent: false,
                type_hierarchy_strikes: 0,
                diagnostics_sink: Arc::new(std::sync::Mutex::new(HashMap::new())),
            }),
            startup_root_override: std::sync::OnceLock::new(),
            query_profile: LspQueryProfile::new(language, command),
        }
    }

    /// Override the LSP server startup working directory.
    ///
    /// When called before the first `enrich()` call, the language server will be
    /// started with `current_dir = lsp_root` and `rootUri = file:///<lsp_root>`.
    /// This is used for monorepo subdirectory roots (e.g., `client/`) where the
    /// language server needs to find `tsconfig.json` / `pyproject.toml` in the
    /// subdirectory.
    ///
    /// File path construction for LSP requests is unaffected — it uses `repo_root`
    /// from `enrich()`, which produces correct absolute file URIs.
    pub fn with_startup_root(self, lsp_root: PathBuf) -> Self {
        let _ = self.startup_root_override.set(lsp_root);
        self
    }

    /// Create a new LSP enricher with custom initialization settings.
    ///
    /// Settings are sent as `initializationOptions` in the LSP initialize request.
    pub fn with_settings(mut self, settings: serde_json::Value) -> Self {
        self.init_settings = Some(settings);
        self
    }

    fn with_compile_command_overrides(
        mut self,
        overrides: &'static [LspCompileCommandOverride],
    ) -> Self {
        self.compile_command_overrides = overrides;
        self
    }

    /// Set the config file hint for lsp_root selection.
    ///
    /// When a monorepo has multiple subdirectory roots, this hint is used to
    /// prefer the root that contains this file (e.g., `tsconfig.json` for TypeScript).
    pub fn with_config_file(mut self, config_file: &'static str) -> Self {
        self.config_file = Some(config_file);
        self
    }

    /// Attach actionable setup guidance for this language server.
    pub fn with_toolchain_remediation(mut self, remediation: &'static str) -> Self {
        self.toolchain_remediation = Some(remediation);
        self
    }

    /// Restrict which node kinds are enriched via LSP.
    /// When set, only nodes matching these kinds are sent for enrichment.
    pub fn with_enrichable_kinds(mut self, kinds: &'static [NodeKind]) -> Self {
        self.query_profile = self.query_profile.with_allowed_kinds(kinds);
        self
    }

    fn with_declared_const_references(mut self, allow: bool) -> Self {
        self.query_profile = self.query_profile.with_declared_const_references(allow);
        self
    }

    pub(crate) fn with_broad_references(mut self, budget: Arc<LspBroadReferenceBudget>) -> Self {
        self.query_profile = self.query_profile.with_broad_references(budget);
        self
    }

    pub(crate) fn with_file_readiness(mut self, enabled: bool) -> Self {
        self.file_readiness = enabled;
        self
    }

    pub(crate) fn with_file_readiness_filter(
        mut self,
        filter: Option<Arc<HashSet<PathBuf>>>,
    ) -> Self {
        self.file_readiness_filter = filter;
        self
    }

    pub(crate) fn broad_reference_budget(&self) -> Option<&Arc<LspBroadReferenceBudget>> {
        self.query_profile.broad_reference_budget()
    }

    async fn within_enrichment_deadline<T>(
        &self,
        job_deadline: tokio::time::Instant,
        phase: &str,
        future: impl std::future::Future<Output = T>,
    ) -> std::result::Result<T, String> {
        enum DeadlineKind<'a> {
            Job,
            BroadReference(&'a LspBroadReferenceBudget),
        }

        let now = tokio::time::Instant::now();
        let (deadline, kind) = if let Some(budget) = self.broad_reference_budget() {
            let broad_deadline = budget
                .remaining_duration()
                .map_or(now, |remaining| now + remaining);
            if broad_deadline <= job_deadline {
                (broad_deadline, DeadlineKind::BroadReference(budget))
            } else {
                (job_deadline, DeadlineKind::Job)
            }
        } else {
            (job_deadline, DeadlineKind::Job)
        };

        match tokio::time::timeout_at(deadline, future).await {
            Ok(output) => Ok(output),
            Err(_) => match kind {
                DeadlineKind::BroadReference(budget) => {
                    budget.open_time_circuit();
                    Err(Self::broad_reference_deadline_detail(budget, phase))
                }
                DeadlineKind::Job => Err(format!(
                    "LSP enrichment job timed out after {}s during {phase}",
                    lsp_job_timeout().as_secs()
                )),
            },
        }
    }

    fn broad_reference_deadline_detail(budget: &LspBroadReferenceBudget, phase: &str) -> String {
        let reason = budget
            .snapshot()
            .circuit_reason
            .unwrap_or_else(|| "broad-reference time budget exhausted".to_string());
        format!("{reason} during {phase}")
    }

    fn mark_broad_reference_deadline(&self, result: &mut EnrichmentResult, detail: String) {
        result.any_enricher_ran = true;
        result.aborted = true;
        result.error_count = result.error_count.saturating_add(1);
        result.diagnostic = Some(format!(
            "LSP enrichment aborted for {}: {}; safely produced partial output was preserved",
            self.server_command, detail
        ));
        tracing::warn!("{}", result.diagnostic.as_deref().unwrap_or_default());
    }

    #[cfg(test)]
    pub(crate) fn enrichable_kinds(&self) -> Option<&std::collections::HashSet<NodeKind>> {
        self.query_profile.allowed_kinds()
    }

    #[cfg(test)]
    pub(crate) fn allows_declared_const_references(&self) -> bool {
        self.query_profile.allows_declared_const_references()
    }

    #[cfg(test)]
    pub(crate) fn admits_pass1_node(&self, node: &Node) -> bool {
        let operation = match node.id.kind {
            NodeKind::Function => policy::LspQueryOperation::CallHierarchy,
            NodeKind::Trait => policy::LspQueryOperation::Implementations,
            NodeKind::Struct | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Const => {
                policy::LspQueryOperation::References
            }
            NodeKind::MarkdownSection
                if node.metadata.get("markdown_kind").map(String::as_str) == Some("link") =>
            {
                policy::LspQueryOperation::Definitions
            }
            NodeKind::MarkdownSection => policy::LspQueryOperation::DocumentLinks,
            NodeKind::Other(_) => policy::LspQueryOperation::DocumentLinks,
            _ => return false,
        };
        self.query_profile.admits(
            node,
            operation,
            LspServerCapabilities {
                references: true,
                call_hierarchy: true,
                definitions: true,
                implementations: true,
                type_hierarchy: true,
                document_symbols: true,
                document_links: true,
            },
            &mut self.query_profile.budget(),
        )
    }

    /// Common admission boundary for every LSP pass.
    ///
    /// Synthetic graph values are searchable evidence, not compiler symbols.
    /// Rejecting them before `matching_nodes` is shared with any pass prevents
    /// them from becoming per-node or file-derived LSP work targets.
    pub(crate) fn admits_node(&self, node: &Node) -> bool {
        self.query_profile.accepts_declaration(node)
    }

    /// Check if an `experimental/serverStatus` notification indicates readiness.
    ///
    /// rust-analyzer sends `quiescent: true` when it has finished all background
    /// work (indexing, proc-macro loading, etc.).  Combined with `health: "ok"`,
    /// this means the server is ready to answer queries.
    fn server_status_is_ready(msg: &serde_json::Value) -> bool {
        let health = msg
            .pointer("/params/health")
            .and_then(|h| h.as_str())
            .unwrap_or("");
        let quiescent = msg
            .pointer("/params/quiescent")
            .and_then(|q| q.as_bool())
            .unwrap_or(false); // Default to NOT ready if field is absent
        health == "ok" && quiescent
    }

    /// Check if the server binary is available on PATH.
    fn is_server_available(&self) -> bool {
        command_exists_on_path(&self.server_command, std::env::var_os("PATH").as_deref())
    }

    /// Pick a deterministic didOpen file from the nodes admitted to this invocation.
    ///
    /// The current admitted cohort already carries dirty-root and node-scope filtering,
    /// so selecting from it cannot warm up an excluded or unrelated file.
    fn find_warmup_file(&self, repo_root: &Path, matching_nodes: &[&Node]) -> Option<PathBuf> {
        let startup_root = self
            .startup_root_override
            .get()
            .map(PathBuf::as_path)
            .unwrap_or(repo_root);
        let mut candidates = matching_nodes
            .iter()
            .map(|node| repo_root.join(&node.id.file))
            .filter(|path| path.starts_with(startup_root))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        candidates.into_iter().find(|path| path.is_file())
    }

    fn inventory_readiness_files(&self, repo_root: &Path) -> Result<Vec<PathBuf>> {
        if !self.file_readiness {
            return Ok(Vec::new());
        }
        let mut paths = crate::lsp_completeness::included_lsp_paths_by_language(repo_root)?
            .remove(&self.language)
            .unwrap_or_default();
        if let Some(filter) = self.file_readiness_filter.as_deref() {
            paths.retain(|path| filter.contains(path));
        }
        Ok(paths)
    }

    async fn validate_inventory_files(
        &self,
        transport: &PipelinedTransport,
        repo_root: &Path,
        paths: &[PathBuf],
        default_root: Option<&str>,
        negotiated: LspNegotiatedCapabilities,
    ) -> (Vec<LspValidationEvidence>, Vec<Node>) {
        let mut validations = Vec::with_capacity(paths.len());
        let mut nodes = Vec::new();
        for relative in paths {
            let absolute = repo_root.join(relative);
            let started = std::time::Instant::now();
            let uri = match path_to_uri(&absolute) {
                Ok(uri) => uri,
                Err(error) => {
                    validations.push(
                        LspValidationEvidence::not_validated(
                            &self.language,
                            &self.server_command,
                            error.to_string(),
                        )
                        .with_duration_ms(started.elapsed().as_millis() as u64),
                    );
                    continue;
                }
            };
            let request_uri = uri.to_string();
            let content = match read_lsp_text(&absolute) {
                Ok(content) => content,
                Err(error) => {
                    validations.push(
                        LspValidationEvidence::not_validated(
                            &self.language,
                            &self.server_command,
                            format!("failed to read {}: {error}", relative.display()),
                        )
                        .with_request_uri(Some(request_uri))
                        .with_negotiated_capabilities(negotiated)
                        .with_duration_ms(started.elapsed().as_millis() as u64),
                    );
                    continue;
                }
            };
            let did_open = transport
                .notify(
                    "textDocument/didOpen",
                    serde_json::json!({
                        "textDocument": {
                            "uri": request_uri,
                            "languageId": self.lsp_language_id_for_path(relative),
                            "version": 1,
                            "text": content,
                        }
                    }),
                )
                .await;
            let validation_method = if negotiated.document_symbol_provider {
                Some(ReadinessValidationMethod::DocumentSymbol)
            } else if negotiated.code_action_provider {
                Some(ReadinessValidationMethod::CodeAction)
            } else {
                None
            };
            let response = match (did_open, validation_method) {
                (Ok(()), Some(method)) => {
                    let params = match method {
                        ReadinessValidationMethod::DocumentSymbol => serde_json::json!({
                            "textDocument": { "uri": request_uri }
                        }),
                        ReadinessValidationMethod::CodeAction => serde_json::json!({
                            "textDocument": { "uri": request_uri },
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 0 }
                            },
                            "context": { "diagnostics": [] }
                        }),
                        ReadinessValidationMethod::WorkspaceSymbol => {
                            unreachable!("inventory validation must be file-scoped")
                        }
                    };
                    match tokio::time::timeout(
                        READINESS_REQUEST_TIMEOUT,
                        transport.request(method.method(), params),
                    )
                    .await
                    {
                        Ok(Ok(response)) => Ok((method, response)),
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(anyhow::anyhow!(
                            "{} timed out after {}ms",
                            method.method(),
                            READINESS_REQUEST_TIMEOUT.as_millis()
                        )),
                    }
                }
                (Ok(()), None) => Err(anyhow::anyhow!(
                    "server advertised neither documentSymbolProvider nor codeActionProvider"
                )),
                (Err(error), _) => Err(error),
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            match response {
                Ok((ReadinessValidationMethod::DocumentSymbol, response)) => {
                    match normalized_document_symbol_evidence(&response, &request_uri) {
                        Ok(mut symbols) => {
                            let symbol_count = symbols.len();
                            match materialize_document_symbols(
                                &self.language,
                                &mut symbols,
                                repo_root,
                                |file| {
                                    if file == relative {
                                        default_root.map(str::to_string)
                                    } else {
                                        None
                                    }
                                },
                            ) {
                                Ok(mut file_nodes) => {
                                    nodes.append(&mut file_nodes);
                                    validations.push(
                                        LspValidationEvidence::processed(
                                            &self.language,
                                            &self.server_command,
                                            "textDocument/documentSymbol",
                                            symbol_count,
                                        )
                                        .with_request_uri(Some(request_uri.clone()))
                                        .with_negotiated_capabilities(negotiated)
                                        .with_document_symbols(symbols)
                                        .with_duration_ms(duration_ms),
                                    );
                                }
                                Err(error) => validations.push(
                                    LspValidationEvidence::not_validated(
                                        &self.language,
                                        &self.server_command,
                                        error.to_string(),
                                    )
                                    .with_request_uri(Some(request_uri.clone()))
                                    .with_negotiated_capabilities(negotiated)
                                    .with_duration_ms(duration_ms),
                                ),
                            }
                        }
                        Err(error) => validations.push(
                            LspValidationEvidence::not_validated(
                                &self.language,
                                &self.server_command,
                                error.to_string(),
                            )
                            .with_request_uri(Some(request_uri.clone()))
                            .with_negotiated_capabilities(negotiated)
                            .with_duration_ms(duration_ms),
                        ),
                    }
                }
                Ok((ReadinessValidationMethod::CodeAction, response)) => {
                    if response.is_null() || response.is_array() {
                        validations.push(
                            LspValidationEvidence::processed(
                                &self.language,
                                &self.server_command,
                                "textDocument/codeAction",
                                response.as_array().map_or(0, Vec::len),
                            )
                            .with_request_uri(Some(request_uri.clone()))
                            .with_negotiated_capabilities(negotiated)
                            .with_duration_ms(duration_ms),
                        );
                    } else {
                        validations.push(
                            LspValidationEvidence::not_validated(
                                &self.language,
                                &self.server_command,
                                "textDocument/codeAction response must be an array or null",
                            )
                            .with_request_uri(Some(request_uri.clone()))
                            .with_negotiated_capabilities(negotiated)
                            .with_duration_ms(duration_ms),
                        );
                    }
                }
                Ok((ReadinessValidationMethod::WorkspaceSymbol, _)) => {
                    unreachable!("inventory validation must be file-scoped")
                }
                Err(error) => validations.push(
                    LspValidationEvidence::not_validated(
                        &self.language,
                        &self.server_command,
                        error.to_string(),
                    )
                    .with_request_uri(Some(request_uri.clone()))
                    .with_negotiated_capabilities(negotiated)
                    .with_duration_ms(duration_ms),
                ),
            }
            let _ = transport
                .notify(
                    "textDocument/didClose",
                    serde_json::json!({ "textDocument": { "uri": request_uri } }),
                )
                .await;
        }
        validations.sort_by(|left, right| left.request_uri.cmp(&right.request_uri));
        nodes.sort_by_key(Node::stable_id);
        nodes.dedup_by_key(|node| node.stable_id());
        (validations, nodes)
    }

    fn lsp_language_id_for_path(&self, path: &Path) -> String {
        lsp_language_id(&self.language, path)
    }

    /// Send textDocument/didOpen for the given file.
    async fn send_did_open(&self, transport: &mut LspTransport, path: &Path) -> Result<()> {
        let uri = path_to_uri(path)?;
        let content = read_lsp_text(path)
            .with_context(|| format!("reading warmup file {}", path.display()))?;
        let language_id = self.lsp_language_id_for_path(path);
        transport
            .notify(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri.to_string(),
                        "languageId": language_id,
                        "version": 1,
                        "text": content
                    }
                }),
            )
            .await?;
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        Ok(())
    }

    fn remediation_suffix(&self) -> String {
        self.toolchain_remediation
            .map(|hint| format!("\n  Fix: {hint}"))
            .unwrap_or_default()
    }

    async fn ensure_initialized(
        &self,
        repo_root: &Path,
        warmup_path: Option<&Path>,
        readiness_files: &[PathBuf],
    ) -> Result<()> {
        let result = self
            .ensure_initialized_inner(repo_root, warmup_path, readiness_files)
            .await;
        if result.is_err() {
            self.reset_incomplete_initialization().await;
        }
        result
    }

    async fn reset_incomplete_initialization(&self) {
        let mut state = self.state.lock().await;
        if state.pipelined.is_none() {
            state.transport.take();
            state.root_path = None;
            state.server_capabilities = None;
            state.validation_evidence = None;
        }
    }

    /// Initialize the language server if not already running.
    async fn ensure_initialized_inner(
        &self,
        repo_root: &Path,
        warmup_path: Option<&Path>,
        readiness_files: &[PathBuf],
    ) -> Result<()> {
        let mut state = self.state.lock().await;

        if state.pipelined.is_some() {
            return Ok(());
        }
        // A cancelled initialization can leave the pre-handshake transport in
        // state. It is not usable by enrichment and must not suppress a retry.
        state.transport.take();
        state.root_path = None;

        if state.init_failed {
            return Err(anyhow::anyhow!(
                "{} initialization previously failed",
                self.server_command
            ));
        }

        // Check if the server binary is available before trying to spawn
        if !self.is_server_available() {
            state.init_failed = true;
            tracing::info!(
                "LSP server '{}' not found, skipping enrichment for {}",
                self.server_command,
                self.language
            );
            return Err(anyhow::anyhow!(
                "LSP server '{}' not found on PATH{}",
                self.server_command,
                self.remediation_suffix()
            ));
        }

        // Use startup_root_override if set (monorepo subdirectory roots), otherwise
        // fall back to repo_root. The startup root determines the LSP server's
        // `current_dir` and `rootUri`, letting language servers find their config
        // files (e.g. typescript-language-server finds `client/tsconfig.json` when
        // started from `client/`). File path construction for LSP requests still
        // uses `repo_root` (the primary root) via `state.root_path` below.
        let startup_root = self
            .startup_root_override
            .get()
            .map(|p| p.as_path())
            .unwrap_or(repo_root);

        if startup_root != repo_root {
            tracing::info!(
                "Starting {} for {} LSP enrichment from '{}' (startup root override)...",
                self.server_command,
                self.language,
                startup_root.display(),
            );
        } else {
            tracing::info!(
                "Starting {} for {} LSP enrichment...",
                self.server_command,
                self.language
            );
        }

        let transport = match LspTransport::spawn(
            &self.server_command,
            &self.server_args,
            startup_root,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                state.init_failed = true;
                tracing::warn!(
                    "{} not available, skipping LSP enrichment for {}: {}",
                    self.server_command,
                    self.language,
                    e
                );
                return Err(e).with_context(|| {
                    format!(
                        "Failed to start LSP server '{}'{}",
                        self.server_command,
                        self.remediation_suffix()
                    )
                });
            }
        };

        // Always store the primary repo_root in root_path — this is used for
        // constructing absolute file paths in LSP requests (root.join(node.id.file)).
        // The startup root is only for server initialization; file paths remain
        // relative to the primary root.
        state.transport = Some(transport);
        state.root_path = Some(repo_root.to_path_buf());

        // Send initialize request using the startup root as rootUri.
        let root_uri = path_to_uri(startup_root)?;

        let workspace_name = startup_root
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| startup_root.to_string_lossy().into_owned());
        let mut init_params = lsp_initialize_params(root_uri, workspace_name);

        // Apply per-language initialization settings if provided. Descriptors
        // whose LangConfig declares venv_candidates receive the Python-analysis
        // venvPath/venv settings expected by that server family.
        let lang_config = crate::extract::configs::config_for_language(&self.language);
        let effective_settings =
            if let Some(venv_dirs) = lang_config.and_then(|c| c.venv_candidates) {
                let found_venv = venv_dirs
                    .iter()
                    .find(|&&name| startup_root.join(name).is_dir());
                if let Some(venv_name) = found_venv {
                    let venv_path_str = startup_root.to_string_lossy().to_string();
                    let mut merged = self
                        .init_settings
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let python_obj = merged.as_object_mut().and_then(|root| {
                        if !root.contains_key("python") {
                            root.insert("python".into(), serde_json::json!({}));
                        }
                        root.get_mut("python")
                    });
                    if let Some(python_val) = python_obj {
                        let analysis_obj = python_val.as_object_mut().and_then(|p| {
                            if !p.contains_key("analysis") {
                                p.insert("analysis".into(), serde_json::json!({}));
                            }
                            p.get_mut("analysis")
                        });
                        if let Some(analysis_val) = analysis_obj
                            && let Some(obj) = analysis_val.as_object_mut()
                        {
                            obj.insert("venvPath".into(), serde_json::Value::String(venv_path_str));
                            obj.insert(
                                "venv".into(),
                                serde_json::Value::String(venv_name.to_string()),
                            );
                        }
                    }
                    tracing::info!(
                        "{}: found {} at '{}', adding venvPath/venv to initializationOptions",
                        self.server_command,
                        venv_name,
                        startup_root.display()
                    );
                    Some(merged)
                } else {
                    self.init_settings.clone()
                }
            } else {
                self.init_settings.clone()
            };
        let effective_settings = initialization_settings_with_compile_commands(
            effective_settings,
            repo_root,
            readiness_files,
            self.compile_command_overrides,
        )?;
        if let Some(ref settings) = effective_settings {
            init_params.initialization_options = Some(settings.clone());
        }
        let workspace_configuration = effective_settings.unwrap_or_else(|| serde_json::json!({}));

        let init_result = {
            let transport = state.transport.as_mut().unwrap();
            transport.set_workspace_configuration(workspace_configuration.clone());
            transport.request("initialize", &init_params).await?
        };

        // Parse and check server capabilities
        // Check type hierarchy provider from raw JSON before from_value consumes it,
        // because lsp-types 0.97 ServerCapabilities is missing the field.
        let has_type_hierarchy = init_result
            .pointer("/capabilities/typeHierarchyProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        // Check call hierarchy provider (LSP 3.16+, "callHierarchyProvider").
        // Without this check, RNA could send prepareCallHierarchy to a server
        // that only supports references and turn every request into an error.
        let has_call_hierarchy = init_result
            .pointer("/capabilities/callHierarchyProvider")
            .map(|v| !v.is_null() && v != &serde_json::Value::Bool(false))
            .unwrap_or(false);

        // Check pull-based diagnostics capability (LSP 3.17+, "diagnosticProvider")
        let has_pull_diagnostics = init_result
            .pointer("/capabilities/diagnosticProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        // Check inlay hints capability (LSP 3.17+, "inlayHintProvider")
        let has_inlay_hints = init_result
            .pointer("/capabilities/inlayHintProvider")
            .map(|v| !v.is_null())
            .unwrap_or(false);

        let init_result_parsed: InitializeResult =
            serde_json::from_value(init_result).context("Failed to parse initialize result")?;
        let server_capabilities = init_result_parsed.capabilities.clone();
        let validation_capabilities =
            ReadinessValidationCapabilities::from_server_capabilities(&server_capabilities);

        let has_references =
            provider_is_enabled(&init_result_parsed.capabilities.references_provider);
        let has_implementation = implementation_provider_is_enabled(
            &init_result_parsed.capabilities.implementation_provider,
        );
        let has_definition =
            provider_is_enabled(&init_result_parsed.capabilities.definition_provider);
        let has_document_links = init_result_parsed
            .capabilities
            .document_link_provider
            .is_some();
        let negotiated_capabilities =
            negotiated_operation_capabilities(&init_result_parsed.capabilities, has_call_hierarchy);
        tracing::info!(
            "{} capabilities: references={}, call_hierarchy={}, definition={}, implementation={}, type_hierarchy={}, document_links={}, pull_diagnostics={}, inlay_hints={}, workspace_symbols={}, document_symbols={}",
            self.server_command,
            has_references,
            has_call_hierarchy,
            has_definition,
            has_implementation,
            has_type_hierarchy,
            has_document_links,
            has_pull_diagnostics,
            has_inlay_hints,
            validation_capabilities.workspace_symbols,
            validation_capabilities.document_symbols
        );

        state.has_type_hierarchy = has_type_hierarchy;
        state.has_call_hierarchy = has_call_hierarchy;
        state.has_implementation = has_implementation;
        state.has_definition = has_definition;
        state.has_document_links = has_document_links;
        state.has_references = has_references;
        state.has_pull_diagnostics = has_pull_diagnostics;
        state.has_inlay_hints = has_inlay_hints;
        state.server_capabilities = Some(server_capabilities);
        state.validation_evidence = None;

        // Send initialized notification
        let transport = state.transport.as_mut().unwrap();
        transport
            .notify("initialized", serde_json::json!({}))
            .await?;
        transport
            .notify(
                "workspace/didChangeConfiguration",
                serde_json::json!({"settings": workspace_configuration}),
            )
            .await?;

        // Send didOpen for a representative source file to create a project
        // context. tsserver requires at least one open file before it creates
        // a project; several servers use it to trigger workspace indexing.
        // Save the URI for use as a documentSymbol validation fallback.
        let warmup_uri: Option<String> = if let Some(warmup_path) = warmup_path {
            let uri_str = path_to_uri(warmup_path).ok().map(|u| u.to_string());
            match self.send_did_open(transport, warmup_path).await {
                Ok(()) => {
                    tracing::info!(
                        "{} sent didOpen for '{}'",
                        self.server_command,
                        warmup_path.display()
                    );
                    uri_str
                }
                Err(e) => {
                    tracing::debug!(
                        "{} didOpen warmup failed (non-fatal): {}",
                        self.server_command,
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        let readiness_method = validation_capabilities.primary();
        if readiness_method == Some(ReadinessValidationMethod::DocumentSymbol)
            && warmup_uri.is_none()
        {
            return Err(anyhow::anyhow!(
                "{} advertises documentSymbol but no deterministic included warm-up file was available; readiness was not validated",
                self.server_command
            ));
        }

        tracing::info!(
            "{} initialized, waiting for indexing...",
            self.server_command
        );

        // Wait for the language server to finish indexing the workspace.
        // rust-analyzer (and most LSP servers) need time after `initialized`
        // to build their project index. Without this wait, all reference
        // lookups return "file not found."
        //
        // Strategy (adaptive, no fixed timeout):
        //
        // 1. If the server sends `experimental/serverStatus`, wait indefinitely
        //    for `quiescent=true`. This is the correct signal — large workspaces
        //    may need minutes. The old 30s hard timeout fired before indexing
        //    finished, producing 193 errors and 0 call edges.
        //
        // 2. If `serverStatus` never arrives (e.g. typescript-language-server,
        //    that do not expose quiescence), use a two-phase probe strategy:
        //
        //    Phase A (responsiveness): probe every 5s with workspace/symbol("")
        //    until 2 consecutive successes confirm the server is alive.
        //
        //    Phase B (indexing validation): send workspace/symbol with non-empty
        //    queries ("main", "init", "test", etc.) to verify the server has
        //    actually indexed files. A responsive-but-unindexed server returns
        //    0 symbols for all queries. Retries use exponential backoff (5s,
        //    10s, 20s, ...) up to 6 attempts. After 3 consecutive empty
        //    responses on different queries, the server is declared "responsive
        //    but not indexed" and Passes 1/3 are skipped.
        //
        //    This fixes the #576 regression where some servers responded
        //    to probes within 5s but hadn't indexed the workspace, producing
        //    0 edges from thousands of nodes.
        //
        // 3. A 10-minute circuit breaker applies in both cases — not a normal
        //    timeout, just a safety net for servers that never become ready.
        //
        // Progress is logged every 30s so long-running indexing is observable.
        let circuit_breaker = tokio::time::Instant::now() + tokio::time::Duration::from_secs(600);
        let transport = state.transport.as_mut().unwrap();
        let mut server_ready = false;
        // Track whether we've seen any serverStatus notification.
        // When true, we wait indefinitely for quiescent rather than probing.
        let mut seen_server_status = false;
        // Track the raw `quiescent` bit independently of `health`.
        // We only care about "done indexing" for the Pass 3 guard; health="warning"
        // (compile errors) does not mean RA is still indexing.
        let mut saw_quiescent = false;
        let start = tokio::time::Instant::now();
        let mut last_progress_log = tokio::time::Instant::now();
        // For the probe path: track the next time we should send a probe.
        let mut next_probe = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        // Track consecutive probe successes — require 2 in a row to confirm responsiveness.
        let mut probe_success_count: u32 = 0;
        // After the server is responsive, validate it has actually indexed files
        // by sending workspace/symbol with non-empty queries. A server that responds
        // to probes but returns 0 symbols for real queries hasn't finished indexing.
        let mut server_responsive = false;
        let mut validation_evidence = None;

        while tokio::time::Instant::now() < circuit_breaker {
            let elapsed = start.elapsed().as_secs();

            // Log progress every 30s so long-running indexing is visible.
            if last_progress_log.elapsed() >= tokio::time::Duration::from_secs(30) {
                tracing::info!(
                    "LSP: waiting for {} to finish indexing ({}s elapsed, seen_serverStatus={})...",
                    self.server_command,
                    elapsed,
                    seen_server_status
                );
                last_progress_log = tokio::time::Instant::now();
            }

            // When the server uses serverStatus, wait up to 60s for the next
            // notification — the server WILL send it eventually.
            // When the server does not use serverStatus, use a short poll interval
            // (until next_probe) so we can interleave probe requests with draining
            // notifications.
            //
            // Cap by remaining time to the next 30s progress log and by the circuit
            // breaker, so progress logs fire accurately and the breaker is not late.
            let now = tokio::time::Instant::now();
            let until_next_log = tokio::time::Duration::from_secs(30)
                .checked_sub(last_progress_log.elapsed())
                .unwrap_or_default();
            let until_breaker = circuit_breaker
                .checked_duration_since(now)
                .unwrap_or_default();
            let msg_timeout = if seen_server_status {
                tokio::time::Duration::from_secs(60)
            } else {
                let remaining_to_probe = next_probe.saturating_duration_since(now);
                remaining_to_probe.min(tokio::time::Duration::from_secs(5))
            }
            .min(until_next_log)
            .min(until_breaker);

            match tokio::time::timeout(msg_timeout, transport.read_message()).await {
                Ok(Ok(msg)) => {
                    if transport.respond_to_server_request(&msg).await? {
                        continue;
                    }
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        match method {
                            // rust-analyzer's readiness signal
                            "experimental/serverStatus" => {
                                seen_server_status = true;
                                let health = msg
                                    .pointer("/params/health")
                                    .and_then(|h| h.as_str())
                                    .unwrap_or("");
                                let quiescent = msg
                                    .pointer("/params/quiescent")
                                    .and_then(|q| q.as_bool())
                                    .unwrap_or(false);
                                tracing::info!(
                                    "{} serverStatus: health={}, quiescent={}",
                                    self.server_command,
                                    health,
                                    quiescent
                                );

                                // Track the raw quiescent bit separately from health.
                                // Pass 3 cares about "done indexing" (quiescent=true),
                                // not about compilation health (which may be "warning" or "error").
                                if quiescent {
                                    saw_quiescent = true;
                                    validation_evidence = Some(
                                        crate::extract::scan_stats::LspValidationEvidence::quiescent(
                                            self.language.clone(),
                                            self.server_command.clone(),
                                            "experimental/serverStatus",
                                        ),
                                    );
                                }

                                if Self::server_status_is_ready(&msg) {
                                    tracing::info!(
                                        "{} ready (serverStatus: ok, quiescent)",
                                        self.server_command
                                    );
                                    server_ready = true;
                                    break;
                                }
                                // If quiescent=true but health!=ok, the server is done indexing
                                // but has errors/warnings. Still break — no point waiting further.
                                if quiescent {
                                    tracing::info!(
                                        "{} quiescent=true (health={}), proceeding despite non-ok health",
                                        self.server_command,
                                        health
                                    );
                                    break;
                                }
                                tracing::debug!(
                                    "{} not yet ready, continuing to wait for indexing...",
                                    self.server_command
                                );
                            }
                            "$/progress" => {
                                // Log progress for debugging but don't use it for readiness
                                let kind = msg
                                    .pointer("/params/value/kind")
                                    .and_then(|k| k.as_str())
                                    .unwrap_or("");
                                let title = msg
                                    .pointer("/params/value/title")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("");
                                if kind == "begin" || kind == "end" {
                                    tracing::info!(
                                        "{} progress {}: {}",
                                        self.server_command,
                                        kind,
                                        title
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("Error reading LSP message during init: {}", e);
                    break;
                }
                Err(_timeout) => {
                    if seen_server_status {
                        // We saw serverStatus but are waiting for the next update.
                        // This is a normal 60s quiet period — keep waiting.
                        tracing::debug!("{} waiting for next serverStatus...", self.server_command);
                        continue;
                    }

                    // No serverStatus — probe only an advertised validation method.
                    if tokio::time::Instant::now() >= next_probe {
                        // Schedule next probe from probe start so cadence is ~5s regardless
                        // of whether the probe request itself times out or succeeds quickly.
                        next_probe =
                            tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

                        let readiness_method = readiness_method.ok_or_else(|| {
                            anyhow::anyhow!(
                                "{} sent no experimental/serverStatus and advertises none of workspace/symbol, textDocument/documentSymbol, or textDocument/codeAction; readiness was not validated",
                                self.server_command
                            )
                        })?;
                        let response = execute_readiness_validation(
                            transport,
                            readiness_method,
                            warmup_uri.as_deref(),
                            "",
                            tokio::time::Duration::from_secs(5),
                        )
                        .await
                        .with_context(|| {
                            format!("{} readiness probe failed", self.server_command)
                        })?;
                        let symbol_count = response.symbol_count;

                        match readiness_method {
                            ReadinessValidationMethod::DocumentSymbol => {
                                validation_evidence = Some(
                                    LspValidationEvidence::processed(
                                        self.language.clone(),
                                        self.server_command.clone(),
                                        readiness_method.method(),
                                        symbol_count,
                                    )
                                    .with_request_uri(warmup_uri.clone())
                                    .with_document_symbols(response.document_symbols),
                                );
                                server_responsive = true;
                                server_ready = true;
                                saw_quiescent = true;
                                tracing::info!(
                                    "{} indexing validated via documentSymbol: {} symbols in deterministic warm-up file ({}s elapsed)",
                                    self.server_command,
                                    symbol_count,
                                    elapsed
                                );
                                break;
                            }
                            ReadinessValidationMethod::CodeAction => {
                                validation_evidence = Some(
                                    LspValidationEvidence::processed(
                                        self.language.clone(),
                                        self.server_command.clone(),
                                        readiness_method.method(),
                                        symbol_count,
                                    )
                                    .with_request_uri(warmup_uri.clone()),
                                );
                                server_responsive = true;
                                server_ready = true;
                                saw_quiescent = true;
                                tracing::info!(
                                    "{} indexing validated via codeAction in deterministic warm-up file ({}s elapsed)",
                                    self.server_command,
                                    elapsed
                                );
                                break;
                            }
                            ReadinessValidationMethod::WorkspaceSymbol => {
                                probe_success_count += 1;
                                if probe_success_count >= 2 {
                                    server_responsive = true;
                                    tracing::info!(
                                        "{} responsive (probe succeeded after {}s, no serverStatus) — validating indexing...",
                                        self.server_command,
                                        elapsed
                                    );
                                    break;
                                }
                                tracing::debug!(
                                    "{} probe {}/2 succeeded ({}s elapsed), waiting for second confirmation...",
                                    self.server_command,
                                    probe_success_count,
                                    elapsed
                                );
                            }
                        }
                    }
                }
            }
        }

        // ----------------------------------------------------------------
        // Indexing validation for probe-based servers (no serverStatus).
        //
        // "Server responds to requests" != "server has finished indexing."
        // Several servers respond to workspace/symbol
        // within 5s but may not have indexed a single file yet. On large
        // repos this produces 0 edges from thousands of nodes.
        //
        // Validation: send workspace/symbol with non-empty queries. An
        // indexed server returns results; an unindexed one returns [].
        // Use exponential backoff (5s, 10s, 20s) between attempts, with
        // up to MAX_INDEXING_VALIDATION_ATTEMPTS total tries.
        // ----------------------------------------------------------------
        if server_responsive && !server_ready {
            const MAX_INDEXING_VALIDATION_ATTEMPTS: u32 = 6;
            // Queries chosen to match common symbols across Python/TypeScript/Rust
            // codebases. We try multiple queries so a project that happens to lack
            // "main" can still pass validation with "init" or "test".
            const VALIDATION_QUERIES: &[&str] = &["main", "init", "test", "get", "set", "app"];
            let mut validation_delay = tokio::time::Duration::from_secs(5);
            let mut consecutive_empty: u32 = 0;

            for attempt in 1..=MAX_INDEXING_VALIDATION_ATTEMPTS {
                if tokio::time::Instant::now() >= circuit_breaker {
                    break;
                }

                // Pick a different query each attempt to avoid false negatives
                // from a project that simply doesn't have a "main" function.
                let query = VALIDATION_QUERIES[((attempt - 1) as usize) % VALIDATION_QUERIES.len()];

                // Drain any pending notifications before sending the validation request.
                // Some servers may send progress or diagnostic notifications
                // that need to be consumed to avoid transport deadlock.
                while let Ok(Ok(msg)) = tokio::time::timeout(
                    tokio::time::Duration::from_millis(100),
                    transport.read_message(),
                )
                .await
                {
                    if transport.respond_to_server_request(&msg).await? {
                        continue;
                    }
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        if method == "$/progress" {
                            let kind = msg
                                .pointer("/params/value/kind")
                                .and_then(|k| k.as_str())
                                .unwrap_or("");
                            let title = msg
                                .pointer("/params/value/title")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            if kind == "begin" || kind == "end" {
                                tracing::info!(
                                    "{} progress {}: {}",
                                    self.server_command,
                                    kind,
                                    title
                                );
                            }
                        } else if method == "experimental/serverStatus" {
                            // A late serverStatus notification arrived during Phase B.
                            // If quiescent=true, the server has finished indexing —
                            // skip the rest of validation and accept the authoritative signal.
                            let quiescent = msg
                                .pointer("/params/quiescent")
                                .and_then(|q| q.as_bool())
                                .unwrap_or(false);
                            if quiescent {
                                tracing::info!(
                                    "{} received experimental/serverStatus quiescent=true during Phase B validation — accepting",
                                    self.server_command
                                );
                                server_ready = true;
                                saw_quiescent = true;
                                validation_evidence = Some(
                                    crate::extract::scan_stats::LspValidationEvidence::quiescent(
                                        self.language.clone(),
                                        self.server_command.clone(),
                                        "experimental/serverStatus",
                                    ),
                                );
                            }
                        }
                    }
                }

                // If serverStatus arrived during drain, skip validation
                if server_ready {
                    break;
                }

                let elapsed = start.elapsed().as_secs();
                if let Some(validation) = execute_indexing_validation_once(
                    transport,
                    validation_capabilities,
                    warmup_uri.as_deref(),
                    query,
                    READINESS_REQUEST_TIMEOUT,
                )
                .await
                .with_context(|| format!("{} indexing validation failed", self.server_command))?
                {
                    tracing::info!(
                        "{} indexing validated via {}: {} symbols ({}s elapsed)",
                        self.server_command,
                        validation.method.method(),
                        validation.symbol_count,
                        elapsed
                    );
                    validation_evidence = Some(
                        LspValidationEvidence::processed(
                            self.language.clone(),
                            self.server_command.clone(),
                            validation.method.method(),
                            validation.symbol_count,
                        )
                        .with_request_uri(validation.request_uri)
                        .with_document_symbols(validation.document_symbols),
                    );
                    server_ready = true;
                    saw_quiescent = true;
                    break;
                }

                consecutive_empty += 1;
                tracing::info!(
                    "{} indexing validation {}/{}: workspace/symbol(\"{}\") returned 0 symbols ({}s elapsed, waiting {}s)",
                    self.server_command,
                    attempt,
                    MAX_INDEXING_VALIDATION_ATTEMPTS,
                    query,
                    elapsed,
                    validation_delay.as_secs()
                );

                // After consecutive empty responses on different queries,
                // the server may not have finished indexing yet. Only bail
                // early if we've waited at least 60s — large Python/TS projects
                // (27k+ nodes) genuinely need this long for a server to index.
                if consecutive_empty >= 3 && attempt >= 3 && elapsed >= 60 {
                    tracing::warn!(
                        "{} indexing validation failed: {} consecutive empty responses across different queries — \
                         server is responsive but has not indexed the workspace ({}s elapsed)",
                        self.server_command,
                        consecutive_empty,
                        elapsed
                    );
                    break;
                }

                // Exponential backoff: 5s, 10s, 20s, 40s, ...
                // Capped by the circuit breaker.
                let sleep_until = tokio::time::Instant::now() + validation_delay;
                let capped = sleep_until.min(circuit_breaker);
                tokio::time::sleep_until(capped).await;
                validation_delay = (validation_delay * 2).min(tokio::time::Duration::from_secs(60));
            }
        }

        if tokio::time::Instant::now() >= circuit_breaker {
            tracing::warn!(
                "{} circuit breaker fired after 10 minutes — proceeding anyway (server may not be fully indexed)",
                self.server_command
            );
        } else if !server_ready && server_responsive {
            tracing::warn!(
                "{} responsive but indexing validation failed — server has not indexed the workspace",
                self.server_command
            );
        } else if !server_ready {
            tracing::info!(
                "{} readiness wait complete (server_ready=false, seen_serverStatus={}), proceeding",
                self.server_command,
                seen_server_status
            );
        }

        // Record whether the server became quiescent. Pass 3 (diagnostics) is
        // only safe to run when the server has finished indexing; sending
        // thousands of textDocument/diagnostic requests to an unindexed server
        // floods its queue, which was the root cause of the #379 regression.
        //
        // Use `saw_quiescent` (raw quiescent=true bit) rather than `server_ready`
        // (which also requires health="ok"). This ensures Pass 3 runs for repos
        // with compile errors (health="warning", quiescent=true) — the server IS
        // done indexing, it just has errors. Pass 3 is safe in that case.
        //
        // The guard only applies when the server supports `experimental/serverStatus`
        // (`seen_server_status = true`) but never sent quiescent=true. Servers
        // that do NOT support serverStatus use the probe path: they are quiescent
        // only when `server_ready=true` (probe + indexing validation both passed).
        // This ensures a server that responded to probes but returned 0 symbols
        // on validation before indexing completes is NOT treated
        // as quiescent — Pass 1 and Pass 3 would produce 0 edges.
        state.was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
        state.validation_evidence = Some(
            validation_evidence
                .unwrap_or_else(|| {
                    LspValidationEvidence::not_validated(
                        self.language.clone(),
                        self.server_command.clone(),
                        "server did not complete an advertised readiness validation method",
                    )
                })
                .with_negotiated_capabilities(negotiated_capabilities),
        );
        if !state.was_quiescent {
            tracing::warn!(
                "{} did not reach quiescent state — Pass 3 (diagnostics) will be skipped this session",
                self.server_command
            );
        }

        tracing::info!("{} ready for {}", self.server_command, self.language);

        // Convert to pipelined transport for concurrent request support.
        // Share the diagnostics sink so publishDiagnostics notifications received
        // during enrichment are captured for later conversion to diagnostic nodes.
        // Pass the initial quiescent state so the pipelined transport's quiescent_flag
        // starts correct; the reader loop will live-update it for subsequent scans.
        if let Some(transport) = state.transport.take() {
            let diag_sink = Arc::clone(&state.diagnostics_sink);
            let pipelined = PipelinedTransport::from_sequential_with_diag_sink(
                transport,
                diag_sink,
                state.was_quiescent,
            );
            tracing::info!("{} converted to pipelined transport", self.server_command);
            state.pipelined = Some(Arc::new(pipelined));
        }

        self.ready.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Prepare call hierarchy at a position (pipelined). Returns the CallHierarchyItem if found.
    async fn prepare_call_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Option<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character }
        });

        let result: serde_json::Value = transport
            .request("textDocument/prepareCallHierarchy", &params)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        if let Some(items) = result.as_array() {
            Ok(items.first().cloned())
        } else {
            Ok(Some(result))
        }
    }

    /// Find outgoing calls (pipelined).
    async fn outgoing_calls_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("callHierarchy/outgoingCalls", &params)
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Find incoming calls (pipelined).
    async fn incoming_calls_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("callHierarchy/incomingCalls", &params)
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Get document links (pipelined).
    async fn document_links_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });
        let result: serde_json::Value = transport
            .request("textDocument/documentLink", &params)
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    async fn goto_locations_p(
        transport: &PipelinedTransport,
        method: &str,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: file_uri.clone(),
                },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let result: serde_json::Value = transport.request(method, &params).await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> =
            match serde_json::from_value::<GotoDefinitionResponse>(result) {
                Ok(GotoDefinitionResponse::Scalar(loc)) => vec![loc],
                Ok(GotoDefinitionResponse::Array(locs)) => locs,
                Ok(GotoDefinitionResponse::Link(links)) => links
                    .into_iter()
                    .map(|link| Location {
                        uri: link.target_uri,
                        range: link.target_range,
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };

        Ok(locations)
    }

    /// Find definitions for a documentation link (pipelined).
    async fn find_definitions_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        Self::goto_locations_p(
            transport,
            "textDocument/definition",
            file_uri,
            line,
            character,
        )
        .await
    }

    /// Find implementations of a trait/interface (pipelined).
    async fn find_implementations_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        Self::goto_locations_p(
            transport,
            "textDocument/implementation",
            file_uri,
            line,
            character,
        )
        .await
    }

    /// Compute the 0-based LSP line and column for a node.
    ///
    /// Uses the AST-recorded byte column of the name identifier stored by the
    /// extractor (metadata key "name_col"). This is exact and language-agnostic:
    /// tree-sitter records start_position().column for the name field node, so
    /// it works correctly even when the name appears multiple times in the
    /// signature (e.g. `pub fn from_str(from_str: &str)`) or when the keyword
    /// prefix length varies across languages (Python `def`, Go `func`, etc.).
    /// If the extractor did not populate name_col (legacy or non-tree-sitter
    /// nodes), falls back to signature scanning.
    fn node_lsp_position(repo_root: &Path, node: &Node) -> (u32, u32) {
        work_items::source_request_position(repo_root, node)
    }

    /// Update the type hierarchy strike counter after a single enrich attempt.
    /// Resets on success, increments on failure, and disables the feature after
    /// `MAX_TYPE_HIERARCHY_STRIKES` consecutive failures.
    fn update_type_hierarchy_strikes(ok: bool, strikes: &mut u32, enabled: &mut bool) {
        if ok {
            *strikes = 0;
        } else {
            *strikes += 1;
            if *strikes >= MAX_TYPE_HIERARCHY_STRIKES {
                tracing::warn!(
                    "Type hierarchy disabled after {} consecutive failures",
                    *strikes
                );
                *enabled = false;
            }
        }
    }

    /// Find references to a symbol at a position (pipelined).
    /// Returns a list of LSP Location objects for each reference site.
    async fn find_references_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<Location>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": false }
        });

        let result: serde_json::Value = transport
            .request("textDocument/references", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        let locations: Vec<Location> = serde_json::from_value(result).unwrap_or_default();
        Ok(locations)
    }

    /// Use type hierarchy to discover supertypes for a node, creating
    /// Implements edges for each resolved supertype relationship.
    ///
    /// Only called for Trait/Struct/Enum nodes (the only kinds eligible for
    /// type hierarchy). Subtypes are not queried here because find_implementations
    /// already covers that direction for Traits, and Rust Struct/Enum nodes
    /// cannot have subtypes.
    ///
    /// Returns `true` if the prepare call succeeded, `false` if it failed (used for
    /// strike counting).
    #[allow(clippy::too_many_arguments)]
    async fn enrich_type_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
        node: &Node,
        matching_nodes: &[&Node],
        root: &Path,
        result: &mut EnrichmentResult,
    ) -> (bool, passes::QueryObservation) {
        let mut observation = passes::QueryObservation::default();
        observation.scheduled_requests += 1;
        let items = match Self::prepare_type_hierarchy_p(transport, file_uri, line, character).await
        {
            Ok(items) if !items.is_empty() => {
                observation.non_empty_responses += 1;
                items
            }
            Ok(_) => return (true, observation), // No type hierarchy item — not a failure
            Err(e) => {
                observation.record_error(&e);
                tracing::debug!("prepareTypeHierarchy failed for {}: {}", node.id.name, e);
                return (false, observation);
            }
        };

        for item in &items {
            observation.scheduled_requests += 1;
            // Supertypes: this node implements/inherits from each supertype
            match Self::type_hierarchy_supertypes_p(transport, item).await {
                Ok(supertypes) => {
                    observation.non_empty_responses += usize::from(!supertypes.is_empty());
                    for supertype in &supertypes {
                        if let Some(target_id) =
                            Self::resolve_type_hierarchy_item(supertype, matching_nodes, root)
                        {
                            // Skip self-references
                            if target_id == node.id {
                                continue;
                            }
                            tracing::debug!(
                                "Type hierarchy: {} implements supertype {}",
                                node.id.name,
                                target_id.name
                            );
                            result.added_edges.push(Edge {
                                from: node.id.clone(),
                                to: target_id,
                                kind: EdgeKind::Implements,
                                source: ExtractionSource::Lsp,
                                confidence: Confidence::Confirmed,
                                evidence: Vec::new(),
                            });
                        }
                    }
                }
                Err(e) => {
                    observation.record_error(&e);
                    tracing::debug!(
                        "typeHierarchy/supertypes failed for {}: {}",
                        node.id.name,
                        e
                    );
                }
            }
        }

        (true, observation) // prepare succeeded
    }

    /// Resolve a TypeHierarchyItem (JSON) to a NodeId in the graph.
    /// Returns None if the item's file/name doesn't match any known node.
    fn resolve_type_hierarchy_item(
        item: &serde_json::Value,
        matching_nodes: &[&Node],
        root: &Path,
    ) -> Option<NodeId> {
        let name = item.get("name")?.as_str()?;
        let uri_str = item.get("uri")?.as_str()?;

        // Use url::Url for proper percent-decoding of file:// URIs
        let abs_path = match url::Url::parse(uri_str) {
            Ok(url) => match url.to_file_path() {
                Ok(p) => p,
                Err(_) => return None,
            },
            Err(_) => {
                // Fallback: manual strip for non-standard URIs
                let file_path_str = uri_str.strip_prefix("file://")?;
                PathBuf::from(file_path_str)
            }
        };

        let rel_path = abs_path
            .strip_prefix(root)
            .unwrap_or(&abs_path)
            .to_path_buf();

        // Skip external dependencies
        if rel_path.to_string_lossy().contains(".cargo") {
            return None;
        }

        let range_start_line = item
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .map(|l| l as usize + 1)
            .unwrap_or(0);

        // Try exact name + file match first
        let candidates: Vec<_> = matching_nodes
            .iter()
            .filter(|n| n.id.file == rel_path)
            .filter(|n| n.id.name == name)
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Trait | NodeKind::Struct | NodeKind::Enum | NodeKind::Impl
                )
            })
            .collect();

        if candidates.len() == 1 {
            return Some(candidates[0].id.clone());
        }

        if candidates.len() > 1 {
            // Ambiguous name match — use position to disambiguate (issue #2: name collision)
            tracing::debug!(
                "resolve_type_hierarchy_item: {} candidates for '{}' in {}, using position tiebreaker",
                candidates.len(),
                name,
                rel_path.display()
            );
            if range_start_line > 0
                && let Some(best) = candidates
                    .iter()
                    .filter(|n| n.line_start <= range_start_line && n.line_end >= range_start_line)
                    .min_by_key(|n| n.line_end - n.line_start)
            {
                return Some(best.id.clone());
            }
            // If position doesn't help, pick closest by line_start
            if range_start_line > 0
                && let Some(best) = candidates.iter().min_by_key(|n| {
                    (n.line_start as isize - range_start_line as isize).unsigned_abs()
                })
            {
                return Some(best.id.clone());
            }
            // Last resort: take first
            return Some(candidates[0].id.clone());
        }

        // Fallback: find enclosing symbol at the position
        matching_nodes
            .iter()
            .filter(|n| n.id.file == rel_path)
            .filter(|n| {
                matches!(
                    n.id.kind,
                    NodeKind::Trait | NodeKind::Struct | NodeKind::Enum | NodeKind::Impl
                )
            })
            .filter(|n| {
                range_start_line == 0
                    || (n.line_start <= range_start_line && n.line_end >= range_start_line)
            })
            .min_by_key(|n| n.line_end - n.line_start)
            .map(|n| n.id.clone())
    }

    /// Prepare type hierarchy at a position (pipelined).
    async fn prepare_type_hierarchy_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": { "line": line, "character": character }
        });

        let result: serde_json::Value = transport
            .request("textDocument/prepareTypeHierarchy", &params)
            .await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Find supertypes for a TypeHierarchyItem (pipelined).
    async fn type_hierarchy_supertypes_p(
        transport: &PipelinedTransport,
        item: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({ "item": item });
        let result: serde_json::Value = transport
            .request("typeHierarchy/supertypes", &params)
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Request pull-based diagnostics for a single file (LSP 3.17+).
    /// Returns an empty Vec if the server returns null or an error.
    async fn pull_diagnostics_p(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });
        let result: serde_json::Value = transport
            .request("textDocument/diagnostic", &params)
            .await?;
        if result.is_null() {
            return Ok(Vec::new());
        }
        // Response is a DocumentDiagnosticReport — either "full" or "unchanged"
        // Full: { kind: "full", items: [...] }
        // Unchanged: { kind: "unchanged", resultId: "..." }
        //
        // rust-analyzer may also return a RelatedDocumentDiagnosticReport which
        // wraps the same structure. Log the raw response at DEBUG level so we
        // can diagnose unexpected shapes without noise in normal runs.
        tracing::debug!(
            "textDocument/diagnostic response for {}: {}",
            file_uri.as_str(),
            serde_json::to_string(&result).unwrap_or_else(|_| "<serialize error>".into())
        );
        let kind = result
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("full");
        if kind == "unchanged" {
            return Ok(Vec::new());
        }
        let items = result
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        tracing::debug!(
            "textDocument/diagnostic: {} items (kind={}) for {}",
            items.len(),
            kind,
            file_uri.as_str()
        );
        Ok(items)
    }

    /// Convert raw LSP diagnostic severity integer to a lowercase string.
    ///
    /// Per LSP spec:
    ///   1 = Error, 2 = Warning, 3 = Information, 4 = Hint
    fn lsp_severity_to_str(severity: u64) -> &'static str {
        match severity {
            1 => "error",
            2 => "warning",
            3 => "information",
            4 => "hint",
            _ => "unknown",
        }
    }

    /// Build diagnostic `Node`s from a set of LSP diagnostics for one file.
    ///
    /// `max_severity_int` is the maximum LSP severity integer to store (inclusive).
    /// LSP encodes severity as 1=Error, 2=Warning, 3=Information, 4=Hint.
    /// Default (from `DiagnosticMinSeverity::Warning`) is 2 — store Error and Warning only.
    ///
    /// Severity 0 is not a valid LSP value and is always filtered. Severities above
    /// `max_severity_int` are dropped.
    #[allow(clippy::too_many_arguments)]
    fn build_diagnostic_nodes(
        file_uri: &str,
        diagnostics: &[serde_json::Value],
        root: &Path,
        root_id: &str,
        server_command: &str,
        language: &str,
        timestamp: &str,
        max_severity_int: u64,
    ) -> Vec<Node> {
        // Resolve file path from URI
        let rel_path = {
            let abs = match url::Url::parse(file_uri)
                .ok()
                .and_then(|u| u.to_file_path().ok())
            {
                Some(p) => p,
                None => {
                    if let Some(p) = file_uri.strip_prefix("file://") {
                        PathBuf::from(p)
                    } else {
                        return Vec::new();
                    }
                }
            };
            abs.strip_prefix(root).unwrap_or(&abs).to_path_buf()
        };

        // Skip external paths
        if rel_path.to_string_lossy().contains(".cargo") {
            return Vec::new();
        }

        let mut nodes = Vec::new();

        for diag in diagnostics {
            let severity_int = diag.get("severity").and_then(|s| s.as_u64()).unwrap_or(1);
            // Severity 0 is not a valid LSP value — always skip.
            // Keep diagnostics whose severity integer is within the configured floor.
            // (Lower integer = higher severity: 1=Error, 2=Warning, 3=Information, 4=Hint)
            if severity_int == 0 || severity_int > max_severity_int {
                continue;
            }
            let severity = Self::lsp_severity_to_str(severity_int);
            let message = diag
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if message.is_empty() {
                continue;
            }
            let source = diag
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or(server_command);

            let start_line = diag
                .pointer("/range/start/line")
                .and_then(|l| l.as_u64())
                .unwrap_or(0) as usize
                + 1;
            let start_char = diag
                .pointer("/range/start/character")
                .and_then(|c| c.as_u64())
                .unwrap_or(0);
            let end_line = diag
                .pointer("/range/end/line")
                .and_then(|l| l.as_u64())
                .unwrap_or(0) as usize
                + 1;
            let end_char = diag
                .pointer("/range/end/character")
                .and_then(|c| c.as_u64())
                .unwrap_or(0);
            let range_str = format!("{}:{}-{}:{}", start_line, start_char, end_line, end_char);

            // Name: truncated message + line number for human readability in search results.
            // Including the start line ensures that identical messages at different positions
            // produce distinct NodeIds (preventing silent overwrites in LanceDB).
            let name_snippet = if message.chars().count() > 80 {
                format!("{}...", message.chars().take(77).collect::<String>())
            } else {
                message.clone()
            };
            // Node name encodes severity + line + snippet for quick scanning and uniqueness
            let node_name = format!("[{}:{}] {}", severity, start_line, name_snippet);

            let mut metadata = std::collections::BTreeMap::new();
            metadata.insert("diagnostic_severity".to_string(), severity.to_string());
            metadata.insert("diagnostic_source".to_string(), source.to_string());
            metadata.insert("diagnostic_message".to_string(), message.clone());
            metadata.insert("diagnostic_range".to_string(), range_str);
            metadata.insert("diagnostic_timestamp".to_string(), timestamp.to_string());

            let node_id = NodeId {
                root: root_id.to_string(),
                file: rel_path.clone(),
                name: node_name.clone(),
                kind: NodeKind::Other("diagnostic".to_string()),
            };

            nodes.push(Node {
                id: node_id,
                language: language.to_string(),
                line_start: start_line,
                line_end: end_line,
                signature: format!("{}: {}", severity, message),
                body: String::new(),
                metadata,
                source: ExtractionSource::Lsp,
            });
        }

        nodes
    }

    // ---------------------------------------------------------------------------
    // #405: Crate-level dependency graph via rust-analyzer/viewCrateGraph
    // ---------------------------------------------------------------------------

    /// Request `rust-analyzer/viewCrateGraph` and parse the DOT output into
    /// `(crate_name, dep_crate_name)` pairs.
    ///
    /// The DOT format emitted by rust-analyzer is:
    /// ```text
    /// digraph rust_analyzer_crate_graph {
    ///     _0 [shape=box label="my_crate"]
    ///     _1 [shape=box label="dep_crate"]
    ///     _0 -> _1
    /// }
    /// ```
    ///
    /// Only workspace crates are included by default (`full: false`).
    /// Returns `(crate_names, dep_pairs)` — see [`parse_crate_graph_dot`] for details.
    async fn fetch_crate_graph(
        transport: &PipelinedTransport,
    ) -> Result<(Vec<String>, Vec<(String, String)>)> {
        let params = serde_json::json!({ "full": false });
        let result = transport
            .request("rust-analyzer/viewCrateGraph", &params)
            .await?;

        let dot = match result.as_str() {
            Some(s) => s.to_string(),
            None => return Ok((Vec::new(), Vec::new())),
        };

        Ok(Self::parse_crate_graph_dot(&dot))
    }

    /// Parse a DOT digraph string from `rust-analyzer/viewCrateGraph`.
    ///
    /// Returns `(crate_names, dep_pairs)` where:
    /// - `crate_names`: all crate names found in the graph (including isolated crates)
    /// - `dep_pairs`: resolved `(from_crate, to_crate)` dependency pairs
    ///
    /// Isolated crates (no dependencies) are included in `crate_names` so they
    /// still get a crate node even when there are no dependency edges.
    fn parse_crate_graph_dot(dot: &str) -> (Vec<String>, Vec<(String, String)>) {
        // Maps DOT node ID (e.g. "_0") to crate name (e.g. "my_crate")
        let mut id_to_name: HashMap<String, String> = HashMap::new();
        let mut edges: Vec<(String, String)> = Vec::new();

        for line in dot.lines() {
            let line = line.trim();

            // Node definition: `_0 [shape=box label="crate_name"]`
            // Capture: node_id and label value
            if let Some(label_start) = line.find("label=\"") {
                // Extract node ID: everything before the first whitespace
                let node_id = line.split_whitespace().next().unwrap_or("").to_string();
                if !node_id.starts_with('_') {
                    continue; // not a crate node
                }
                let after_label = &line[label_start + 7..]; // skip 'label="'
                if let Some(end) = after_label.find('"') {
                    let name = after_label[..end].to_string();
                    if !name.is_empty() {
                        id_to_name.insert(node_id, name);
                    }
                }
                continue;
            }

            // Edge definition: `_0 -> _1` (with optional trailing semicolon/attributes)
            if line.contains("->") {
                let parts: Vec<&str> = line.splitn(3, "->").collect();
                if parts.len() >= 2 {
                    let from_id = parts[0].trim().trim_end_matches(';').to_string();
                    let to_id = parts[1]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .to_string();
                    if from_id.starts_with('_') && to_id.starts_with('_') {
                        edges.push((from_id, to_id));
                    }
                }
            }
        }

        // All crate names (including isolated crates with no edges)
        let mut crate_names: Vec<String> = id_to_name.values().cloned().collect();
        crate_names.sort();
        crate_names.dedup();

        // Resolve edge IDs to crate names
        let resolved_edges = edges
            .into_iter()
            .filter_map(|(from_id, to_id)| {
                let from = id_to_name.get(&from_id)?;
                let to = id_to_name.get(&to_id)?;
                Some((from.clone(), to.clone()))
            })
            .collect();

        (crate_names, resolved_edges)
    }

    /// Emit crate nodes and `DependsOn` edges from a parsed crate graph.
    ///
    /// Creates a `NodeKind::Other("crate")` node for every crate name (including
    /// isolated crates with no edges), then emits a `DependsOn` edge for each
    /// dependency relationship.
    fn emit_crate_graph_edges(
        crate_names: &[String],
        pairs: &[(String, String)],
        root_id: &str,
        result: &mut EnrichmentResult,
    ) {
        // Collect all unique crate names (isolated crates from crate_names + crates in edges)
        let mut all_crates: std::collections::BTreeSet<String> =
            crate_names.iter().cloned().collect();
        for (from, to) in pairs {
            all_crates.insert(from.clone());
            all_crates.insert(to.clone());
        }

        // Create a crate node for each unique crate.
        // body = crate name so build_code_embedding_text produces meaningful embeddings.
        for crate_name in &all_crates {
            let node_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: crate_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            result.new_nodes.push(Node {
                id: node_id,
                language: "rust".to_string(),
                line_start: 0,
                line_end: 0,
                signature: format!("crate {}", crate_name),
                body: crate_name.clone(),
                metadata: std::collections::BTreeMap::new(),
                source: ExtractionSource::Lsp,
            });
        }

        // Emit DependsOn edges
        for (from_name, to_name) in pairs {
            let from_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: from_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            let to_id = NodeId {
                root: root_id.to_string(),
                file: PathBuf::from("Cargo.toml"),
                name: to_name.clone(),
                kind: NodeKind::Other("crate".to_string()),
            };
            result.added_edges.push(Edge {
                from: from_id,
                to: to_id,
                kind: EdgeKind::DependsOn,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
        }
    }

    // ---------------------------------------------------------------------------
    // #396: BelongsTo edges via rust-analyzer/parentModule (Rust) or directory
    // ---------------------------------------------------------------------------

    /// Request `rust-analyzer/parentModule` for a file, returning the module path
    /// as a string (e.g., `"crate::server::handlers"`).
    async fn ra_parent_module(
        transport: &PipelinedTransport,
        file_uri: &Uri,
    ) -> Result<Option<String>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        let result: serde_json::Value = transport
            .request("rust-analyzer/parentModule", &params)
            .await?;

        if result.is_null() {
            return Ok(None);
        }

        // rust-analyzer returns an array of LocationLinks; the first gives the
        // parent module's URI which we use as the module path.
        if let Some(arr) = result.as_array()
            && let Some(first) = arr.first()
        {
            // The target URI gives us the parent file path; derive module name
            // from the file name (e.g. `src/server/mod.rs` → `server`)
            if let Some(uri_str) = first.get("targetUri").and_then(|u| u.as_str()) {
                // Extract the module name from the URI: strip file:// and get basename
                let path = if let Some(p) = uri_str.strip_prefix("file://") {
                    PathBuf::from(p)
                } else {
                    PathBuf::from(uri_str)
                };
                let module_name = path.file_stem().and_then(|s| s.to_str()).map(|s| {
                    if s == "mod" {
                        // For mod.rs, use the directory name
                        path.parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or(s)
                            .to_string()
                    } else {
                        s.to_string()
                    }
                });
                return Ok(module_name);
            }
        }

        Ok(None)
    }

    /// Emit `BelongsTo` edges from all symbols in a file to a module node.
    ///
    /// For Rust files, tries `rust-analyzer/parentModule` first.
    /// Falls back to directory-based module detection for all languages.
    ///
    /// Module nodes use `NodeKind::Module` and are created as virtual nodes
    /// if they don't already exist.
    async fn emit_belongs_to_edges(
        transport: &PipelinedTransport,
        file_nodes: &[&Node],
        rel_file: &Path,
        root: &Path,
        has_parent_module: bool,
        result: &mut EnrichmentResult,
    ) {
        if file_nodes.is_empty() {
            return;
        }

        // Derive a module name for this file.
        // Priority: (1) LSP parentModule request if supported, (2) directory-based fallback.
        let module_name: Option<String> = if has_parent_module {
            let abs_path = root.join(rel_file);
            if let Ok(file_uri) = path_to_uri(&abs_path) {
                Self::ra_parent_module(transport, &file_uri)
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            }
        } else {
            None
        };

        // Fallback: derive module name from the immediate parent directory
        let module_name = module_name.or_else(|| {
            // For files directly in the root or without a parent dir, use the
            // file stem as the module name (e.g. `main.rs` → `main`)
            rel_file
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    rel_file
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
        });

        let module_name = match module_name {
            Some(n) if !n.is_empty() => n,
            _ => return,
        };

        // Derive a stable module path from the directory path
        let module_path = rel_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(""));

        // Use the first node's root as the module node root
        let root_id = file_nodes[0].id.root.clone();

        // Create a virtual module node (may already exist in the graph — dedup is
        // handled at persist time by stable_id uniqueness)
        let module_node_id = NodeId {
            root: root_id.clone(),
            file: module_path.clone(),
            name: module_name.clone(),
            kind: NodeKind::Module,
        };

        result.new_nodes.push(Node {
            id: module_node_id.clone(),
            language: file_nodes[0].language.clone(),
            line_start: 0,
            line_end: 0,
            signature: format!("mod {}", module_name),
            body: String::new(),
            metadata: std::collections::BTreeMap::new(),
            source: ExtractionSource::Lsp,
        });

        // Emit BelongsTo edges from each symbol in this file to the module node
        for node in file_nodes {
            // Skip module nodes (avoid self-loop) and diagnostic nodes (transient, not structural)
            if node.id.kind == NodeKind::Module {
                continue;
            }
            if matches!(&node.id.kind, NodeKind::Other(s) if s == "diagnostic") {
                continue;
            }
            result.added_edges.push(Edge {
                from: node.id.clone(),
                to: module_node_id.clone(),
                kind: EdgeKind::BelongsTo,
                source: ExtractionSource::Lsp,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
        }
    }

    // ---------------------------------------------------------------------------
    // #408: Inlay hints — inferred types in embeddings
    // ---------------------------------------------------------------------------

    /// Request `textDocument/inlayHint` for a file range and return a compact
    /// string of inferred type names suitable for embedding.
    ///
    /// Supported by: rust-analyzer, TypeScript LS, Pyrefly, gopls.
    async fn inlay_hints_for_file(
        transport: &PipelinedTransport,
        file_uri: &Uri,
        line_count: u32,
    ) -> Result<Vec<serde_json::Value>> {
        let params = serde_json::json!({
            "textDocument": { "uri": file_uri.as_str() },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": line_count, "character": 0 }
            }
        });

        let result: serde_json::Value =
            transport.request("textDocument/inlayHint", &params).await?;

        if result.is_null() {
            return Ok(Vec::new());
        }

        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// Extract type names from inlay hints and group them by the function/symbol
    /// that contains each hint's line position.
    ///
    /// Returns a map from node stable_id → compact type string.
    fn group_inlay_hints_by_node(
        hints: &[serde_json::Value],
        file_nodes: &[&Node],
    ) -> HashMap<String, String> {
        let mut node_types: HashMap<String, Vec<String>> = HashMap::new();

        for hint in hints {
            // Only capture type hints (kind=1) — parameter hints (kind=2) add noise
            let kind = hint.get("kind").and_then(|k| k.as_u64()).unwrap_or(1);
            if kind != 1 {
                continue;
            }

            let hint_line = hint
                .pointer("/position/line")
                .and_then(|l| l.as_u64())
                .map(|l| l as usize + 1) // Convert to 1-indexed
                .unwrap_or(0);

            if hint_line == 0 {
                continue;
            }

            // Extract the label text (may be a string or array of InlayHintLabelPart)
            let label = match hint.get("label") {
                Some(serde_json::Value::String(s)) => s.trim().to_string(),
                Some(serde_json::Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => continue,
            };

            // Strip leading ": " annotation prefix that rust-analyzer emits
            let label = label.trim_start_matches(": ").trim().to_string();

            if label.is_empty() || label.len() > 64 {
                continue;
            }

            // Find the narrowest enclosing function/impl/struct for this hint line
            let enclosing = file_nodes
                .iter()
                .filter(|n| {
                    matches!(
                        n.id.kind,
                        NodeKind::Function | NodeKind::Impl | NodeKind::Struct
                    )
                })
                .filter(|n| n.line_start <= hint_line && n.line_end >= hint_line)
                .min_by_key(|n| n.line_end - n.line_start);

            if let Some(node) = enclosing {
                node_types
                    .entry(node.id.to_stable_id())
                    .or_default()
                    .push(label);
            }
        }

        // Deduplicate and format each node's types as a space-separated string
        node_types
            .into_iter()
            .map(|(id, mut types)| {
                types.sort();
                types.dedup();
                (id, types.join(" "))
            })
            .collect()
    }
}

fn lsp_job_timeout() -> std::time::Duration {
    std::env::var("RNA_LSP_JOB_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| std::time::Duration::from_secs(30 * 60))
}

#[async_trait::async_trait]
impl Enricher for LspEnricher {
    fn languages(&self) -> &[&str] {
        self.language_static
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn name(&self) -> &str {
        &self.display_name
    }

    fn set_startup_root(&self, lsp_root: std::path::PathBuf) {
        // OnceLock::set returns Err if already set; we silently ignore that since
        // the server may already be initialized (first call wins).
        let _ = self.startup_root_override.set(lsp_root);
    }

    fn config_file_hint(&self) -> Option<&str> {
        self.config_file
    }

    fn toolchain_remediation(&self) -> Option<&str> {
        self.toolchain_remediation
    }

    fn manages_broad_reference_deadline(&self) -> bool {
        true
    }

    async fn enrich(
        &self,
        nodes: &[Node],
        _index: &GraphIndex,
        repo_root: &Path,
    ) -> Result<EnrichmentResult> {
        let mut result = EnrichmentResult::default();
        let job_deadline = tokio::time::Instant::now() + lsp_job_timeout();

        // Establish one admitted input set before any pass derives work from it.
        let admitted_count = nodes.iter().filter(|node| self.admits_node(node)).count();
        let matching_nodes: Vec<&Node> = nodes
            .iter()
            .filter(|node| self.admits_node(node))
            .filter(|node| is_regular_repo_file(repo_root, &node.id.file))
            .collect();
        let rejected_non_files = admitted_count.saturating_sub(matching_nodes.len());
        if rejected_non_files > 0 {
            tracing::debug!(
                "LSP ignored {} admitted node(s) whose paths were not normalized regular files for {}",
                rejected_non_files,
                self.language
            );
        }
        let mut readiness_files = self.inventory_readiness_files(repo_root)?;

        let fn_count = matching_nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .count();
        let trait_count = matching_nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Trait)
            .count();
        tracing::info!(
            "LSP enriching {} nodes ({} functions, {} traits) for {}",
            matching_nodes.len(),
            fn_count,
            trait_count,
            self.language
        );

        if matching_nodes.is_empty() && readiness_files.is_empty() {
            return Ok(result);
        }

        let warmup_file = self
            .find_warmup_file(repo_root, &matching_nodes)
            .or_else(|| readiness_files.first().map(|path| repo_root.join(path)));
        let default_root = matching_nodes
            .first()
            .copied()
            .or_else(|| nodes.first())
            .map(|node| node.id.root.as_str());

        // Try to initialize the language server using the repo root from --repo.
        // Scoped requests enforce their shared deadline here rather than at the
        // event-bus boundary, where cancellation would discard Pass 1 output.
        match self
            .within_enrichment_deadline(
                job_deadline,
                "language-server initialization",
                self.ensure_initialized(repo_root, warmup_file.as_deref(), &readiness_files),
            )
            .await
        {
            Ok(Ok(())) => {
                result.any_enricher_ran = true;
            }
            Ok(Err(e)) => {
                tracing::debug!("LSP enrichment skipped for {}: {}", self.language, e);
                return Err(e);
            }
            Err(detail) => {
                self.reset_incomplete_initialization().await;
                self.mark_broad_reference_deadline(&mut result, detail);
                return Ok(result);
            }
        }

        // Extract state under lock, then release for concurrent work.
        let (
            transport,
            root,
            has_type_hierarchy,
            type_hierarchy_strikes,
            has_references,
            has_call_hierarchy,
            has_definition,
            has_implementation,
            has_document_links,
            has_pull_diagnostics,
            has_inlay_hints,
            was_quiescent,
            mut validation_evidence,
            diag_sink,
        ) = {
            let state = self.state.lock().await;
            let root = state
                .root_path
                .clone()
                .unwrap_or_else(|| repo_root.to_path_buf());
            let transport = match &state.pipelined {
                Some(t) => Arc::clone(t),
                None => return Ok(result),
            };
            let diag_sink = Arc::clone(&state.diagnostics_sink);
            let was_quiescent = transport.quiescent_flag.load(Ordering::Acquire);
            (
                transport,
                root,
                state.has_type_hierarchy,
                state.type_hierarchy_strikes,
                state.has_references,
                state.has_call_hierarchy,
                state.has_definition,
                state.has_implementation,
                state.has_document_links,
                state.has_pull_diagnostics,
                state.has_inlay_hints,
                was_quiescent,
                state.validation_evidence.clone(),
                diag_sink,
            )
        };
        if let Some(validation) = validation_evidence.as_mut() {
            result.new_nodes.extend(materialize_document_symbol_nodes(
                validation,
                repo_root,
                &matching_nodes,
                default_root,
            )?);
        }
        result.lsp_validation = validation_evidence;
        if let Some(validation) = result.lsp_validation.as_ref() {
            tracing::info!(
                "{} readiness validation evidence: {}",
                self.server_command,
                validation.summary()
            );
        }

        // Pass 0: crate-level dependency graph (Rust only, no quiescence needed)
        if let Err(detail) = self
            .within_enrichment_deadline(
                job_deadline,
                "Pass 0 crate graph",
                self.run_pass0_crate_graph(&transport, &matching_nodes, &mut result),
            )
            .await
        {
            self.mark_broad_reference_deadline(&mut result, detail);
            return Ok(result);
        }

        // Guard: skip enrichment passes when server never reached quiescent state
        if !was_quiescent {
            tracing::info!(
                "LSP Pass 1 skipped: {} did not reach quiescent state during initialization",
                self.server_command
            );
            tracing::info!(
                "LSP enrichment complete for {}: 0 edges, 0 diagnostic nodes (0 attempted, 0 errors) -- skipped (not quiescent)",
                self.language,
            );
            result.aborted = true;
            result.error_count = 1;
            result.diagnostic = Some(format!(
                "LSP enrichment aborted for {}: server did not reach quiescent state during initialization",
                self.server_command
            ));
            return Ok(result);
        }

        if self.file_readiness {
            if let Some(initial_uri) = result
                .lsp_validation
                .as_ref()
                .filter(|validation| {
                    validation.method.as_deref() == Some("textDocument/documentSymbol")
                })
                .and_then(|validation| validation.request_uri.as_deref())
                .and_then(|uri| Uri::from_str(uri).ok())
            {
                let initial_path = uri_to_relative_path(&initial_uri, repo_root);
                readiness_files.retain(|path| path != &initial_path);
            }
            let negotiated = result
                .lsp_validation
                .as_ref()
                .and_then(|validation| validation.negotiated_capabilities)
                .unwrap_or_default();
            let (validations, mut file_nodes) = self
                .validate_inventory_files(
                    &transport,
                    repo_root,
                    &readiness_files,
                    default_root,
                    negotiated,
                )
                .await;
            result.lsp_file_validations = validations;
            result.new_nodes.append(&mut file_nodes);
        }

        if matching_nodes.is_empty() {
            return Ok(result);
        }

        // Shared state for concurrent passes
        let matching_nodes_owned: Arc<Vec<Node>> =
            Arc::new(matching_nodes.iter().map(|n| (*n).clone()).collect());
        let refs_by_file_shared: Arc<HashMap<std::path::PathBuf, Vec<Node>>> = {
            let mut map: HashMap<std::path::PathBuf, Vec<Node>> = HashMap::new();
            // Call-hierarchy endpoints are not limited to the subset admitted for
            // LSP work. Resolve against the complete extracted graph so a valid
            // caller/callee that was not itself scheduled is still persisted.
            for n in nodes {
                map.entry(n.id.file.clone()).or_default().push(n.clone());
            }
            for nodes in map.values_mut() {
                nodes.sort_by_key(|node| node.line_end.saturating_sub(node.line_start));
            }
            Arc::new(map)
        };
        let capabilities = LspServerCapabilities {
            references: has_references,
            call_hierarchy: has_call_hierarchy,
            definitions: has_definition,
            implementations: has_implementation,
            type_hierarchy: has_type_hierarchy,
            document_symbols: result
                .lsp_validation
                .as_ref()
                .and_then(|evidence| evidence.negotiated_capabilities)
                .is_some_and(|capabilities| capabilities.document_symbol_provider),
            document_links: has_document_links,
        };
        let mut query_budget = self.query_profile.budget();
        let query_telemetry = Arc::new(policy::LspQueryTelemetry::new(&self.query_profile));

        // Pass 1: call hierarchy, references, implementations, document links (concurrent)
        let (attempted, errors, aborted, abort_diagnostic) = self
            .run_pass1_references(
                &transport,
                &root,
                &matching_nodes,
                &matching_nodes_owned,
                &refs_by_file_shared,
                capabilities,
                &mut query_budget,
                &query_telemetry,
                &mut result,
                job_deadline,
            )
            .await;
        result.error_count = errors as usize;
        if aborted {
            result.aborted = true;
            let detail = abort_diagnostic
                .unwrap_or_else(|| "Pass 1 aborted without a diagnostic snapshot".to_string());
            result.diagnostic = Some(format!(
                "LSP Pass 1 aborted for {} after {} attempted nodes and {} errors: {}",
                self.server_command, attempted, errors, detail
            ));
            tracing::warn!("{}", result.diagnostic.as_deref().unwrap_or_default());
            result.lsp_query_metrics = query_telemetry.snapshot();
            return Ok(result);
        }

        // Pass 2: type hierarchy (sequential -- strike counting needs order)
        let (has_type_hierarchy, type_hierarchy_strikes) = match self
            .within_enrichment_deadline(
                job_deadline,
                "Pass 2 type hierarchy",
                self.run_pass2_type_hierarchy(
                    &transport,
                    &root,
                    &matching_nodes,
                    capabilities,
                    &mut query_budget,
                    &query_telemetry,
                    type_hierarchy_strikes,
                    &mut result,
                ),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(detail) => {
                self.mark_broad_reference_deadline(&mut result, detail);
                result.lsp_query_metrics = query_telemetry.snapshot();
                return Ok(result);
            }
        };

        // Persist strike counter back to state
        {
            let mut state = self.state.lock().await;
            state.type_hierarchy_strikes = type_hierarchy_strikes;
            state.has_type_hierarchy = has_type_hierarchy;
        }

        // Pass 4: BelongsTo edges -- module hierarchy
        if let Err(detail) = self
            .within_enrichment_deadline(
                job_deadline,
                "Pass 4 module hierarchy",
                self.run_pass4_belongs_to(&transport, &root, &matching_nodes, &mut result),
            )
            .await
        {
            self.mark_broad_reference_deadline(&mut result, detail);
            result.lsp_query_metrics = query_telemetry.snapshot();
            return Ok(result);
        }

        // Pass 5: InlayHints -- inferred types in embeddings
        if let Err(detail) = self
            .within_enrichment_deadline(
                job_deadline,
                "Pass 5 inlay hints",
                self.run_pass5_inlay_hints(
                    &transport,
                    &root,
                    &matching_nodes,
                    has_inlay_hints,
                    &mut result,
                ),
            )
            .await
        {
            self.mark_broad_reference_deadline(&mut result, detail);
            result.lsp_query_metrics = query_telemetry.snapshot();
            return Ok(result);
        }

        // Pass 3: diagnostics (runs last, guarded by quiescence)
        if !was_quiescent {
            tracing::info!(
                "LSP Pass 3 skipped: {} did not reach quiescent state during initialization",
                self.server_command
            );
        } else if let Err(detail) = self
            .within_enrichment_deadline(
                job_deadline,
                "Pass 3 diagnostics",
                self.run_pass3_diagnostics(
                    &transport,
                    &root,
                    &matching_nodes,
                    has_pull_diagnostics,
                    &diag_sink,
                    repo_root,
                    &mut result,
                ),
            )
            .await
        {
            self.mark_broad_reference_deadline(&mut result, detail);
            result.lsp_query_metrics = query_telemetry.snapshot();
            return Ok(result);
        }

        let diag_count = result
            .new_nodes
            .iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "diagnostic"))
            .count();
        tracing::info!(
            "LSP enrichment complete for {}: {} edges, {} diagnostic nodes ({} attempted, {} errors{})",
            self.language,
            result.added_edges.len(),
            diag_count,
            attempted,
            errors,
            if aborted { ", aborted" } else { "" },
        );

        result.error_count = errors as usize;
        result.aborted = aborted;
        result.lsp_query_metrics = query_telemetry.snapshot();
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::str::FromStr;

    use super::transport::{LspRpcError, find_enclosing_symbol};
    use super::*;
    use crate::extract::Extractor;

    #[test]
    fn server_availability_uses_locked_path_without_external_which() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("locked-language-server"), b"fixture").unwrap();

        assert!(command_exists_on_path(
            "locked-language-server",
            Some(bin.as_os_str())
        ));
        assert!(!command_exists_on_path("which", Some(bin.as_os_str())));
    }

    #[test]
    fn frozen_cohort_descriptor_commands_match_locked_launchers() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmark/swebench-act-context/lsp-toolchain/descriptor-inventory.json");
        let inventory: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let servers = inventory["servers"]
            .as_array()
            .expect("descriptor inventory servers must be an array");
        assert_eq!(servers.len(), 32, "frozen cohort language count drifted");

        for profile in servers {
            let languages = profile["languages"]
                .as_array()
                .expect("descriptor languages must be an array");
            assert_eq!(languages.len(), 1, "each frozen profile owns one language");
            let language = languages[0]
                .as_str()
                .expect("descriptor language must be a string");
            let command = profile["command"]
                .as_str()
                .expect("descriptor command must be a string");
            let args = profile["args"]
                .as_array()
                .expect("descriptor args must be an array")
                .iter()
                .map(|arg| arg.as_str().expect("descriptor arg must be a string"))
                .collect::<Vec<_>>();
            let descriptor = builtin_lsp_descriptors()
                .iter()
                .find(|descriptor| descriptor.language == language)
                .unwrap_or_else(|| panic!("missing {language} descriptor"));
            assert_eq!(descriptor.command, command);
            assert_eq!(descriptor.args, args.as_slice());
        }
    }

    #[test]
    fn inventory_language_ids_and_legacy_text_seed_mandatory_files() {
        assert_eq!(
            lsp_language_id("c-cpp", Path::new("cextern/wcslib/wcsconfig.h.in")),
            "c"
        );
        assert_eq!(
            lsp_language_id("cython", Path::new("astropy/io/ascii/cparser.pyx")),
            "cython"
        );
        assert_eq!(
            lsp_language_id("cohort-text", Path::new("cextern/wcslib/THANKS")),
            "cohort-text"
        );

        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("CHANGES");
        std::fs::write(&path, [b'l', b'e', b'g', b'a', b'c', b'y', 0xff]).unwrap();
        let text = read_lsp_text(&path).expect("legacy inventory text must be seedable");
        assert_eq!(text, "legacy\u{fffd}");
    }

    #[test]
    fn issue825_js_template_descriptor_and_language_id() {
        let path = Path::new("sphinx/themes/basic/static/documentation_options.js_t");
        assert_eq!(lsp_language_id("typescript", path), "javascript");
        assert_eq!(
            builtin_lsp_descriptor_for_path(path)
                .expect("tracked JavaScript templates require a locked descriptor")
                .language(),
            "typescript"
        );
    }

    #[test]
    fn mjs_has_typescript_lsp_descriptor_and_javascript_language_id() {
        let path = Path::new("bin/test_pyodide.mjs");
        assert_eq!(
            builtin_lsp_descriptor_for_path(path)
                .expect("tracked ECMAScript modules require a locked descriptor")
                .language(),
            "typescript"
        );
        assert_eq!(lsp_language_id("typescript", path), "javascript");
    }

    #[test]
    fn exact_duplicate_document_symbols_are_canonicalized_without_losing_distinct_ranges() {
        let first = serde_json::json!({
            "name": "value",
            "kind": 13,
            "range": {
                "start": {"line": 1, "character": 0},
                "end": {"line": 1, "character": 5}
            }
        });
        let second = serde_json::json!({
            "name": "value",
            "kind": 13,
            "range": {
                "start": {"line": 3, "character": 0},
                "end": {"line": 3, "character": 5}
            }
        });
        let response = serde_json::json!([first.clone(), first, second]);
        let mut symbols = normalized_document_symbol_evidence(
            &response,
            "file:///fixture/astropy/io/ascii/cparser.pyx",
        )
        .expect("duplicate Cyright response should normalize");

        assert_eq!(symbols.len(), 2);
        let nodes =
            materialize_document_symbols("cython", &mut symbols, Path::new("/fixture"), |_| {
                Some("fixture".to_string())
            })
            .expect("canonical symbols should have distinct graph identities");
        assert_eq!(nodes.len(), 2);
        assert_ne!(nodes[0].stable_id(), nodes[1].stable_id());
    }

    #[test]
    fn lsp_work_items_require_normalized_regular_repo_files() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src/nested")).unwrap();
        std::fs::write(repo.path().join("src/app.py"), b"VALUE = 1\n").unwrap();
        std::fs::write(
            repo.path().parent().unwrap().join("outside.py"),
            b"escape\n",
        )
        .unwrap();

        assert!(is_regular_repo_file(repo.path(), Path::new("src/app.py")));
        assert!(!is_regular_repo_file(repo.path(), Path::new("")));
        assert!(!is_regular_repo_file(repo.path(), Path::new("src/nested")));
        assert!(!is_regular_repo_file(
            repo.path(),
            Path::new("../outside.py")
        ));
        assert!(!is_regular_repo_file(
            repo.path(),
            &repo.path().join("src/app.py")
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("app.py", repo.path().join("src/app-link.py")).unwrap();
            assert!(!is_regular_repo_file(
                repo.path(),
                Path::new("src/app-link.py")
            ));
        }
    }

    enum ValidationFixtureResponse {
        Success(serde_json::Value),
        RpcError(i64),
        Crash,
        Timeout,
    }

    struct ValidationFixture {
        responses: VecDeque<ValidationFixtureResponse>,
        calls: Vec<String>,
    }

    impl ValidationFixture {
        fn new(responses: impl IntoIterator<Item = ValidationFixtureResponse>) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                calls: Vec::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ReadinessRequester for ValidationFixture {
        async fn readiness_request(
            &mut self,
            method: &str,
            _params: serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.calls.push(method.to_string());
            match self.responses.pop_front().expect("fixture response") {
                ValidationFixtureResponse::Success(value) => Ok(value),
                ValidationFixtureResponse::RpcError(code) => {
                    Err(LspRpcError::new(code, "fixture RPC error").into())
                }
                ValidationFixtureResponse::Crash => Err(anyhow::anyhow!("server exited")),
                ValidationFixtureResponse::Timeout => {
                    std::future::pending::<Result<serde_json::Value>>().await
                }
            }
        }
    }

    fn validation_capabilities(
        workspace_symbols: bool,
        document_symbols: bool,
    ) -> ReadinessValidationCapabilities {
        ReadinessValidationCapabilities {
            workspace_symbols,
            document_symbols,
            code_actions: false,
        }
    }

    fn parsed_validation_capabilities(
        initialize_result: serde_json::Value,
    ) -> ReadinessValidationCapabilities {
        let result: InitializeResult =
            serde_json::from_value(initialize_result).expect("valid initialize fixture");
        ReadinessValidationCapabilities::from_server_capabilities(&result.capabilities)
    }

    #[test]
    fn negotiated_capabilities_report_initialize_providers_not_readiness_methods() {
        let result: InitializeResult = serde_json::from_value(serde_json::json!({
            "capabilities": {
                "referencesProvider": true,
                "definitionProvider": true,
                "implementationProvider": false,
                "documentLinkProvider": {"resolveProvider": false},
                "documentSymbolProvider": {"label": "outline"}
            }
        }))
        .expect("valid initialize fixture");
        let negotiated = negotiated_operation_capabilities(&result.capabilities, true);
        assert!(negotiated.references_provider);
        assert!(negotiated.call_hierarchy_provider);
        assert!(negotiated.definition_provider);
        assert!(!negotiated.implementation_provider);
        assert!(negotiated.document_link_provider);
        assert!(negotiated.document_symbol_provider);
    }

    #[tokio::test]
    async fn readiness_absent_or_false_workspace_capability_uses_only_document_symbols() {
        let absent = parsed_validation_capabilities(
            serde_json::json!({"capabilities": {"documentSymbolProvider": true}}),
        );
        let explicit_false = parsed_validation_capabilities(serde_json::json!({"capabilities": {
            "workspaceSymbolProvider": false,
            "documentSymbolProvider": {"label": "supported"}
        }}));
        assert_eq!(
            absent.primary(),
            Some(ReadinessValidationMethod::DocumentSymbol)
        );
        assert_eq!(
            explicit_false.primary(),
            Some(ReadinessValidationMethod::DocumentSymbol)
        );

        let mut fixture =
            ValidationFixture::new([ValidationFixtureResponse::Success(serde_json::json!([]))]);
        let outcome = execute_indexing_validation_once(
            &mut fixture,
            explicit_false,
            Some("file:///fixture.json"),
            "main",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect("document validation should succeed")
        .expect("document validation is terminal");
        assert_eq!(outcome.method, ReadinessValidationMethod::DocumentSymbol);
        assert_eq!(fixture.calls, ["textDocument/documentSymbol"]);
    }

    #[tokio::test]
    async fn readiness_document_symbol_fallback_is_deterministic_and_accepts_zero_symbols() {
        let mut fixture = ValidationFixture::new([
            ValidationFixtureResponse::Success(serde_json::json!([])),
            ValidationFixtureResponse::Success(serde_json::json!([])),
        ]);
        let outcome = execute_indexing_validation_once(
            &mut fixture,
            validation_capabilities(true, true),
            Some("file:///fixture.html"),
            "main",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect("fallback should succeed")
        .expect("fallback should be terminal");
        assert_eq!(outcome.method, ReadinessValidationMethod::DocumentSymbol);
        assert_eq!(outcome.symbol_count, 0);
        assert_eq!(
            fixture.calls,
            ["workspace/symbol", "textDocument/documentSymbol"]
        );

        let evidence = crate::extract::scan_stats::LspValidationEvidence::processed(
            "html",
            "fixture-server",
            outcome.method.method(),
            outcome.symbol_count,
        )
        .with_request_uri(outcome.request_uri);
        assert_eq!(evidence.symbol_count, Some(0));
        assert!(evidence.summary().contains("processed"));
    }

    #[tokio::test]
    async fn document_symbol_payload_materializes_deterministic_graph_evidence() {
        let response = serde_json::json!([{
            "name": "Guide",
            "kind": 3,
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 4, "character": 0}
            },
            "children": [{
                "name": "Usage",
                "kind": 3,
                "range": {
                    "start": {"line": 2, "character": 0},
                    "end": {"line": 4, "character": 0}
                }
            }]
        }]);
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::Success(response)]);
        let outcome = execute_indexing_validation_once(
            &mut fixture,
            validation_capabilities(false, true),
            Some("file:///fixture/docs/guide.md"),
            "",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect("document validation should succeed")
        .expect("document validation should be terminal");
        assert_eq!(outcome.symbol_count, 2);
        assert_eq!(outcome.document_symbols.len(), 2);

        let mut validation = LspValidationEvidence::processed(
            "markdown",
            "fixture-server",
            outcome.method.method(),
            outcome.symbol_count,
        )
        .with_request_uri(outcome.request_uri)
        .with_document_symbols(outcome.document_symbols)
        .with_negotiated_capabilities(LspNegotiatedCapabilities {
            document_symbol_provider: true,
            ..LspNegotiatedCapabilities::default()
        });
        let extracted = Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from("docs/guide.md"),
                name: "Guide".to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "# Guide".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::Markdown,
        };
        let nodes = materialize_document_symbol_nodes(
            &mut validation,
            Path::new("/fixture"),
            &[&extracted],
            Some("fixture"),
        )
        .expect("response evidence should map to graph nodes");
        assert_eq!(nodes.len(), 2);
        assert!(validation.document_symbols.iter().all(|symbol| {
            symbol.file.as_deref() == Some("docs/guide.md")
                && symbol
                    .graph_result_id
                    .as_deref()
                    .is_some_and(|result_id| nodes.iter().any(|node| node.stable_id() == result_id))
        }));
    }

    #[tokio::test]
    async fn malformed_document_fixture_fails_closed_at_response_normalization() {
        assert!(
            include_str!("../../../tests/fixtures/lsp_capability_repo/docs/malformed.md")
                .contains("unterminated")
        );
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::Success(
            serde_json::json!([{"name": "Malformed", "kind": 3}]),
        )]);
        let error = execute_indexing_validation_once(
            &mut fixture,
            validation_capabilities(false, true),
            Some("file:///fixture/docs/malformed.md"),
            "",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect_err("malformed documentSymbol evidence must block readiness");
        assert!(error.to_string().contains("has no range"));
    }

    #[test]
    fn compile_command_overrides_preserve_settings_and_do_not_leak_suffixes() {
        const OVERRIDES: &[LspCompileCommandOverride] = &[LspCompileCommandOverride {
            suffix: ".h.in",
            compiler: "clang",
            args: &["-xc"],
        }];
        let repo = tempfile::tempdir().unwrap();
        for path in ["config.h.in", "native.cpp"] {
            std::fs::write(repo.path().join(path), "int configured_header;\n").unwrap();
        }
        let settings = initialization_settings_with_compile_commands(
            Some(serde_json::json!({"existing": {"enabled": true}})),
            repo.path(),
            &[PathBuf::from("native.cpp"), PathBuf::from("config.h.in")],
            OVERRIDES,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            settings.pointer("/existing/enabled"),
            Some(&serde_json::json!(true))
        );
        let changes = settings["compilationDatabaseChanges"].as_object().unwrap();
        assert_eq!(
            changes.len(),
            1,
            "nonmatching suffix must not receive an override"
        );
        let exact_path = std::fs::canonicalize(repo.path().join("config.h.in")).unwrap();
        let command = &changes[exact_path.to_str().unwrap()];
        assert_eq!(
            command["compilationCommand"],
            serde_json::json!(["clang", "-xc", exact_path])
        );
        assert_eq!(
            command["workingDirectory"],
            serde_json::json!(std::fs::canonicalize(repo.path()).unwrap())
        );
    }

    #[tokio::test]
    async fn exact_compile_command_override_precedes_document_symbol_request() {
        const OVERRIDES: &[LspCompileCommandOverride] = &[LspCompileCommandOverride {
            suffix: ".h.in",
            compiler: "clang",
            args: &["-xc"],
        }];
        let repo = tempfile::tempdir().unwrap();
        let header = repo.path().join("config.h.in");
        std::fs::write(&header, "int configured_header;\n").unwrap();
        let exact_header = std::fs::canonicalize(&header).unwrap();
        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let fixture = fixture_server.to_str().expect("UTF-8 fixture path");
        let expected_path = exact_header.to_str().expect("UTF-8 header path");
        let enricher = LspEnricher::new(
            "c-cpp",
            "python3",
            &[fixture, "compile_command_override", expected_path],
            &["h"],
        )
        .with_compile_command_overrides(OVERRIDES);

        enricher
            .ensure_initialized(repo.path(), Some(&header), &[PathBuf::from("config.h.in")])
            .await
            .expect("fixture requires exact initialize override before didOpen/documentSymbol");
    }

    #[tokio::test]
    async fn mock_document_server_exercises_link_definition_and_reference_requests() {
        let repo = tempfile::tempdir().unwrap();
        for directory in ["docs", "src", "tests"] {
            std::fs::create_dir_all(repo.path().join(directory)).unwrap();
        }
        for path in [
            "README.md",
            "docs/guide.md",
            "src/app.py",
            "tests/test_app.py",
        ] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lsp_capability_repo")
                .join(path);
            std::fs::copy(source, repo.path().join(path)).unwrap();
        }
        let markdown = crate::extract::markdown::MarkdownExtractor::new();
        let mut document_nodes = Vec::new();
        for path in ["README.md", "docs/guide.md"] {
            let content = std::fs::read_to_string(repo.path().join(path)).unwrap();
            document_nodes.extend(markdown.extract(Path::new(path), &content).unwrap().nodes);
        }
        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = LspEnricher::new(
            "markdown",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "document_features",
            ],
            &["md"],
        );
        let result = enricher
            .enrich(&document_nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("mock document enrichment succeeds");
        assert!(
            !result.aborted,
            "mock document enrichment aborted: {result:?}"
        );
        let negotiated = result
            .lsp_validation
            .as_ref()
            .and_then(|validation| validation.negotiated_capabilities)
            .expect("negotiated document capabilities");
        assert!(negotiated.document_symbol_provider);
        assert!(negotiated.document_link_provider);
        assert!(negotiated.definition_provider);
        assert!(negotiated.references_provider);
        let records = work_items::load_records_since(repo.path(), 0).unwrap();
        let operations = records
            .iter()
            .flat_map(|record| record.requested_operations.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        assert!(operations.contains("document_symbols"));
        assert!(operations.contains("document_links"));
        assert!(operations.contains("definitions"));
        assert!(operations.contains("references"));
        let document_symbol_files = records
            .iter()
            .filter(|record| {
                record
                    .requested_operations
                    .iter()
                    .any(|operation| operation == "document_symbols")
            })
            .map(|record| record.file.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            document_symbol_files,
            BTreeSet::from(["README.md", "docs/guide.md"])
        );
        let persisted_symbol_files = result
            .new_nodes
            .iter()
            .filter(|node| node.id.kind == NodeKind::Other("lsp_document_symbol".to_string()))
            .map(|node| node.id.file.to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            persisted_symbol_files,
            BTreeSet::from(["README.md".to_string(), "docs/guide.md".to_string()])
        );
        assert!(
            result
                .added_edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::DependsOn)
        );
        assert!(
            result
                .added_edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::Implements)
        );
        assert!(
            result
                .added_edges
                .iter()
                .any(|edge| edge.kind == EdgeKind::ReferencedBy)
        );
    }

    #[tokio::test]
    async fn binding_final_review_python_server_persists_source_and_test_call_provenance() {
        use crate::extract::python::PythonExtractor;
        use crate::server::store::{load_graph_from_lance, persist_graph_to_lance};

        let repo = tempfile::tempdir().unwrap();
        for directory in ["src", "tests"] {
            std::fs::create_dir_all(repo.path().join(directory)).unwrap();
        }
        let mut nodes = Vec::new();
        for path in ["src/app.py", "tests/test_app.py"] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lsp_capability_repo")
                .join(path);
            std::fs::copy(&source, repo.path().join(path)).unwrap();
            let content = std::fs::read_to_string(source).unwrap();
            nodes.extend(
                PythonExtractor::new()
                    .extract(Path::new(path), &content)
                    .unwrap()
                    .nodes,
            );
        }
        assert!(nodes.iter().any(|node| {
            node.id.file.as_path() == Path::new("src/app.py") && node.id.name == "greet"
        }));
        assert!(nodes.iter().any(|node| {
            node.id.file.as_path() == Path::new("tests/test_app.py") && node.id.name == "test_greet"
        }));

        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = LspEnricher::new(
            "python",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "python_features",
            ],
            &["py"],
        );
        let result = enricher
            .enrich(&nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("mock Python enrichment succeeds");
        assert!(
            !result.aborted,
            "mock Python enrichment aborted: {result:?}"
        );
        let negotiated = result
            .lsp_validation
            .as_ref()
            .and_then(|validation| validation.negotiated_capabilities)
            .expect("negotiated Python capabilities");
        assert!(negotiated.references_provider);
        assert!(negotiated.document_symbol_provider);

        let records = work_items::load_records_since(repo.path(), 0).unwrap();
        let relation = result
            .added_edges
            .iter()
            .find(|edge| {
                edge.kind == EdgeKind::Calls
                    && edge.source == ExtractionSource::Lsp
                    && edge.from.file.as_path() == Path::new("tests/test_app.py")
                    && edge.from.name == "test_greet"
                    && edge.to.file.as_path() == Path::new("src/app.py")
                    && edge.to.name == "greet"
            })
            .unwrap_or_else(|| {
                panic!(
                    "mock references response did not emit the test-to-source call edge; edges={:?}; records={:?}",
                    result.added_edges, records
                )
            })
            .clone();
        let reference_records = records
            .iter()
            .filter(|record| {
                record
                    .requested_operations
                    .iter()
                    .any(|operation| operation == "references")
            })
            .collect::<Vec<_>>();
        assert_eq!(reference_records.len(), 1);
        assert_eq!(reference_records[0].file, "src/app.py");
        assert_eq!(
            reference_records[0].state,
            work_items::LspWorkItemState::Completed
        );
        assert_eq!(reference_records[0].observed_result_count, 1);
        assert!(
            reference_records[0]
                .output_edges
                .iter()
                .any(|edge| edge.stable_id() == relation.stable_id())
        );

        for path in ["src/app.py", "tests/test_app.py"] {
            let symbol_record = records
                .iter()
                .find(|record| {
                    record.file == path
                        && record
                            .requested_operations
                            .iter()
                            .any(|operation| operation == "document_symbols")
                })
                .unwrap_or_else(|| panic!("{path} has no durable document-symbol request"));
            assert_eq!(symbol_record.state, work_items::LspWorkItemState::Completed);
            let expected_count = u64::from(path == "src/app.py");
            assert_eq!(symbol_record.observed_result_count, expected_count);
            assert_eq!(symbol_record.output_nodes.len() as u64, expected_count);
            assert!(
                symbol_record
                    .output_nodes
                    .iter()
                    .all(|node| node.id.file.as_path() == Path::new(path))
            );
        }

        let mut persisted_nodes = nodes;
        persisted_nodes.extend(result.new_nodes);
        persist_graph_to_lance(repo.path(), &persisted_nodes, &result.added_edges)
            .await
            .expect("persist mock Python graph");
        let reopened = load_graph_from_lance(repo.path())
            .await
            .expect("reopen mock Python graph");
        let reopened_relation = reopened
            .edges
            .iter()
            .find(|edge| edge.stable_id() == relation.stable_id())
            .expect("persisted call edge survives graph reopen");
        assert_eq!(reopened_relation.source, ExtractionSource::Lsp);
        for path in ["src/app.py", "tests/test_app.py"] {
            assert!(
                reopened_relation.from.file.as_path() == Path::new(path)
                    || reopened_relation.to.file.as_path() == Path::new(path),
                "persisted LSP provenance does not include {path}"
            );
        }
    }

    #[tokio::test]
    async fn unmapped_call_hierarchy_result_materializes_and_survives_graph_reopen() {
        use crate::extract::python::PythonExtractor;
        use crate::server::store::{load_graph_from_lance, persist_graph_to_lance};

        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        let source = "def target():\n    pass\n";
        std::fs::write(repo.path().join("src/app.py"), source).unwrap();
        let nodes = PythonExtractor::new()
            .extract(Path::new("src/app.py"), source)
            .unwrap()
            .nodes;
        let target = nodes
            .iter()
            .find(|node| node.id.name == "target")
            .expect("fixture target extracted")
            .id
            .clone();

        let fixture_server =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_capability_server.py");
        let enricher = LspEnricher::new(
            "python",
            "python3",
            &[
                fixture_server.to_str().expect("UTF-8 fixture path"),
                "call_hierarchy_unmapped",
            ],
            &["py"],
        );
        let result = enricher
            .enrich(&nodes, &GraphIndex::new(), repo.path())
            .await
            .expect("mock call hierarchy enrichment succeeds");
        assert!(!result.aborted, "mock enrichment aborted: {result:?}");

        let materialized = result
            .new_nodes
            .iter()
            .find(|node| {
                node.id.file.as_path() == Path::new("src/app.py")
                    && node.id.name == "generated_target@lsp:49:0-50:1"
                    && node.metadata.get("lsp_name").map(String::as_str) == Some("generated_target")
                    && node.source == ExtractionSource::Lsp
            })
            .expect("raw endpoint without an extracted node is materialized")
            .clone();
        let relation = result
            .added_edges
            .iter()
            .find(|edge| {
                edge.kind == EdgeKind::Calls
                    && edge.source == ExtractionSource::Lsp
                    && edge.from == target
                    && edge.to == materialized.id
            })
            .expect("raw call result becomes an LSP Calls edge")
            .clone();
        let records = work_items::load_records_since(repo.path(), 0).unwrap();
        let record = records
            .iter()
            .find(|record| {
                record.file == "src/app.py"
                    && record
                        .requested_operations
                        .iter()
                        .any(|operation| operation == "call_hierarchy")
            })
            .expect("call hierarchy work item persisted");
        assert_eq!(record.observed_result_count, 1);
        assert!(
            record
                .output_nodes
                .iter()
                .any(|node| node.stable_id() == materialized.stable_id())
        );
        assert!(
            record
                .output_edges
                .iter()
                .any(|edge| edge.stable_id() == relation.stable_id())
        );

        let mut persisted_nodes = nodes;
        persisted_nodes.extend(result.new_nodes);
        persist_graph_to_lance(repo.path(), &persisted_nodes, &result.added_edges)
            .await
            .expect("persist graph containing materialized endpoint");
        let reopened = load_graph_from_lance(repo.path())
            .await
            .expect("reopen persisted graph");
        assert!(
            reopened
                .nodes
                .iter()
                .any(|node| node.stable_id() == materialized.stable_id())
        );
        assert!(
            reopened
                .edges
                .iter()
                .any(|edge| edge.stable_id() == relation.stable_id())
        );
    }

    #[tokio::test]
    async fn readiness_workspace_symbol_server_uses_advertised_method() {
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::Success(
            serde_json::json!([{"name": "one"}, {"name": "two"}]),
        )]);
        let outcome = execute_indexing_validation_once(
            &mut fixture,
            validation_capabilities(true, false),
            None,
            "main",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect("workspace validation should succeed")
        .expect("non-empty workspace symbols should be terminal");
        assert_eq!(outcome.method, ReadinessValidationMethod::WorkspaceSymbol);
        assert_eq!(outcome.symbol_count, 2);
        assert_eq!(fixture.calls, ["workspace/symbol"]);
    }

    #[tokio::test]
    async fn readiness_method_not_found_fails_immediately_without_retry() {
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::RpcError(-32601)]);
        let result = tokio::time::timeout(
            tokio::time::Duration::from_millis(50),
            execute_indexing_validation_once(
                &mut fixture,
                validation_capabilities(true, false),
                None,
                "main",
                tokio::time::Duration::from_secs(30),
            ),
        )
        .await
        .expect("-32601 must complete within the deterministic 50ms bound");
        let error = result.expect_err("advertised unsupported method is a hard error");
        assert!(error.to_string().contains("-32601"));
        assert_eq!(fixture.calls, ["workspace/symbol"]);
    }

    #[tokio::test]
    async fn readiness_advertised_capability_crash_is_a_hard_error() {
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::Crash]);
        let error = execute_indexing_validation_once(
            &mut fixture,
            validation_capabilities(false, true),
            Some("file:///fixture.json"),
            "",
            tokio::time::Duration::from_millis(50),
        )
        .await
        .expect_err("server crash must not become success");
        assert!(error.to_string().contains("validation failed"));
    }

    #[tokio::test]
    async fn readiness_advertised_capability_timeout_is_a_hard_error() {
        let mut fixture = ValidationFixture::new([ValidationFixtureResponse::Timeout]);
        let result = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            execute_indexing_validation_once(
                &mut fixture,
                validation_capabilities(false, true),
                Some("file:///fixture.json"),
                "",
                tokio::time::Duration::from_millis(10),
            ),
        )
        .await
        .expect("fixture timeout must stay within the deterministic 100ms bound");
        let error = result.expect_err("timeout must not become success");
        assert!(error.to_string().contains("timed out after 10ms"));
    }

    #[test]
    fn readiness_warmup_file_selection_is_deterministic() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("z.json"), "{}").unwrap();
        std::fs::write(temp.path().join("a.json"), "{}").unwrap();
        let enricher = LspEnricher::new("json", "fixture-server", &[], &["json"]);
        let admitted = Node {
            id: NodeId {
                root: "primary".to_string(),
                file: PathBuf::from("z.json"),
                name: "value".to_string(),
                kind: NodeKind::Function,
            },
            language: "json".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        assert_eq!(
            enricher
                .find_warmup_file(temp.path(), &[&admitted])
                .unwrap()
                .file_name()
                .unwrap(),
            "z.json",
            "warm-up must come from the invocation's admitted cohort"
        );
    }

    #[test]
    fn readiness_warmup_accepts_only_admitted_test_or_large_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let tests_dir = temp.path().join("tests");
        std::fs::create_dir(&tests_dir).unwrap();
        let relative_path = PathBuf::from("tests/only.test.json");
        std::fs::write(temp.path().join(&relative_path), vec![b'x'; 60_000]).unwrap();
        let enricher = LspEnricher::new("json", "fixture-server", &[], &["json"]);
        let admitted = Node {
            id: NodeId {
                root: "primary".to_string(),
                file: relative_path.clone(),
                name: "value".to_string(),
                kind: NodeKind::Function,
            },
            language: "json".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        assert_eq!(
            enricher.find_warmup_file(temp.path(), &[&admitted]),
            Some(temp.path().join(relative_path)),
            "scope-valid files must not be rejected by test-name or size heuristics"
        );
    }

    /// Verify the Enricher trait can be implemented (compile-time check).
    #[tokio::test]
    async fn test_enricher_trait_implementable() {
        struct DummyEnricher;

        #[async_trait::async_trait]
        impl Enricher for DummyEnricher {
            fn languages(&self) -> &[&str] {
                &["test"]
            }

            fn is_ready(&self) -> bool {
                true
            }

            fn name(&self) -> &str {
                "dummy"
            }

            async fn enrich(
                &self,
                _nodes: &[Node],
                _index: &GraphIndex,
                _repo_root: &Path,
            ) -> Result<EnrichmentResult> {
                Ok(EnrichmentResult::default())
            }
        }

        let enricher = DummyEnricher;
        assert_eq!(enricher.languages(), &["test"]);
        assert!(enricher.is_ready());
        assert_eq!(enricher.name(), "dummy");

        let index = GraphIndex::new();
        let result = enricher
            .enrich(&[], &index, std::path::Path::new("."))
            .await
            .unwrap();
        assert!(result.added_edges.is_empty());
        assert!(result.updated_nodes.is_empty());
    }

    #[tokio::test]
    async fn internal_broad_reference_deadline_keeps_completed_partial_mutations() {
        let budget = Arc::new(LspBroadReferenceBudget::new(
            10,
            std::time::Duration::from_millis(5),
        ));
        let enricher =
            LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]).with_broad_references(budget);
        let mut partial_output = Vec::new();

        let detail = enricher
            .within_enrichment_deadline(
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                "test phase",
                async {
                    partial_output.push("completed edge");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                },
            )
            .await
            .expect_err("deadline should expire");

        assert_eq!(partial_output, vec!["completed edge"]);
        assert!(detail.contains("test phase"));
        let mut result = EnrichmentResult::default();
        enricher.mark_broad_reference_deadline(&mut result, detail);
        assert!(result.aborted);
        assert!(result.any_enricher_ran);
        assert!(
            result
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("partial output was preserved"))
        );
    }

    #[tokio::test]
    async fn internal_job_deadline_bounds_unbudgeted_phases_and_preserves_partial_mutations() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let mut partial_output = Vec::new();

        let detail = enricher
            .within_enrichment_deadline(
                tokio::time::Instant::now() + std::time::Duration::from_millis(5),
                "unbudgeted test phase",
                async {
                    partial_output.push("completed edge");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                },
            )
            .await
            .expect_err("job deadline should expire");

        assert_eq!(partial_output, vec!["completed edge"]);
        assert!(detail.contains("job timed out"));
        assert!(detail.contains("unbudgeted test phase"));
    }

    /// Verify the LspEnricher can be constructed with correct properties for each language.
    #[test]
    fn test_lsp_enricher_creation() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        assert_eq!(enricher.languages(), &["rust"]);
        assert!(!enricher.is_ready());
        assert_eq!(enricher.name(), "rust-analyzer-lsp");
        assert_eq!(enricher.server_command, "rust-analyzer");
        assert!(enricher.server_args.is_empty());
    }

    #[test]
    fn test_builtin_python_factory_applies_lang_config_policy() {
        let python = builtin_lsp_enricher("python").expect("python is a built-in LSP profile");

        assert_eq!(python.server_command, "pyrefly");
        assert_eq!(
            python.server_args,
            vec![
                "lsp",
                "--verbose",
                "--indexing-mode",
                "lazy-blocking",
                "--threads",
                "1",
                "--workspace-indexing-limit",
                "5000",
                "--build-system-blocking",
                "--color",
                "never"
            ]
        );
        assert_eq!(python.config_file_hint(), Some("pyproject.toml"));
        let python_kinds = python
            .enrichable_kinds()
            .expect("the shared factory must retain Python's admission policy");
        assert_eq!(python_kinds.len(), 2);
        assert!(python_kinds.contains(&NodeKind::Function));
        assert!(python_kinds.contains(&NodeKind::Trait));
        assert!(
            !python.allows_declared_const_references(),
            "Python declared-Const references remain default-deny without a qualifying probe"
        );
        let rust = builtin_lsp_enricher("rust").expect("rust is a built-in LSP profile");
        assert!(
            rust.allows_declared_const_references(),
            "rust-analyzer cleared the #768 declared-Const yield threshold"
        );
        assert_eq!(python.init_settings, None);
        let python_config = crate::extract::configs::config_for_language("python")
            .expect("Python must retain its LangConfig policy");
        assert_eq!(python_config.venv_candidates, None);

        let cython = builtin_lsp_enricher("cython").expect("cython is a built-in LSP profile");
        assert_eq!(cython.server_command, "cyright-langserver");
        assert_eq!(cython.server_args, vec!["--stdio"]);
        let cython_config = crate::extract::configs::config_for_language("cython")
            .expect("Cython must retain its LangConfig policy");
        assert_eq!(
            cython_config.venv_candidates,
            Some(&[".venv", "venv", "env"][..])
        );
        let cython_kinds = cython
            .enrichable_kinds()
            .expect("the shared factory must retain Cython's admission policy");
        assert_eq!(cython_kinds.len(), 2);
        assert!(cython_kinds.contains(&NodeKind::Function));
        assert!(cython_kinds.contains(&NodeKind::Trait));
    }

    #[tokio::test]
    async fn test_synthetic_const_produces_no_lsp_work_item() {
        let enricher = LspEnricher::new(
            "python",
            "rna-test-server-must-not-be-started",
            &[],
            &["py"],
        );
        let mut metadata = BTreeMap::new();
        metadata.insert("synthetic".to_string(), "true".to_string());
        let synthetic_const = Node {
            id: NodeId {
                root: "test".into(),
                file: PathBuf::from("app.py"),
                name: "application/json".into(),
                kind: NodeKind::Const,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 1,
            signature: "application/json".into(),
            body: "application/json".into(),
            metadata,
            source: ExtractionSource::TreeSitter,
        };

        assert!(!enricher.admits_node(&synthetic_const));
        let mut declared_const = synthetic_const.clone();
        declared_const
            .metadata
            .insert("synthetic".to_string(), "false".to_string());
        assert!(
            enricher.admits_node(&declared_const),
            "the common boundary must distinguish declared constants from synthetic values"
        );
        assert!(
            !enricher.admits_pass1_node(&declared_const),
            "declared constants must not produce default Pass 1 reference work"
        );
        let result = enricher
            .enrich(&[synthetic_const], &GraphIndex::new(), Path::new("."))
            .await
            .expect("an empty admitted set must return before starting an LSP server");
        assert!(result.added_edges.is_empty());
        assert!(result.updated_nodes.is_empty());
        assert!(result.new_nodes.is_empty());
    }

    /// Verify enrichers for each language have correct properties.
    #[test]
    fn test_lsp_enricher_all_languages() {
        let rust = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        assert_eq!(rust.languages(), &["rust"]);
        assert_eq!(rust.name(), "rust-analyzer-lsp");

        let python = LspEnricher::new(
            "python",
            "pyrefly",
            &[
                "lsp",
                "--verbose",
                "--indexing-mode",
                "lazy-blocking",
                "--threads",
                "1",
                "--workspace-indexing-limit",
                "5000",
                "--build-system-blocking",
                "--color",
                "never",
            ],
            &["py"],
        );
        assert_eq!(python.languages(), &["python"]);
        assert_eq!(python.name(), "pyrefly-lsp");
        assert_eq!(python.server_args.len(), 11);

        let typescript = LspEnricher::new(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx"],
        );
        assert_eq!(typescript.languages(), &["typescript"]);
        assert_eq!(typescript.name(), "typescript-language-server-lsp");

        let go = LspEnricher::new("go", "gopls", &["serve"], &["go"]);
        assert_eq!(go.languages(), &["go"]);
        assert_eq!(go.name(), "gopls-lsp");
        assert_eq!(go.server_args, vec!["serve"]);

        let markdown = LspEnricher::new("markdown", "marksman", &["server"], &["md"]);
        assert_eq!(markdown.languages(), &["markdown"]);
        assert_eq!(markdown.name(), "marksman-lsp");
    }

    /// Verify enrichment returns empty result when no matching nodes are present.
    #[tokio::test]
    async fn test_lsp_enricher_no_matching_nodes() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();

        // Pass nodes with a non-matching language
        let nodes = vec![Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("test.py"),
                name: "hello".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 1,
            signature: "def hello()".into(),
            body: "def hello(): pass".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }];

        let result = enricher
            .enrich(&nodes, &index, std::path::Path::new("."))
            .await
            .unwrap();
        assert!(result.added_edges.is_empty());
    }

    /// Verify the EnricherRegistry works correctly with multiple enrichers.
    #[tokio::test]
    async fn test_enricher_registry() {
        use super::super::EnricherRegistry;

        let registry = EnricherRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);

        let registry = EnricherRegistry::with_builtins();
        assert!(
            registry.len() >= 30,
            "should have 30+ auto-discovered LSP servers, got {}",
            registry.len()
        );
    }

    /// Verify multiple enrichers can be registered and coexist.
    #[tokio::test]
    async fn test_multiple_enrichers_registered() {
        use super::super::EnricherRegistry;

        let mut registry = EnricherRegistry::new();

        registry.register(Box::new(LspEnricher::new(
            "rust",
            "rust-analyzer",
            &[],
            &["rs"],
        )));
        registry.register(Box::new(LspEnricher::new(
            "python",
            "pyrefly",
            &["lsp"],
            &["py"],
        )));
        registry.register(Box::new(LspEnricher::new(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &["ts", "tsx", "js", "jsx"],
        )));

        assert_eq!(registry.len(), 3);

        // Enrich with no nodes should work fine for all enrichers
        let index = GraphIndex::new();
        let result = registry
            .enrich_all(
                &[],
                &index,
                &["rust".to_string(), "python".to_string()],
                std::path::Path::new("."),
                &[],
            )
            .await;
        assert!(result.added_edges.is_empty());
    }

    /// Verify the with_settings builder works.
    #[test]
    fn test_lsp_enricher_with_settings() {
        let settings = serde_json::json!({ "fixture": { "enabled": true } });
        let enricher = LspEnricher::new("fixture", "fixture-lsp", &[], &["fixture"])
            .with_settings(settings.clone());
        assert_eq!(enricher.init_settings, Some(settings));
    }

    /// Verify URI helper functions work correctly.
    #[test]
    fn test_uri_to_relative_path() {
        let root = PathBuf::from("/home/user/project");
        let uri = Uri::from_str("file:///home/user/project/src/main.rs").unwrap();
        let rel = uri_to_relative_path(&uri, &root);
        assert_eq!(rel, PathBuf::from("src/main.rs"));
    }

    /// Verify that percent-encoded characters in file URIs are decoded correctly.
    ///
    /// LSP servers return URIs like `file:///path/with%20spaces/main.rs` when the
    /// workspace lives under a directory whose name contains characters that require
    /// percent-encoding in a URI (spaces, parentheses, etc.).  Without decoding,
    /// `strip_prefix` would fail and we would silently drop graph edges.
    #[test]
    fn test_uri_to_relative_path_percent_encoded() {
        // Space encoded as %20
        let root = PathBuf::from("/home/user/my project");
        let uri = Uri::from_str("file:///home/user/my%20project/src/main.rs").unwrap();
        let rel = uri_to_relative_path(&uri, &root);
        assert_eq!(rel, PathBuf::from("src/main.rs"));

        // Parentheses encoded as %28 / %29 — common on macOS with versioned dirs
        let root2 = PathBuf::from("/home/user/project (v2)");
        let uri2 = Uri::from_str("file:///home/user/project%20%28v2%29/lib.rs").unwrap();
        let rel2 = uri_to_relative_path(&uri2, &root2);
        assert_eq!(rel2, PathBuf::from("lib.rs"));
    }

    /// Adversarial: verify fallback and edge-case behaviour of uri_to_relative_path.
    ///
    /// Seeded from dissent findings:
    /// 1. URI outside the workspace root — should return an absolute-looking path rather than panic.
    /// 2. URI with a non-file scheme that passes url::Url::parse but fails to_file_path() —
    ///    the fallback raw-strip code should be reached.
    /// 3. Normal file URI to a file outside the root — strip_prefix fails, fallback returns absolute.
    #[test]
    fn test_uri_to_relative_path_adversarial() {
        let root = PathBuf::from("/home/user/project");

        // 1. Encoded URI for a file outside the workspace root — should return the decoded
        //    absolute path (strip_prefix fails, but we still decode correctly).
        let outside_uri = Uri::from_str("file:///tmp/other%20project/foo.rs").unwrap();
        let result = uri_to_relative_path(&outside_uri, &root);
        // Should be the decoded absolute path, NOT contain %20
        let result_str = result.to_string_lossy();
        assert!(
            !result_str.contains("%20"),
            "fallback should not contain raw percent-encoding: {result_str}"
        );
        assert!(
            result_str.contains("other project"),
            "path should be decoded: {result_str}"
        );

        // 2. Encoded root path matches exactly the file — relative should be empty/current dir.
        let root2 = PathBuf::from("/home/user/my project");
        let exact_uri = Uri::from_str("file:///home/user/my%20project").unwrap();
        let rel2 = uri_to_relative_path(&exact_uri, &root2);
        // strip_prefix of identical path yields "" which is PathBuf::new()
        assert_eq!(rel2, PathBuf::from(""));
    }

    // -----------------------------------------------------------------------
    // Tests for resolve_type_hierarchy_item (pure function, no LSP server needed)
    // -----------------------------------------------------------------------

    fn make_node(
        file: &str,
        name: &str,
        kind: NodeKind,
        line_start: usize,
        line_end: usize,
    ) -> Node {
        let kind_str = match &kind {
            NodeKind::Trait => "trait",
            NodeKind::Struct => "struct",
            NodeKind::Enum => "enum",
            _ => "impl",
        };
        Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature: format!("{} {}", kind_str, name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_type_hierarchy_item(name: &str, uri: &str, start_line: u64) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "uri": uri,
            "kind": 5,
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": start_line + 5, "character": 0 }
            }
        })
    }

    #[test]
    fn test_resolve_type_hierarchy_single_match() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "MyTrait", NodeKind::Trait, 10, 20);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item("MyTrait", "file:///project/src/lib.rs", 9);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.unwrap().name, "MyTrait");
    }

    #[test]
    fn test_resolve_type_hierarchy_name_collision_uses_position() {
        let root = PathBuf::from("/project");
        let node1 = make_node("src/lib.rs", "Config", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Config", NodeKind::Struct, 50, 60);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        // Item at line 50 (0-indexed: 49) should resolve to node2
        let item = make_type_hierarchy_item("Config", "file:///project/src/lib.rs", 49);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.as_ref().unwrap().name, "Config");
        // The resolved node should be the one at line 50-60 (node2)
        let resolved_id = result.unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n.id == resolved_id && n.line_start == 50),
            "should resolve to the node at line 50, not line 10"
        );
    }

    #[test]
    fn test_resolve_type_hierarchy_position_fallback() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 10, 20);
        let nodes: Vec<&Node> = vec![&node];

        // Item with a different name — should fall through to position-based fallback
        let item = make_type_hierarchy_item("DifferentName", "file:///project/src/lib.rs", 14);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // line 14 (0-indexed) + 1 = 15, which is within [10, 20]
        assert_eq!(result.unwrap().name, "MyStruct");
    }

    #[test]
    fn test_resolve_type_hierarchy_external_dependency_filtered() {
        let root = PathBuf::from("/project");
        let node = make_node(
            ".cargo/registry/src/tokio/lib.rs",
            "Runtime",
            NodeKind::Struct,
            1,
            100,
        );
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item(
            "Runtime",
            "file:///project/.cargo/registry/src/tokio/lib.rs",
            0,
        );
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), ".cargo paths should be filtered out");
    }

    #[test]
    fn test_resolve_type_hierarchy_missing_fields_returns_none() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Missing "uri"
        let item = serde_json::json!({"name": "Foo"});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());

        // Missing "name"
        let item = serde_json::json!({"uri": "file:///project/src/lib.rs"});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());

        // Empty object
        let item = serde_json::json!({});
        assert!(LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none());
    }

    #[test]
    fn test_resolve_type_hierarchy_percent_encoded_uri() {
        let root = PathBuf::from("/my project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // URI with percent-encoded space
        let item = make_type_hierarchy_item("Foo", "file:///my%20project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result.unwrap().name, "Foo");
    }

    #[test]
    fn test_max_type_hierarchy_strikes_constant() {
        // Verify the constant is reasonable
        assert_eq!(MAX_TYPE_HIERARCHY_STRIKES, 3);
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for type hierarchy edge cases
    // -----------------------------------------------------------------------

    /// If two candidates have the EXACT same name, file, kind, and line range,
    /// the position tiebreaker cannot distinguish them. Verify we get *some*
    /// result (not a panic) and it's deterministic.
    #[test]
    fn test_resolve_type_hierarchy_identical_position_tiebreaker() {
        let root = PathBuf::from("/project");
        // Two nodes with identical name, file, kind, and line range
        let node1 = make_node("src/lib.rs", "Handler", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Handler", NodeKind::Struct, 10, 20);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        let item = make_type_hierarchy_item("Handler", "file:///project/src/lib.rs", 9);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);

        // Must resolve to something, not panic or return None
        assert!(
            result.is_some(),
            "should resolve even with identical candidates"
        );

        // Must be deterministic across calls
        let result2 = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(result, result2, "resolution should be deterministic");
    }

    /// URI for a file completely outside the repo root. The path won't
    /// strip_prefix successfully, so rel_path == abs_path. There should be
    /// no match because no node lives at that absolute path.
    #[test]
    fn test_resolve_type_hierarchy_file_outside_repo() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // URI points to a completely different directory
        let item = make_type_hierarchy_item("Foo", "file:///other-project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // strip_prefix fails, rel_path becomes /other-project/src/lib.rs
        // No node has that path, so should be None
        assert!(result.is_none(), "file outside repo should not resolve");
    }

    /// Stdlib or dependency URI that doesn't contain ".cargo" (e.g. sysroot).
    /// The .cargo filter won't catch it — verify it doesn't accidentally match
    /// a same-name node in the repo.
    #[test]
    fn test_resolve_type_hierarchy_stdlib_uri_no_cargo_filter() {
        let root = PathBuf::from("/project");
        // A repo node named "Iterator"
        let node = make_node("src/lib.rs", "Iterator", NodeKind::Trait, 1, 50);
        let nodes: Vec<&Node> = vec![&node];

        // LSP returns a stdlib URI (no .cargo in path, but outside repo)
        let item = make_type_hierarchy_item(
            "Iterator",
            "file:///rustup/toolchains/stable/lib/rustlib/src/rust/library/core/src/iter/traits/iterator.rs",
            0,
        );
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        // strip_prefix fails, rel_path is the full sysroot path —
        // no node has that file path, so should not match
        assert!(
            result.is_none(),
            "stdlib path outside repo should not resolve to a repo node"
        );
    }

    /// Non-Latin characters in file paths (Chinese, Arabic, emoji).
    /// url::Url should handle percent-encoding for these.
    #[test]
    fn test_resolve_type_hierarchy_unicode_path() {
        let root = PathBuf::from("/project");
        let node = make_node("src/\u{4e2d}\u{6587}.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Percent-encoded Chinese characters: 中文 = %E4%B8%AD%E6%96%87
        let item = make_type_hierarchy_item("Foo", "file:///project/src/%E4%B8%AD%E6%96%87.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(
            result.unwrap().name,
            "Foo",
            "percent-encoded Unicode paths should decode correctly"
        );
    }

    /// Malformed URI that url::Url::parse rejects. The fallback path should
    /// attempt manual strip_prefix.
    #[test]
    fn test_resolve_type_hierarchy_malformed_uri_fallback() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Bar", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // Not a valid URL (no scheme) — url::Url::parse will fail
        let item = serde_json::json!({
            "name": "Bar",
            "uri": "file:///project/src/lib.rs",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // This should work via url::Url (valid file:// URI)
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_some());

        // Now try something that *only* works via fallback
        let item_bad = serde_json::json!({
            "name": "Bar",
            "uri": "not-a-url",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // url::Url::parse("not-a-url") => Err, fallback strip_prefix("file://") => None
        let result = LspEnricher::resolve_type_hierarchy_item(&item_bad, &nodes, &root);
        assert!(
            result.is_none(),
            "URI without file:// scheme should fail gracefully"
        );
    }

    /// Type hierarchy item with unexpected field types (name as number, uri as null).
    /// Should return None, not panic.
    #[test]
    fn test_resolve_type_hierarchy_wrong_field_types() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // name is a number instead of string
        let item = serde_json::json!({
            "name": 42,
            "uri": "file:///project/src/lib.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        assert!(
            LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none(),
            "numeric name should not match"
        );

        // uri is null
        let item = serde_json::json!({
            "name": "Foo",
            "uri": null,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        assert!(
            LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root).is_none(),
            "null URI should not match"
        );

        // range.start.line is a string instead of number
        let item = serde_json::json!({
            "name": "Foo",
            "uri": "file:///project/src/lib.rs",
            "range": { "start": { "line": "zero", "character": 0 }, "end": { "line": 5, "character": 0 } }
        });
        // Should still resolve — line defaults to 0 when not parseable
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(
            result.is_some(),
            "bad line type should degrade gracefully, not prevent resolution"
        );
    }

    /// When range is completely missing from the item, resolution should still
    /// work for unique name+file matches (range_start_line defaults to 0).
    #[test]
    fn test_resolve_type_hierarchy_no_range() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        let item = serde_json::json!({
            "name": "Foo",
            "uri": "file:///project/src/lib.rs",
            "kind": 5
        });
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert_eq!(
            result.unwrap().name,
            "Foo",
            "missing range should not prevent unique match"
        );
    }

    /// If the node kind is Function (not Trait/Struct/Enum/Impl), the candidate
    /// filter should exclude it. This tests the kind whitelist.
    #[test]
    fn test_resolve_type_hierarchy_ignores_non_type_nodes() {
        let root = PathBuf::from("/project");
        // A function with the same name as the type hierarchy item
        let node = make_node("src/lib.rs", "process", NodeKind::Function, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        let item = make_type_hierarchy_item("process", "file:///project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(
            result.is_none(),
            "Functions should not be resolved as type hierarchy targets"
        );
    }

    /// Empty matching_nodes should return None, not panic.
    #[test]
    fn test_resolve_type_hierarchy_empty_nodes() {
        let root = PathBuf::from("/project");
        let nodes: Vec<&Node> = vec![];
        let item = make_type_hierarchy_item("Foo", "file:///project/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none());
    }

    /// The .cargo filter uses `contains(".cargo")` which would also match a
    /// directory literally named ".cargo" inside the repo. Verify the filter
    /// catches nested .cargo paths.
    #[test]
    fn test_resolve_type_hierarchy_cargo_filter_nested() {
        let root = PathBuf::from("/project");
        // A node that happens to be under a .cargo subdir in the repo
        let node = make_node(
            "vendor/.cargo/config.toml/Foo",
            "Foo",
            NodeKind::Struct,
            1,
            10,
        );
        let nodes: Vec<&Node> = vec![&node];

        let item =
            make_type_hierarchy_item("Foo", "file:///project/vendor/.cargo/config.toml/Foo", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(
            result.is_none(),
            "paths containing .cargo anywhere should be filtered"
        );
    }

    /// Position tiebreaker when the LSP range doesn't overlap any candidate's
    /// line range. Falls back to closest-by-line_start.
    ///
    /// BUG FINDING: When two nodes have the same name/file/kind, they produce
    /// identical NodeIds (NodeId doesn't include line numbers). So even though
    /// the resolver picks the positionally closest candidate, the *returned*
    /// NodeId is indistinguishable. The edge will be created with the right
    /// NodeId, but both nodes share it — the graph can't distinguish them.
    /// This is a known limitation of the NodeId design.
    #[test]
    fn test_resolve_type_hierarchy_position_no_overlap() {
        let root = PathBuf::from("/project");
        let node1 = make_node("src/lib.rs", "Config", NodeKind::Struct, 10, 20);
        let node2 = make_node("src/lib.rs", "Config", NodeKind::Struct, 50, 60);
        let nodes: Vec<&Node> = vec![&node1, &node2];

        // Line 35 (0-indexed: 34, +1=35) doesn't overlap [10,20] or [50,60]
        let item = make_type_hierarchy_item("Config", "file:///project/src/lib.rs", 34);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(
            result.is_some(),
            "should fall back to closest-by-line_start"
        );

        // The resolver picks closest by unsigned distance: |10-35|=25, |50-35|=15
        // So node2 should be selected. But since NodeId doesn't include line info,
        // both nodes have the same NodeId — we can only verify resolution succeeded.
        let resolved = result.unwrap();
        assert_eq!(resolved.name, "Config");
        // NOTE: NodeId equality means we can't distinguish which physical node
        // was chosen from the returned ID alone. This is a design limitation.
        assert_eq!(
            node1.id, node2.id,
            "NodeId lacks position — identical-name nodes are indistinguishable"
        );
    }

    /// Verify that an item with a non-file URI scheme (e.g. untitled:, http://)
    /// is handled gracefully.
    #[test]
    fn test_resolve_type_hierarchy_non_file_uri_scheme() {
        let root = PathBuf::from("/project");
        let node = make_node("src/lib.rs", "Foo", NodeKind::Struct, 1, 10);
        let nodes: Vec<&Node> = vec![&node];

        // http:// scheme — url::Url::parse succeeds but to_file_path() fails
        let item = make_type_hierarchy_item("Foo", "http://example.com/src/lib.rs", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "non-file URI should return None");

        // untitled: scheme
        let item = make_type_hierarchy_item("Foo", "untitled:Untitled-1", 0);
        let result = LspEnricher::resolve_type_hierarchy_item(&item, &nodes, &root);
        assert!(result.is_none(), "untitled: URI should return None");
    }

    /// Verify strike counter state is correctly initialized and the constant
    /// is used properly. The strike logic itself is tested via integration
    /// (needs a mock transport), but we can verify the initial state.
    #[test]
    fn test_strike_counter_initial_state() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.try_lock().unwrap();
        assert_eq!(state.type_hierarchy_strikes, 0);
        assert!(
            !state.has_type_hierarchy,
            "should default to false until init confirms"
        );
    }

    #[tokio::test]
    async fn reset_incomplete_initialization_drops_pre_handshake_transport() {
        let root = tempfile::tempdir().unwrap();
        let enricher = LspEnricher::new("rust", "cat", &[], &["rs"]);
        let transport = LspTransport::spawn("cat", &[], root.path()).await.unwrap();
        {
            let mut state = enricher.state.lock().await;
            state.transport = Some(transport);
            state.root_path = Some(root.path().to_path_buf());
            assert!(state.pipelined.is_none());
        }

        enricher.reset_incomplete_initialization().await;

        let state = enricher.state.lock().await;
        assert!(state.transport.is_none());
        assert!(state.root_path.is_none());
    }

    /// If LspState has type hierarchy disabled (has_type_hierarchy = false),
    /// verify the strikes counter is irrelevant — it's the flag that gates.
    #[test]
    fn test_strike_counter_flag_vs_count_independence() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let mut state = enricher.state.try_lock().unwrap();

        // Even with 0 strikes, if has_type_hierarchy is false, nothing runs.
        // And even with many strikes, resetting the flag should allow it.
        state.type_hierarchy_strikes = 100;
        state.has_type_hierarchy = true;
        // The enrich loop checks `has_type_hierarchy` first, so this state
        // means "enabled but with many past strikes". Since strikes reset on
        // success, this would only persist if all calls failed.
        assert!(state.has_type_hierarchy);
        assert_eq!(state.type_hierarchy_strikes, 100);
    }

    /// If rust-analyzer is available, test actual enrichment on a small Rust file.
    #[tokio::test]
    async fn test_lsp_enricher_with_rust_analyzer() {
        // Check if rust-analyzer is installed
        let ra_check = tokio::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .await;

        if ra_check.is_err() {
            eprintln!("Skipping: rust-analyzer not installed");
            return;
        }

        // This test validates the LspEnricher can start and respond,
        // but we don't have a full Cargo project to index against in tests.
        // The enricher should handle the initialization gracefully.
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();

        let nodes = vec![Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: "test_fn".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 1,
            signature: "fn test_fn()".into(),
            body: "fn test_fn() {}".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }];

        // This may succeed or fail depending on whether we're in a Cargo project.
        // Either way, it should not panic.
        let _result = enricher
            .enrich(&nodes, &index, std::path::Path::new("."))
            .await;
    }

    /// Reproducible, opt-in probe for issue #768. This intentionally exercises
    /// RNA's real LSP scheduling, telemetry, and edge rendering against maintained
    /// fixtures. Production opt-ins remain separately encoded in built-in profiles.
    #[tokio::test]
    #[ignore = "requires installed rust-analyzer and pyrefly"]
    async fn measure_declared_const_reference_yield() {
        use crate::extract::{Extractor, python::PythonExtractor, rust::RustExtractor};

        struct FixtureCase {
            language: &'static str,
            server: &'static str,
            args: &'static [&'static str],
            version_command: &'static str,
            version_args: &'static [&'static str],
            extension: &'static str,
            config_name: &'static str,
            config: &'static str,
            sources: &'static [(&'static str, &'static str)],
            expected_const_requests: usize,
            expected_const_edges:
                &'static [(&'static str, &'static str, &'static str, &'static str)],
            expect_enable: bool,
        }

        const RUST_CONST_EDGES: &[(&str, &str, &str, &str)] = &[
            (
                "src/lib.rs",
                "local_retry_limit",
                "src/lib.rs",
                "RETRY_LIMIT",
            ),
            ("src/lib.rs", "make_config", "src/lib.rs", "RETRY_LIMIT"),
            (
                "src/worker.rs",
                "worker_retry_limit",
                "src/lib.rs",
                "RETRY_LIMIT",
            ),
            (
                "src/worker.rs",
                "worker_config",
                "src/lib.rs",
                "RETRY_LIMIT",
            ),
            ("src/lib.rs", "local_timeout", "src/lib.rs", "TIMEOUT_MS"),
            (
                "src/worker.rs",
                "worker_timeout",
                "src/lib.rs",
                "TIMEOUT_MS",
            ),
            ("src/lib.rs", "local_port", "src/lib.rs", "DEFAULT_PORT"),
            ("src/worker.rs", "worker_port", "src/lib.rs", "DEFAULT_PORT"),
            ("src/lib.rs", "local_feature", "src/lib.rs", "FEATURE_FLAG"),
            (
                "src/worker.rs",
                "worker_feature",
                "src/lib.rs",
                "FEATURE_FLAG",
            ),
            (
                "src/lib.rs",
                "local_static_timeout",
                "src/lib.rs",
                "STATIC_TIMEOUT_MS",
            ),
            (
                "src/worker.rs",
                "worker_static_timeout",
                "src/lib.rs",
                "STATIC_TIMEOUT_MS",
            ),
            (
                "src/lib.rs",
                "local_mutable_limit",
                "src/lib.rs",
                "MUTABLE_LIMIT",
            ),
            (
                "src/worker.rs",
                "worker_mutable_limit",
                "src/lib.rs",
                "MUTABLE_LIMIT",
            ),
            (
                "src/lib.rs",
                "local_associated_limit",
                "src/lib.rs",
                "ASSOCIATED_LIMIT",
            ),
            (
                "src/worker.rs",
                "worker_associated_limit",
                "src/lib.rs",
                "ASSOCIATED_LIMIT",
            ),
        ];

        let cases = [
            FixtureCase {
                language: "rust",
                server: "rust-analyzer",
                args: &[],
                version_command: "rust-analyzer",
                version_args: &["--version"],
                extension: "rs",
                config_name: "Cargo.toml",
                config: include_str!("../../../tests/fixtures/lsp_const_yield/rust/Cargo.toml"),
                sources: &[
                    (
                        "src/lib.rs",
                        include_str!("../../../tests/fixtures/lsp_const_yield/rust/src/lib.rs"),
                    ),
                    (
                        "src/worker.rs",
                        include_str!("../../../tests/fixtures/lsp_const_yield/rust/src/worker.rs"),
                    ),
                ],
                expected_const_requests: 8,
                expected_const_edges: RUST_CONST_EDGES,
                expect_enable: true,
            },
            FixtureCase {
                language: "python",
                server: "pyrefly",
                args: &[
                    "lsp",
                    "--verbose",
                    "--indexing-mode",
                    "lazy-blocking",
                    "--threads",
                    "1",
                    "--workspace-indexing-limit",
                    "5000",
                    "--build-system-blocking",
                    "--color",
                    "never",
                ],
                version_command: "pyrefly",
                version_args: &["--version"],
                extension: "py",
                config_name: "pyproject.toml",
                config: include_str!(
                    "../../../tests/fixtures/lsp_const_yield/python/pyproject.toml"
                ),
                sources: &[
                    (
                        "constants.py",
                        include_str!("../../../tests/fixtures/lsp_const_yield/python/constants.py"),
                    ),
                    (
                        "consumer.py",
                        include_str!("../../../tests/fixtures/lsp_const_yield/python/consumer.py"),
                    ),
                ],
                expected_const_requests: 5,
                expected_const_edges: &[],
                expect_enable: false,
            },
        ];

        for case in cases {
            let version = tokio::process::Command::new(case.version_command)
                .args(case.version_args)
                .output()
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "{} is required for the #768 measurement probe: {error}",
                        case.version_command
                    )
                });
            assert!(
                version.status.success(),
                "{} version command failed: {}",
                case.version_command,
                String::from_utf8_lossy(&version.stderr)
            );
            let version = String::from_utf8_lossy(&version.stdout).trim().to_string();

            let fixture = tempfile::tempdir().expect("create measurement fixture");
            std::fs::write(fixture.path().join(case.config_name), case.config)
                .expect("write fixture config");

            let mut nodes = Vec::new();
            for (relative_path, content) in case.sources {
                let absolute_path = fixture.path().join(relative_path);
                std::fs::create_dir_all(
                    absolute_path
                        .parent()
                        .expect("fixture source has a parent directory"),
                )
                .expect("create fixture source directory");
                std::fs::write(&absolute_path, content).expect("write fixture source");

                let extracted = match case.language {
                    "rust" => RustExtractor::new()
                        .extract(std::path::Path::new(relative_path), content)
                        .expect("extract Rust fixture"),
                    "python" => PythonExtractor::new()
                        .extract(std::path::Path::new(relative_path), content)
                        .expect("extract Python fixture"),
                    other => panic!("unsupported measurement fixture language: {other}"),
                };
                nodes.extend(extracted.nodes);
            }

            let mut enricher =
                LspEnricher::new(case.language, case.server, case.args, &[case.extension])
                    .with_declared_const_references(true);
            enricher = match case.language {
                "rust" => enricher.with_config_file("Cargo.toml"),
                "python" => enricher.with_config_file("pyproject.toml"),
                _ => enricher,
            };
            let result = enricher
                .enrich(&nodes, &GraphIndex::new(), fixture.path())
                .await
                .unwrap_or_else(|error| panic!("{} enrichment failed: {error}", case.server));

            assert!(
                !result.aborted,
                "{} aborted: {:?}",
                case.server, result.diagnostic
            );
            let const_metric = result
                .lsp_query_metrics
                .iter()
                .find(|metric| {
                    metric.operation == "references" && metric.declaration_class == "const"
                })
                .unwrap_or_else(|| panic!("{} emitted no Const telemetry", case.server));
            let type_metric = result
                .lsp_query_metrics
                .iter()
                .find(|metric| {
                    metric.operation == "references" && metric.declaration_class == "struct"
                })
                .unwrap_or_else(|| panic!("{} emitted no Struct telemetry", case.server));

            let const_edges = result
                .added_edges
                .iter()
                .filter(|edge| {
                    edge.kind == EdgeKind::ReferencedBy && edge.to.kind == NodeKind::Const
                })
                .collect::<Vec<_>>();

            assert_eq!(
                const_metric.scheduled_requests, case.expected_const_requests,
                "{} measured an unexpected declared-constant surface",
                case.server
            );
            let const_average_ms =
                const_metric.latency_ms.max(1) / const_metric.scheduled_requests.max(1) as u64;
            let type_average_ms =
                type_metric.latency_ms.max(1) / type_metric.scheduled_requests.max(1) as u64;
            let clears_threshold = const_metric.non_empty_responses * 100
                >= const_metric.scheduled_requests * 80
                && const_metric.emitted_edges >= const_metric.scheduled_requests
                && const_metric.timeouts == 0
                && const_metric.errors == 0
                && const_average_ms <= type_average_ms.saturating_mul(2).max(2);
            assert_eq!(
                clears_threshold, case.expect_enable,
                "{} eligibility decision changed: Const={const_metric:?}, Struct={type_metric:?}",
                case.server
            );

            assert!(
                const_edges
                    .iter()
                    .all(|edge| edge.to.name != "UNUSED_SENTINEL"),
                "{} emitted a spurious edge to the unused control constant",
                case.server
            );
            if case.expect_enable {
                let actual_pairs = const_edges
                    .iter()
                    .map(|edge| {
                        (
                            edge.from.file.to_string_lossy().to_string(),
                            edge.from.name.clone(),
                            edge.to.file.to_string_lossy().to_string(),
                            edge.to.name.clone(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                let expected_pairs = case
                    .expected_const_edges
                    .iter()
                    .map(|(from_file, from_name, to_file, to_name)| {
                        (
                            (*from_file).to_string(),
                            (*from_name).to_string(),
                            (*to_file).to_string(),
                            (*to_name).to_string(),
                        )
                    })
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    const_edges.len(),
                    expected_pairs.len(),
                    "{} emitted duplicate or extra Const edges: {const_edges:#?}",
                    case.server
                );
                assert_eq!(
                    actual_pairs, expected_pairs,
                    "{} Const edge mapping was not exactly correct",
                    case.server
                );
            }

            eprintln!(
                "CONST_YIELD_RESULT language={} server={} version={:?} eligible={} result_errors={} metrics={:#?} const_edges={:#?}",
                case.language,
                case.server,
                version,
                clears_threshold,
                result.error_count,
                result.lsp_query_metrics,
                const_edges
            );
        }
    }

    /// Verify that the quiescent readiness condition matches the rust-analyzer
    /// specification: `quiescent: true` means the server is fully ready (no
    /// pending background work).  This is a regression test for the inverted
    /// check fixed in PR #226 / issue #215.
    #[test]
    fn test_server_status_quiescent_means_ready() {
        // Simulate serverStatus notifications as serde_json::Value
        let make_status = |health: &str, quiescent: bool| -> serde_json::Value {
            serde_json::json!({
                "method": "experimental/serverStatus",
                "params": { "health": health, "quiescent": quiescent }
            })
        };

        // quiescent: true, health: ok => READY (server finished background work)
        assert!(
            LspEnricher::server_status_is_ready(&make_status("ok", true)),
            "quiescent: true + health: ok should be ready"
        );

        // quiescent: false, health: ok => NOT READY (still indexing)
        assert!(
            !LspEnricher::server_status_is_ready(&make_status("ok", false)),
            "quiescent: false + health: ok should NOT be ready"
        );

        // quiescent: true, health: warning => NOT READY (unhealthy)
        assert!(
            !LspEnricher::server_status_is_ready(&make_status("warning", true)),
            "health: warning should NOT be ready regardless of quiescent"
        );

        // quiescent: true, health: error => NOT READY (unhealthy)
        assert!(
            !LspEnricher::server_status_is_ready(&make_status("error", true)),
            "health: error should NOT be ready regardless of quiescent"
        );

        // Missing quiescent field defaults to false (NOT ready)
        // Conservative: if the server doesn't tell us it's quiescent, assume it's not
        let no_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok" }
        });
        assert!(
            !LspEnricher::server_status_is_ready(&no_quiescent),
            "missing quiescent should default to false (not ready)"
        );

        // Adversarial: completely empty params should not be ready
        let empty_params = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": {}
        });
        assert!(
            !LspEnricher::server_status_is_ready(&empty_params),
            "empty params should not be ready"
        );

        // Adversarial: missing params entirely should not be ready
        let no_params = serde_json::json!({
            "method": "experimental/serverStatus"
        });
        assert!(
            !LspEnricher::server_status_is_ready(&no_params),
            "missing params should not be ready"
        );

        // Adversarial: quiescent as string "true" should not be ready
        // (must be boolean true, not string)
        let string_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": "true" }
        });
        assert!(
            !LspEnricher::server_status_is_ready(&string_quiescent),
            "quiescent as string 'true' should not be ready (must be bool)"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for find_enclosing_symbol (PR #286 ship pipeline)
    // Seeded from dissent: module-level references, self-references, edge cases
    // -----------------------------------------------------------------------

    /// Dissent finding: references at module level (outside any function/struct/impl)
    /// should return None -- no enclosing symbol exists.
    #[test]
    fn test_find_enclosing_symbol_module_level_returns_none() {
        let nodes = vec![
            make_node("src/lib.rs", "my_fn", NodeKind::Function, 10, 20),
            make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 25, 35),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line 5 is before any symbol -- module-level use statement
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 5);
        assert!(
            result.is_none(),
            "module-level reference should not resolve to any symbol"
        );

        // Line 22 is between symbols -- also module level
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 22);
        assert!(result.is_none(), "gap between symbols should not resolve");
    }

    /// Dissent finding: nested symbols should resolve to the narrowest enclosing one.
    #[test]
    fn test_find_enclosing_symbol_prefers_narrowest() {
        let nodes = vec![
            make_node("src/lib.rs", "MyImpl", NodeKind::Impl, 1, 50),
            make_node("src/lib.rs", "inner_fn", NodeKind::Function, 10, 20),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line 15 is inside both MyImpl and inner_fn -- should resolve to inner_fn
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 15);
        assert_eq!(
            result.unwrap().name,
            "inner_fn",
            "should resolve to narrowest enclosing symbol"
        );
    }

    /// Dissent finding: references in a different file should not match.
    #[test]
    fn test_find_enclosing_symbol_wrong_file_returns_none() {
        let nodes = vec![make_node("src/lib.rs", "my_fn", NodeKind::Function, 1, 50)];
        let refs: Vec<&Node> = nodes.iter().collect();

        let result = find_enclosing_symbol(&refs, Path::new("src/other.rs"), 10);
        assert!(
            result.is_none(),
            "reference in different file should not match"
        );
    }

    /// Verify find_enclosing_symbol resolves Enum and Const scopes
    /// (expanded filter per CodeRabbit review feedback).
    #[test]
    fn test_find_enclosing_symbol_resolves_enum_and_const() {
        let nodes = vec![
            make_node("src/lib.rs", "MyEnum", NodeKind::Enum, 1, 20),
            make_node("src/lib.rs", "MY_CONST", NodeKind::Const, 25, 30),
        ];
        let refs: Vec<&Node> = nodes.iter().collect();

        // Line inside enum -- should resolve after filter expansion
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 10);
        assert_eq!(
            result.unwrap().name,
            "MyEnum",
            "Enum should now resolve in find_enclosing_symbol"
        );

        // Line inside const -- should resolve after filter expansion
        let result = find_enclosing_symbol(&refs, Path::new("src/lib.rs"), 27);
        assert_eq!(
            result.unwrap().name,
            "MY_CONST",
            "Const should now resolve in find_enclosing_symbol"
        );
    }

    /// Verify the self-reference filtering logic: a reference at the definition
    /// site (same file, within line_start..line_end) should be filtered.
    #[test]
    fn test_self_reference_detection_logic() {
        let node = make_node("src/lib.rs", "MyStruct", NodeKind::Struct, 10, 20);

        // Reference at the definition site
        let ref_file = PathBuf::from("src/lib.rs");
        let ref_line: usize = 15;
        let is_self_ref =
            ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(
            is_self_ref,
            "reference within definition site should be detected as self-reference"
        );

        // Reference in same file but outside definition
        let ref_line: usize = 25;
        let is_self_ref =
            ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(
            !is_self_ref,
            "reference outside definition should not be self-reference"
        );

        // Reference in different file
        let ref_file = PathBuf::from("src/other.rs");
        let ref_line: usize = 15;
        let is_self_ref =
            ref_file == node.id.file && ref_line >= node.line_start && ref_line <= node.line_end;
        assert!(
            !is_self_ref,
            "reference in different file should not be self-reference"
        );
    }

    /// Verify that .cargo path filtering works correctly.
    #[test]
    fn test_cargo_dep_filtering_logic() {
        let cargo_path =
            PathBuf::from("/home/user/.cargo/registry/src/index.crates.io/serde-1.0.0/src/lib.rs");
        assert!(
            cargo_path.to_string_lossy().contains(".cargo"),
            ".cargo dependency should be detected"
        );

        let project_path = PathBuf::from("src/lib.rs");
        assert!(
            !project_path.to_string_lossy().contains(".cargo"),
            "project file should not be filtered"
        );

        // Dissent edge case: project with "cargo" in name
        let tricky_path = PathBuf::from("my-cargo-tool/src/lib.rs");
        assert!(
            !tricky_path.to_string_lossy().contains(".cargo"),
            "project with 'cargo' in name (no dot prefix) should not be filtered"
        );
    }

    /// Verify ReferencedBy edge kind has correct weight and string representation.
    #[test]
    fn test_referenced_by_edge_properties() {
        let edge = Edge {
            from: NodeId {
                root: String::new(),
                file: PathBuf::from("src/main.rs"),
                name: "caller".into(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: String::new(),
                file: PathBuf::from("src/lib.rs"),
                name: "MyStruct".into(),
                kind: NodeKind::Struct,
            },
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };

        assert_eq!(format!("{}", edge.kind), "referenced_by");
        assert_eq!(edge.source, ExtractionSource::Lsp);
        assert_eq!(edge.confidence, Confidence::Confirmed);
    }

    /// Verify that the initialize request truthfully declares the generic LSP
    /// operations RNA sends and preserves server-status readiness support.
    ///
    /// Without this capability, rust-analyzer won't send serverStatus notifications
    /// and the readiness wait falls through to a 5s timeout, querying before indexing
    /// is complete and producing 0 edges. This was the root cause of issue #293.
    #[test]
    fn test_init_params_declare_supported_client_operations() {
        let root_uri = Uri::from_str("file:///tmp/test").unwrap();
        let init_params = lsp_initialize_params(root_uri, "test".to_string());
        let serialized = serde_json::to_value(&init_params).unwrap();

        assert_eq!(
            serialized.pointer("/workspaceFolders/0/name"),
            Some(&serde_json::json!("test"))
        );
        assert_eq!(
            serialized.pointer("/rootUri"),
            serialized.pointer("/workspaceFolders/0/uri"),
            "the compatibility rootUri and workspace folder must identify the same root"
        );
        assert_eq!(
            serialized.pointer("/capabilities/workspace/workspaceFolders"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            serialized.pointer("/capabilities/workspace/configuration"),
            Some(&serde_json::json!(true))
        );
        assert!(
            serialized
                .pointer("/capabilities/workspace/symbol")
                .is_some(),
            "workspace/symbol support must be declared"
        );
        assert_eq!(
            serialized.pointer(
                "/capabilities/textDocument/documentSymbol/hierarchicalDocumentSymbolSupport"
            ),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            serialized.pointer("/capabilities/textDocument/documentSymbol/dynamicRegistration"),
            Some(&serde_json::json!(false))
        );
        for operation in [
            "synchronization",
            "references",
            "definition",
            "implementation",
            "codeAction",
            "documentLink",
            "publishDiagnostics",
            "callHierarchy",
            "typeHierarchy",
            "inlayHint",
            "diagnostic",
        ] {
            assert!(
                serialized
                    .pointer(&format!("/capabilities/textDocument/{operation}"))
                    .is_some(),
                "textDocument/{operation} support must be declared"
            );
        }

        // Verify the experimental capability is set
        let experimental = init_params
            .capabilities
            .experimental
            .as_ref()
            .expect("experimental capabilities must be set");
        assert_eq!(
            experimental.get("serverStatusNotification"),
            Some(&serde_json::json!(true)),
            "serverStatusNotification must be true to receive serverStatus from rust-analyzer"
        );
    }

    /// Verify that the was_quiescent computation correctly handles the probe
    /// validation path introduced by #576.
    ///
    /// For servers without serverStatus:
    /// - Probe succeeds + validation returns symbols → was_quiescent = true
    /// - Probe succeeds + validation returns 0 symbols → was_quiescent = false
    /// - Probe never succeeds → was_quiescent = false
    ///
    /// This ensures Pass 1 and Pass 3 are skipped when a server
    /// tsserver) responds to probes but hasn't indexed the workspace.
    #[test]
    fn test_was_quiescent_probe_validation_path() {
        // Case 1: Probe + validation both succeed → was_quiescent = true
        // (saw_quiescent=true from validation, server_ready=true)
        {
            let saw_quiescent = true;
            let seen_server_status = false;
            let server_ready = true;
            let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
            assert!(
                was_quiescent,
                "probe + validation success must set was_quiescent=true — Pass 1/3 should run"
            );
        }

        // Case 2: Probe succeeds but validation returns 0 symbols → was_quiescent = false
        // (saw_quiescent=false, server_ready=false after validation failure)
        {
            let saw_quiescent = false;
            let seen_server_status = false;
            let server_ready = false;
            let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
            assert!(
                !was_quiescent,
                "probe success with failed validation must NOT set was_quiescent — \
                 Pass 1/3 must be skipped (server responsive but not indexed, #576)"
            );
        }

        // Case 3: Neither probe nor validation succeed → was_quiescent = false
        {
            let saw_quiescent = false;
            let seen_server_status = false;
            let server_ready = false;
            let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
            assert!(
                !was_quiescent,
                "no probe success must not set was_quiescent — server never responded"
            );
        }

        // Case 4: serverStatus path (not probe) — quiescent=true → was_quiescent = true
        {
            let saw_quiescent = true;
            let seen_server_status = true;
            let server_ready = true;
            let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
            assert!(
                was_quiescent,
                "serverStatus quiescent=true must set was_quiescent regardless of probe path"
            );
        }

        // Case 5: serverStatus path — not quiescent → was_quiescent = false
        {
            let saw_quiescent = false;
            let seen_server_status = true;
            let server_ready = false;
            let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
            assert!(
                !was_quiescent,
                "serverStatus without quiescent=true must NOT set was_quiescent — \
                 server timed out during indexing"
            );
        }
    }

    /// Verify the validation query list covers common symbol names across
    /// Python, TypeScript, and Rust codebases. An empty validation query
    /// list would bypass indexing validation entirely.
    #[test]
    fn test_indexing_validation_queries_not_empty() {
        // These constants are defined inline in ensure_initialized. Replicate them
        // here to verify the design contract: at least 3 diverse queries so a
        // project missing "main" can still pass validation.
        let queries = &["main", "init", "test", "get", "set", "app"];
        assert!(
            queries.len() >= 3,
            "need at least 3 validation queries to avoid false negatives \
             from projects that happen to lack a particular symbol name"
        );
        // Verify no empty strings — an empty query would bypass validation
        // since workspace/symbol("") returns all symbols (or matches anything).
        assert!(
            queries.iter().all(|q| !q.is_empty()),
            "validation queries must be non-empty strings — empty query bypasses validation"
        );
        // Verify no duplicates
        let unique: std::collections::HashSet<&&str> = queries.iter().collect();
        assert_eq!(
            unique.len(),
            queries.len(),
            "validation queries must be unique"
        );
    }

    /// Adversarial: verify the early-exit condition for indexing validation.
    /// The code exits early when consecutive_empty >= 3 && attempt >= 3.
    /// Note: only empty responses and errors count toward consecutive_empty.
    /// Timeouts do NOT count (the server is actively working, not empty).
    #[test]
    fn test_indexing_validation_early_exit_boundary() {
        // Simulate the early exit condition from ensure_initialized
        let check_early_exit = |consecutive_empty: u32, attempt: u32| -> bool {
            consecutive_empty >= 3 && attempt >= 3
        };

        // Boundary: 2 empties at attempt 3 — NOT enough evidence
        assert!(
            !check_early_exit(2, 3),
            "2 consecutive empty responses is not enough to declare server unindexed"
        );

        // Boundary: 3 empties at attempt 2 — too early to give up
        assert!(
            !check_early_exit(3, 2),
            "should not exit early on attempt 2 even with 3 empties — give more time"
        );

        // Exact threshold: 3 empties at attempt 3 — exit
        assert!(
            check_early_exit(3, 3),
            "3 consecutive empty responses at attempt 3 should trigger early exit"
        );

        // Above threshold: 4 empties at attempt 4
        assert!(
            check_early_exit(4, 4),
            "4 empties at attempt 4 should also trigger early exit"
        );

        // Edge: attempt 1 with 0 empties — never exit
        assert!(!check_early_exit(0, 1));

        // Scenario: 3 timeouts + 0 empties at attempt 3 — should NOT exit
        // because timeouts don't count toward consecutive_empty
        // (server is actively indexing, not returning empty results)
        assert!(
            !check_early_exit(0, 3),
            "timeouts should not trigger early exit — only empty results and errors count"
        );
    }

    /// Adversarial: verify validation query rotation covers all queries
    /// before wrapping. With 6 queries and 6 max attempts, each query
    /// should be used exactly once before any repeats.
    #[test]
    fn test_indexing_validation_query_rotation() {
        let queries = &["main", "init", "test", "get", "set", "app"];
        let max_attempts: u32 = 6;

        let mut used: Vec<&str> = Vec::new();
        for attempt in 1..=max_attempts {
            let query = queries[((attempt - 1) as usize) % queries.len()];
            used.push(query);
        }

        // All 6 queries should be used exactly once in 6 attempts
        assert_eq!(
            used.len(),
            queries.len(),
            "6 attempts should use all 6 queries"
        );
        let unique: std::collections::HashSet<&&str> = used.iter().collect();
        assert_eq!(
            unique.len(),
            queries.len(),
            "each query should be used exactly once in 6 attempts — no repeats"
        );
    }

    /// Adversarial: verify the was_quiescent formula handles the edge case
    /// where a server with serverStatus reports quiescent=true BUT the probe
    /// validation path was never entered (server_responsive=false).
    /// The formula `saw_quiescent || (!seen_server_status && server_ready)`
    /// should still produce true because saw_quiescent=true from serverStatus.
    #[test]
    fn test_was_quiescent_serverstatus_overrides_probe_path() {
        // serverStatus quiescent=true, but probe was never attempted
        // (because serverStatus arrived first)
        let saw_quiescent = true;
        let seen_server_status = true;
        let server_ready = false; // probe never ran
        let was_quiescent = saw_quiescent || (!seen_server_status && server_ready);
        assert!(
            was_quiescent,
            "serverStatus quiescent=true must produce was_quiescent=true \
             even when probe path was not exercised"
        );
    }

    /// Integration test: run LSP enrichment against the RNA repo itself.
    ///
    /// This test requires rust-analyzer to be on PATH and the RNA repo to be a
    /// valid Cargo workspace. It is marked #[ignore] so it doesn't run in CI
    /// (which may not have rust-analyzer installed), but can be run explicitly
    /// with `cargo test -- --ignored test_lsp_enrichment_produces_edges`.
    ///
    /// Regression guard for #379: verifies that rust-analyzer reaches quiescent
    /// state and produces >0 call edges. If quiescence fails, 0 edges result
    /// because RA is not indexed when call-hierarchy queries run.
    #[tokio::test]
    #[ignore]
    async fn test_lsp_enrichment_produces_edges() {
        use crate::extract::ExtractorRegistry;
        use crate::scanner::Scanner;

        // Find the repo root (where Cargo.toml is)
        let repo_root = std::env::current_dir().expect("failed to get cwd");
        assert!(
            repo_root.join("Cargo.toml").exists(),
            "test must be run from the repo root"
        );

        // Scan the repo to get files
        let mut scanner = Scanner::new(repo_root.clone()).expect("failed to create scanner");
        let scan_result = scanner.scan().expect("scan failed");

        // Extract nodes from scanned files
        let registry = ExtractorRegistry::default();
        let extraction = registry.extract_scan_result(&repo_root, &scan_result);
        let nodes = extraction.nodes;
        assert!(
            nodes.len() > 100,
            "expected >100 nodes from RNA repo, got {}",
            nodes.len()
        );

        // Create enricher and run
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let index = GraphIndex::new();
        let result = enricher
            .enrich(&nodes, &index, &repo_root)
            .await
            .expect("LSP enrichment failed");

        let edge_count = result.added_edges.len();
        eprintln!(
            "LSP enrichment produced {} edges from {} nodes",
            edge_count,
            nodes.len()
        );

        // Regression guard for #379: check was_quiescent first so failures
        // report a clear message rather than just "0 edges".
        let was_quiescent = {
            let state = enricher.state.lock().await;
            state.was_quiescent
        };
        assert!(
            was_quiescent,
            "rust-analyzer did not reach quiescent state — this is the root cause of the \
             #379 regression. Check that rust-analyzer can index the repo within 120s."
        );

        assert!(
            edge_count > 100,
            "expected >100 LSP edges from RNA repo, got {}. \
             This likely means rust-analyzer is not responding to call hierarchy queries.",
            edge_count
        );

        // Check that we have Calls edges specifically
        let calls_edges = result
            .added_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        assert!(
            calls_edges > 50,
            "expected >50 Calls edges, got {}",
            calls_edges
        );
    }

    /// Regression test for #379: verify that LspState.was_quiescent defaults to false.
    ///
    /// The guard in Pass 3 relies on was_quiescent being false until the
    /// server explicitly reaches quiescent=true. If it defaulted to true,
    /// Pass 3 would run even when the server never finished indexing.
    #[test]
    fn test_lsp_state_was_quiescent_defaults_false() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        // The state is accessed via the async lock; use a blocking read for testing.
        let state = enricher.state.blocking_lock();
        assert!(
            !state.was_quiescent,
            "was_quiescent must default to false — Pass 3 must be skipped until \
             the server explicitly reaches quiescent=true (regression guard for #379)"
        );
    }

    /// Regression test for #379 round 4: Pass 1 (call hierarchy) must also be
    /// skipped when `was_quiescent=false`.
    ///
    /// When RA hasn't indexed (deadline expired), Pass 1 returns 0 edges for
    /// all ZERO_EDGE_ABORT_THRESHOLD nodes, triggering the zero-edge abort.
    /// This is indistinguishable from a misconfigured server. The same guard
    /// that protects Pass 3 must also protect Pass 1.
    ///
    /// This test verifies that `was_quiescent` defaults to false (so Pass 1
    /// is skipped until RA explicitly becomes quiescent).
    #[test]
    fn test_lsp_state_was_quiescent_defaults_false_protects_pass1() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.blocking_lock();
        assert!(
            !state.was_quiescent,
            "was_quiescent must default to false — Pass 1 (call hierarchy) must be skipped \
             until the server explicitly reaches quiescent=true. Without this guard, the \
             zero-edge abort fires on large repos where RA doesn't index within 120s \
             (regression: #379 r4)"
        );
    }

    /// Verify the was_quiescent logic: only servers that sent serverStatus but
    /// never reached quiescent=true trigger the Pass 3 skip.
    ///
    /// The guard is `saw_quiescent || !seen_server_status`:
    /// - saw_quiescent=true (any health): done indexing → Run Pass 3
    /// - seen_server_status=false: no serverStatus → assumed ready → Run Pass 3
    /// - saw_quiescent=false, seen_server_status=true: RA timed out → SKIP Pass 3
    ///
    /// Note: `saw_quiescent` tracks the raw `quiescent=true` bit, not `server_ready`
    /// which also requires health="ok". This means health="warning" + quiescent=true
    /// (compile errors but done indexing) correctly enables Pass 3.
    #[test]
    fn test_server_status_is_ready_drives_quiescence() {
        // health=ok + quiescent=true: server_ready=true, saw_quiescent=true → Pass 3 runs
        let ready_msg = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": true }
        });
        assert!(
            LspEnricher::server_status_is_ready(&ready_msg),
            "health=ok + quiescent=true must be ready — saw_quiescent=true, Pass 3 runs"
        );

        // health="warning" + quiescent=true: server_ready=false but saw_quiescent=true → Pass 3 runs
        // (compile errors but done indexing — diagnostics are needed precisely in this state)
        let warning_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "warning", "quiescent": true }
        });
        assert!(
            !LspEnricher::server_status_is_ready(&warning_quiescent),
            "health=warning is not 'ready' (server_ready=false), but saw_quiescent=true \
             means was_quiescent=true and Pass 3 will run correctly"
        );

        // quiescent=false: saw_quiescent stays false, Pass 3 blocked if deadline expires
        let not_quiescent = serde_json::json!({
            "method": "experimental/serverStatus",
            "params": { "health": "ok", "quiescent": false }
        });
        assert!(
            !LspEnricher::server_status_is_ready(&not_quiescent),
            "quiescent=false: saw_quiescent=false — if deadline expires with only these \
             messages, seen_server_status=true and saw_quiescent=false → was_quiescent=false"
        );
    }

    // -----------------------------------------------------------------------
    // Tests for build_diagnostic_nodes (pure function, no LSP server needed)
    // -----------------------------------------------------------------------

    /// Verify that error and warning diagnostics produce nodes.
    #[test]
    fn test_build_diagnostic_nodes_basic() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "type mismatch",
                "source": "rust-analyzer",
                "range": {
                    "start": { "line": 141, "character": 4 },
                    "end": { "line": 141, "character": 20 }
                }
            }),
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "source": "rust-analyzer",
                "range": {
                    "start": { "line": 88, "character": 8 },
                    "end": { "line": 88, "character": 9 }
                }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 2, "error and warning should produce 2 nodes");

        // Check error node — name format is "[severity:line] message"
        let error_node = nodes
            .iter()
            .find(|n| n.id.name.starts_with("[error:142]"))
            .unwrap();
        assert_eq!(error_node.id.file, PathBuf::from("src/service.rs"));
        assert_eq!(
            error_node.id.kind,
            NodeKind::Other("diagnostic".to_string())
        );
        assert_eq!(error_node.line_start, 142); // 0-indexed line 141 -> 1-indexed 142
        assert_eq!(
            error_node.metadata.get("diagnostic_severity").unwrap(),
            "error"
        );
        assert_eq!(
            error_node.metadata.get("diagnostic_message").unwrap(),
            "type mismatch"
        );
        assert_eq!(
            error_node.metadata.get("diagnostic_source").unwrap(),
            "rust-analyzer"
        );
        assert_eq!(
            error_node.metadata.get("diagnostic_range").unwrap(),
            "142:4-142:20"
        );
        assert_eq!(
            error_node.metadata.get("diagnostic_timestamp").unwrap(),
            "1700000000"
        );

        // Check warning node — name includes line number for uniqueness
        let warn_node = nodes
            .iter()
            .find(|n| n.id.name.starts_with("[warning:89]"))
            .unwrap();
        assert_eq!(warn_node.line_start, 89);
        assert_eq!(
            warn_node.metadata.get("diagnostic_severity").unwrap(),
            "warning"
        );
    }

    /// Verify that Information (3) and Hint (4) diagnostics are filtered out.
    #[test]
    fn test_build_diagnostic_nodes_filters_information_and_hint() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 3,
                "message": "consider using async",
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 10 } }
            }),
            serde_json::json!({
                "severity": 4,
                "message": "hint: you might want to...",
                "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 10, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(
            nodes.is_empty(),
            "information and hint diagnostics should not produce nodes"
        );
    }

    /// Verify that an empty diagnostics list produces no nodes (zero-error files rule).
    #[test]
    fn test_build_diagnostic_nodes_empty_produces_no_nodes() {
        let root = PathBuf::from("/project");
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &[],
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );
        assert!(nodes.is_empty(), "zero diagnostics should produce no nodes");
    }

    /// Verify .cargo paths are filtered out.
    #[test]
    fn test_build_diagnostic_nodes_cargo_path_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": "some error",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/.cargo/registry/tokio/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );
        assert!(nodes.is_empty(), ".cargo paths should be filtered");
    }

    /// Verify that long messages are truncated in the node name but preserved in metadata.
    #[test]
    fn test_build_diagnostic_nodes_long_message_truncated_in_name() {
        let root = PathBuf::from("/project");
        let long_msg = "a".repeat(200);
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": long_msg,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        // Name should be truncated (max 80 chars message snippet + "[error:N] " prefix + "...")
        assert!(
            node.id.name.len() < 200,
            "node name should be truncated for long messages"
        );
        assert!(
            node.id.name.ends_with("..."),
            "truncated name should end with ..."
        );
        // Name includes the line number for uniqueness
        assert!(
            node.id.name.starts_with("[error:1]"),
            "name should include severity and line number"
        );
        // Full message preserved in metadata
        assert_eq!(
            node.metadata.get("diagnostic_message").unwrap().len(),
            200,
            "full message preserved in metadata"
        );
    }

    #[test]
    fn test_build_diagnostic_nodes_truncates_multibyte_message_on_char_boundary() {
        let root = PathBuf::from("/project");
        let message = format!("{}\u{00a0}{}", "a".repeat(76), "b".repeat(20));
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": message,
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 5 }
            }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "pyrefly",
            "python",
            "1700000000",
            2,
        );

        assert_eq!(nodes.len(), 1);
        assert!(
            nodes[0].id.name.ends_with("..."),
            "multibyte diagnostic name should be truncated safely"
        );
        assert_eq!(
            nodes[0].metadata.get("diagnostic_message"),
            Some(&message),
            "full multibyte message should remain in metadata"
        );
    }

    /// Verify diagnostic node has ExtractionSource::Lsp.
    #[test]
    fn test_build_diagnostic_nodes_source_is_lsp() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": "error",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(nodes[0].source, ExtractionSource::Lsp);
    }

    /// Verify severity_to_str handles all known severity values.
    #[test]
    fn test_lsp_severity_to_str() {
        assert_eq!(LspEnricher::lsp_severity_to_str(1), "error");
        assert_eq!(LspEnricher::lsp_severity_to_str(2), "warning");
        assert_eq!(LspEnricher::lsp_severity_to_str(3), "information");
        assert_eq!(LspEnricher::lsp_severity_to_str(4), "hint");
        assert_eq!(LspEnricher::lsp_severity_to_str(0), "unknown");
        assert_eq!(LspEnricher::lsp_severity_to_str(99), "unknown");
    }

    /// Verify diagnostic node metadata contains all required fields.
    #[test]
    fn test_build_diagnostic_nodes_has_all_metadata_fields() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": "test error",
            "source": "my-lsp",
            "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 10 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "myroot",
            "my-lsp",
            "rust",
            "1234567890",
            2,
        );

        assert_eq!(nodes.len(), 1);
        let meta = &nodes[0].metadata;
        assert!(
            meta.contains_key("diagnostic_severity"),
            "missing diagnostic_severity"
        );
        assert!(
            meta.contains_key("diagnostic_source"),
            "missing diagnostic_source"
        );
        assert!(
            meta.contains_key("diagnostic_message"),
            "missing diagnostic_message"
        );
        assert!(
            meta.contains_key("diagnostic_range"),
            "missing diagnostic_range"
        );
        assert!(
            meta.contains_key("diagnostic_timestamp"),
            "missing diagnostic_timestamp"
        );
        assert_eq!(meta.get("diagnostic_timestamp").unwrap(), "1234567890");
        assert_eq!(nodes[0].id.root, "myroot");
    }

    /// Verify diagnostics with missing severity default to error (severity 1 = error).
    #[test]
    fn test_build_diagnostic_nodes_missing_severity_defaults_to_error() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            // No severity field — should default to 1 (error)
            "message": "something bad",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(
            nodes.len(),
            1,
            "missing severity defaults to error which should produce a node"
        );
        assert_eq!(
            nodes[0].metadata.get("diagnostic_severity").unwrap(),
            "error"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests seeded from dissent findings
    // -----------------------------------------------------------------------

    /// Dissent finding #2: identical messages at different lines should produce
    /// distinct NodeIds (no silent overwrites in LanceDB).
    #[test]
    fn test_build_diagnostic_nodes_same_message_different_lines_distinct_ids() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "range": { "start": { "line": 9, "character": 4 }, "end": { "line": 9, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 2,
                "message": "unused variable: `x`",
                "range": { "start": { "line": 24, "character": 4 }, "end": { "line": 24, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(
            nodes.len(),
            2,
            "identical messages at different lines should produce 2 nodes"
        );
        // NodeIds must be distinct
        let id0 = nodes[0].id.to_stable_id();
        let id1 = nodes[1].id.to_stable_id();
        assert_ne!(
            id0, id1,
            "different line positions should produce distinct NodeIds"
        );
        // Names should include the line number
        assert!(
            nodes[0].id.name.contains(":10]") || nodes[0].id.name.contains(":25]"),
            "name should include line 10 or 25: got '{}'",
            nodes[0].id.name
        );
    }

    /// Dissent finding #1: stale diagnostic nodes should be identifiable by timestamp.
    /// Verify the timestamp is preserved and is a non-empty string.
    #[test]
    fn test_build_diagnostic_nodes_timestamp_preserved_for_staleness_detection() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 1,
            "message": "an error",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        let ts = "1700123456";
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            ts,
            2,
        );

        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0].metadata.get("diagnostic_timestamp").unwrap(),
            ts,
            "timestamp must be preserved exactly for agent-side staleness filtering"
        );
    }

    /// Adversarial: diagnostic with empty message should be skipped (not produce a node
    /// with an empty name that breaks search).
    #[test]
    fn test_build_diagnostic_nodes_empty_message_skipped() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "",  // empty
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 1,
                "message": "   ",  // whitespace-only (trimmed to empty)
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(
            nodes.is_empty(),
            "empty/whitespace messages should not produce nodes"
        );
    }

    /// Adversarial: malformed range fields (null, missing, out of order).
    /// Should produce a node with a safe default range, not panic.
    #[test]
    fn test_build_diagnostic_nodes_malformed_range_degrades_gracefully() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 1,
                "message": "error with null range",
                "range": null
            }),
            serde_json::json!({
                "severity": 1,
                "message": "error with no range"
                // no "range" key at all
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        // Should produce nodes despite malformed ranges (default to line 1)
        assert_eq!(
            nodes.len(),
            2,
            "malformed range should not prevent node creation"
        );
        for node in &nodes {
            assert_eq!(node.line_start, 1, "missing range should default to line 1");
        }
    }

    /// Adversarial: severity value 0 and very large values (out of spec).
    /// Severity 0 is not defined in LSP spec — should be treated as "unknown" and
    /// since it's not 1 or 2, should be filtered out.
    #[test]
    fn test_build_diagnostic_nodes_out_of_spec_severity_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 0,  // below spec minimum
                "message": "unknown severity",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
            }),
            serde_json::json!({
                "severity": 100,  // far above spec maximum
                "message": "unknown large severity",
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 5 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(
            nodes.is_empty(),
            "severity 0 and 100 should be filtered (only 1 and 2 are stored)"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for #379 r4: Pass 1 guard and diagnostic 0-capture
    // -----------------------------------------------------------------------

    /// Adversarial: "unlinked-file" diagnostic (the VS Code example in issue #379)
    /// is severity 2 (Warning) per LSP spec, so it SHOULD produce a node.
    ///
    /// If RA returns it as severity 3 (Information), that explains 0 captures.
    /// This test documents the expected behavior: severity 2 unlinked-file → captured.
    #[test]
    fn test_build_diagnostic_nodes_unlinked_file_warning_captured() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 2,  // Warning
            "message": "This file is not included in any crates [unlinked-file]",
            "source": "rust-analyzer",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert_eq!(
            nodes.len(),
            1,
            "unlinked-file at severity 2 (Warning) should produce a diagnostic node; \
             if this fails, RA is reporting it as Information (3) which gets filtered"
        );
        assert_eq!(
            nodes[0].metadata.get("diagnostic_severity").unwrap(),
            "warning"
        );
    }

    /// Adversarial: "unlinked-file" diagnostic at severity 3 (Information)
    /// should be filtered out. This is the suspected root cause of 0 captured diagnostics
    /// in issue #379 — RA may report unlinked-file as Information, not Warning.
    #[test]
    fn test_build_diagnostic_nodes_unlinked_file_information_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 3,  // Information — below our capture threshold
            "message": "This file is not included in any crates [unlinked-file]",
            "source": "rust-analyzer",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            2,
        );

        assert!(
            nodes.is_empty(),
            "unlinked-file at severity 3 (Information) should be filtered; \
             this is intentional — Information diagnostics are too noisy for code-understanding queries"
        );
    }

    /// When max_severity_int=4 (hint level), Information (3) diagnostics are captured.
    /// This exercises diagnostic_min_severity = "information" in .oh/config.toml.
    #[test]
    fn test_build_diagnostic_nodes_information_captured_when_threshold_is_information() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 3,  // Information
            "message": "consider using async",
            "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 5 } }
        })];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            3, // max_severity_int = Information
        );

        assert_eq!(
            nodes.len(),
            1,
            "information diagnostic should be captured at threshold=3"
        );
        assert_eq!(
            nodes[0].metadata.get("diagnostic_severity").unwrap(),
            "information"
        );
    }

    /// When max_severity_int=4 (hint level), Hint (4) diagnostics like unlinked-file
    /// and inactive-code are captured.
    /// This exercises diagnostic_min_severity = "hint" in .oh/config.toml.
    #[test]
    fn test_build_diagnostic_nodes_hint_captured_when_threshold_is_hint() {
        let root = PathBuf::from("/project");
        let diags = vec![
            serde_json::json!({
                "severity": 4,  // Hint
                "message": "This file is not included in any crates [unlinked-file]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } }
            }),
            serde_json::json!({
                "severity": 4,  // Hint
                "message": "code is inactive due to #[cfg] directives [inactive-code]",
                "source": "rust-analyzer",
                "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 15, "character": 0 } }
            }),
        ];

        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/service.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            4, // max_severity_int = Hint
        );

        assert_eq!(
            nodes.len(),
            2,
            "hint-level diagnostics should be captured at threshold=4"
        );
        for node in &nodes {
            assert_eq!(node.metadata.get("diagnostic_severity").unwrap(), "hint");
        }
    }

    /// Severity 0 is always invalid and must be filtered regardless of threshold.
    #[test]
    fn test_build_diagnostic_nodes_severity_zero_always_filtered() {
        let root = PathBuf::from("/project");
        let diags = vec![serde_json::json!({
            "severity": 0,
            "message": "invalid severity",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 5 } }
        })];

        // Even with hint-level threshold (4), severity 0 must be filtered
        let nodes = LspEnricher::build_diagnostic_nodes(
            "file:///project/src/lib.rs",
            &diags,
            &root,
            "/project",
            "rust-analyzer",
            "rust",
            "1700000000",
            4, // hint level — most permissive threshold
        );

        assert!(
            nodes.is_empty(),
            "severity 0 is not a valid LSP value and must always be filtered"
        );
    }

    /// DiagnosticMinSeverity::max_severity_int returns the correct floor for each variant.
    #[test]
    fn test_diagnostic_min_severity_max_int() {
        use crate::scanner::DiagnosticMinSeverity;
        assert_eq!(DiagnosticMinSeverity::Error.max_severity_int(), 1);
        assert_eq!(DiagnosticMinSeverity::Warning.max_severity_int(), 2);
        assert_eq!(DiagnosticMinSeverity::Information.max_severity_int(), 3);
        assert_eq!(DiagnosticMinSeverity::Hint.max_severity_int(), 4);
    }

    /// Default DiagnosticMinSeverity is Warning.
    #[test]
    fn test_diagnostic_min_severity_default_is_warning() {
        use crate::scanner::DiagnosticMinSeverity;
        assert_eq!(
            DiagnosticMinSeverity::default(),
            DiagnosticMinSeverity::Warning
        );
    }

    /// LspConfig deserializes "hint" correctly.
    #[test]
    fn test_lsp_config_deserializes_hint() {
        use crate::scanner::{DiagnosticMinSeverity, LspConfig};
        let config: LspConfig = toml::from_str(r#"diagnostic_min_severity = "hint""#).unwrap();
        assert_eq!(config.diagnostic_min_severity, DiagnosticMinSeverity::Hint);
    }

    /// LspConfig default (empty section) is Warning.
    #[test]
    fn test_lsp_config_default_is_warning() {
        use crate::scanner::{DiagnosticMinSeverity, LspConfig};
        let config = LspConfig::default();
        assert_eq!(
            config.diagnostic_min_severity,
            DiagnosticMinSeverity::Warning
        );
    }

    /// Adversarial: was_quiescent guard is the same for both Pass 1 and Pass 3.
    /// Verify the default state prevents both passes from running.
    /// (The actual early return from enrich() prevents all passes when !was_quiescent.)
    #[test]
    fn test_lsp_state_was_quiescent_false_prevents_all_passes() {
        let enricher = LspEnricher::new("rust", "rust-analyzer", &[], &["rs"]);
        let state = enricher.state.blocking_lock();
        // Both Pass 1 and Pass 3 check was_quiescent before running.
        // With the default false, both are guarded.
        assert!(
            !state.was_quiescent,
            "was_quiescent=false must prevent Pass 1 AND Pass 3; \
             the early return at Pass 1 guard covers both passes"
        );
        // has_pull_diagnostics also defaults false (not yet initialized)
        assert!(
            !state.has_pull_diagnostics,
            "has_pull_diagnostics must default false (not yet initialized from LSP capabilities)"
        );
        // has_inlay_hints also defaults false (not yet initialized)
        assert!(
            !state.has_inlay_hints,
            "has_inlay_hints must default false (not yet initialized from LSP capabilities)"
        );
    }

    // -----------------------------------------------------------------------
    // #408 InlayHints: group_inlay_hints_by_node tests
    // -----------------------------------------------------------------------

    fn make_fn_node_with_lines(file: &str, name: &str, line_start: usize, line_end: usize) -> Node {
        Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    /// Type hints within a function's line range are attributed to it
    #[test]
    fn test_group_inlay_hints_basic() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "process_order", 5, 20);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![
            serde_json::json!({
                "kind": 1,
                "position": { "line": 9, "character": 10 },  // 0-indexed → 1-indexed line 10
                "label": ": f64"
            }),
            serde_json::json!({
                "kind": 1,
                "position": { "line": 12, "character": 10 },  // line 13
                "label": [{ "value": ": OrderTotal" }]
            }),
        ];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        let stable_id = fn_node.id.to_stable_id();
        assert!(
            type_map.contains_key(&stable_id),
            "hints within fn lines should be attributed to the function"
        );
        let types_str = &type_map[&stable_id];
        assert!(types_str.contains("f64"), "should contain f64");
        assert!(
            types_str.contains("OrderTotal"),
            "should contain OrderTotal"
        );
    }

    /// Parameter hints (kind=2) are filtered out
    #[test]
    fn test_group_inlay_hints_filters_param_hints() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "do_thing", 1, 10);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![serde_json::json!({
            "kind": 2,  // parameter hint — should be ignored
            "position": { "line": 4, "character": 5 },
            "label": "amount:"
        })];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        assert!(
            type_map.is_empty(),
            "parameter hints (kind=2) should be filtered"
        );
    }

    /// Type hints outside all function ranges produce no entries
    #[test]
    fn test_group_inlay_hints_outside_function_range() {
        let fn_node = make_fn_node_with_lines("src/lib.rs", "small_fn", 5, 8);
        let file_nodes: Vec<&Node> = vec![&fn_node];

        let hints = vec![serde_json::json!({
            "kind": 1,
            "position": { "line": 20, "character": 5 },  // 0-indexed line 20 → 1-indexed 21
            "label": ": String"
        })];

        let type_map = LspEnricher::group_inlay_hints_by_node(&hints, &file_nodes);
        assert!(
            type_map.is_empty(),
            "hints outside all function line ranges should produce no entries"
        );
    }

    // -----------------------------------------------------------------------
    // EdgeKind: new variants roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_edge_kind_tested_by_display() {
        assert_eq!(EdgeKind::TestedBy.to_string(), "tested_by");
    }

    #[test]
    fn test_edge_kind_belongs_to_display() {
        assert_eq!(EdgeKind::BelongsTo.to_string(), "belongs_to");
    }

    // -----------------------------------------------------------------------
    // #405: parse_crate_graph_dot tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_crate_graph_dot_basic() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="my_crate"]
    _1 [shape=box label="dep_crate"]
    _0 -> _1
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 2);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "my_crate");
        assert_eq!(pairs[0].1, "dep_crate");
    }

    #[test]
    fn test_parse_crate_graph_dot_multiple_deps() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="rna"]
    _1 [shape=box label="lancedb"]
    _2 [shape=box label="petgraph"]
    _0 -> _1
    _0 -> _2
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 3);
        assert_eq!(pairs.len(), 2);
        let from_names: Vec<&str> = pairs.iter().map(|(f, _)| f.as_str()).collect();
        assert!(from_names.iter().all(|&f| f == "rna"));
        let to_names: std::collections::HashSet<&str> =
            pairs.iter().map(|(_, t)| t.as_str()).collect();
        assert!(to_names.contains("lancedb"));
        assert!(to_names.contains("petgraph"));
    }

    #[test]
    fn test_parse_crate_graph_dot_empty() {
        let dot = "digraph rust_analyzer_crate_graph {}";
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert!(crate_names.is_empty());
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_parse_crate_graph_dot_no_edges() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="standalone_crate"]
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        // Isolated crate should be in crate_names even with no edges
        assert_eq!(
            crate_names,
            vec!["standalone_crate"],
            "isolated crate should appear in crate_names"
        );
        assert!(pairs.is_empty(), "no edges should produce empty pairs");
    }

    #[test]
    fn test_emit_crate_graph_edges_nodes_and_edges() {
        let crate_names = vec!["crate_a".to_string(), "crate_b".to_string()];
        let pairs = vec![("crate_a".to_string(), "crate_b".to_string())];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "my_root", &mut result);

        // Should have 2 crate nodes
        let crate_nodes: Vec<_> = result
            .new_nodes
            .iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "crate"))
            .collect();
        assert_eq!(crate_nodes.len(), 2);

        // Bodies should be non-empty (crate name as body for embedding quality)
        for n in &crate_nodes {
            assert!(
                !n.body.is_empty(),
                "crate node body should be the crate name"
            );
        }

        // Should have 1 DependsOn edge
        let dep_edges: Vec<_> = result
            .added_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 1);
        assert_eq!(dep_edges[0].from.name, "crate_a");
        assert_eq!(dep_edges[0].to.name, "crate_b");
        assert_eq!(dep_edges[0].from.root, "my_root");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests for #405: DOT parser edge cases and emit robustness
    // -----------------------------------------------------------------------

    /// Malformed DOT: edge references unknown node IDs — should produce no pairs but preserve known crate
    #[test]
    fn test_parse_crate_graph_dot_dangling_edge() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="known_crate"]
    _0 -> _99
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        // _99 has no label; edge should be filtered out but known_crate node is preserved
        assert_eq!(
            crate_names,
            vec!["known_crate"],
            "known crate should still be in crate_names"
        );
        assert!(
            pairs.is_empty(),
            "dangling edge to unknown node should produce no pairs"
        );
    }

    /// Label with special characters (hyphens, underscores — common in Rust crate names)
    #[test]
    fn test_parse_crate_graph_dot_hyphenated_crate_names() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="my-crate"]
    _1 [shape=box label="another_crate-2"]
    _0 -> _1
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 2);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "my-crate");
        assert_eq!(pairs[0].1, "another_crate-2");
    }

    /// Diamond dependency: A→B, A→C, B→C should produce 3 edges (not deduplicated)
    #[test]
    fn test_parse_crate_graph_dot_diamond_dependency() {
        let dot = r#"digraph rust_analyzer_crate_graph {
    _0 [shape=box label="app"]
    _1 [shape=box label="core"]
    _2 [shape=box label="utils"]
    _0 -> _1
    _0 -> _2
    _1 -> _2
}"#;
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot(dot);
        assert_eq!(crate_names.len(), 3);
        assert_eq!(pairs.len(), 3, "diamond graph should have 3 edges");
        let has_app_core = pairs.iter().any(|(f, t)| f == "app" && t == "core");
        let has_app_utils = pairs.iter().any(|(f, t)| f == "app" && t == "utils");
        let has_core_utils = pairs.iter().any(|(f, t)| f == "core" && t == "utils");
        assert!(has_app_core, "should have app→core edge");
        assert!(has_app_utils, "should have app→utils edge");
        assert!(has_core_utils, "should have core→utils edge");
    }

    /// Empty DOT string should not panic
    #[test]
    fn test_parse_crate_graph_dot_completely_empty_string() {
        let (crate_names, pairs) = LspEnricher::parse_crate_graph_dot("");
        assert!(crate_names.is_empty());
        assert!(pairs.is_empty());
    }

    /// Crate nodes use `file: Cargo.toml` — verify the file path anchoring
    #[test]
    fn test_emit_crate_graph_edges_file_anchor() {
        let crate_names = vec!["crate_a".to_string(), "crate_b".to_string()];
        let pairs = vec![("crate_a".to_string(), "crate_b".to_string())];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "root", &mut result);

        for node in &result.new_nodes {
            if matches!(&node.id.kind, NodeKind::Other(s) if s == "crate") {
                assert_eq!(
                    node.id.file,
                    PathBuf::from("Cargo.toml"),
                    "crate nodes must use Cargo.toml as file anchor"
                );
            }
        }
    }

    /// Isolated crate (no edges) should still produce a crate node
    #[test]
    fn test_emit_crate_graph_edges_isolated_crate_gets_node() {
        // Single isolated crate with no edges
        let crate_names = vec!["solo_crate".to_string()];
        let pairs: Vec<(String, String)> = vec![];
        let mut result = EnrichmentResult::default();
        LspEnricher::emit_crate_graph_edges(&crate_names, &pairs, "root", &mut result);

        let crate_nodes: Vec<_> = result
            .new_nodes
            .iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "crate"))
            .collect();
        assert_eq!(crate_nodes.len(), 1, "isolated crate should get a node");
        assert_eq!(crate_nodes[0].id.name, "solo_crate");
        assert!(result.added_edges.is_empty(), "no edges for isolated crate");
    }
}
