# Task: Design a Spotify Connect Adapter

You're designing a Spotify Connect adapter for the unified-hifi-control project.

The repo is at /Users/muness1/src/hiphi-repos/unified-hifi-control. Explore the codebase to understand the adapter architecture, then produce a design document.

## Requirements

Your design must cover:

1. **Files to create and modify** — exact paths, not vague descriptions
2. **Traits/interfaces to implement** — name the specific traits, show the key method signatures
3. **Event bus integration** — what zone ID format? what bus events to emit/handle?
4. **Discovery** — how should Spotify Connect discover this bridge on the network? what existing discovery code can you reference?
5. **Configuration** — what fields in the config file? authentication flow?
6. **Reference adapter** — which existing adapter is the closest architectural match and why?
7. **Volume change flow** — trace the complete call chain from web UI volume slider to the actual audio device, naming every function and file in the path

Be specific. Name exact files, structs, traits, enum variants, and function signatures. Vague answers like "add an adapter struct" are not useful.
