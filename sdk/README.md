# Loom extension SDKs

This directory contains reusable bindings and wrappers for Loom's extension API.
SDK subdirectories are named after the language used to write an extension;
`typescript/` is the SDK for extensions written in TypeScript, regardless of the
languages or tools those extensions support.

SDKs expose generic document, provider, process, workspace and UI capabilities.
They do not contain language server clients, protocol initialization, server
installation rules or language-specific behavior. Those belong to the relevant
language server plugin in the `loom-extensions` repository, including the
JavaScript/TypeScript plugin.

The shared contract is `wit/loom-extension/world.wit` at the repository root.
