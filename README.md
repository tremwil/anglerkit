# anglerkit

Heavily WIP cross-platform Rust hooking library. Meant to be a better `detour`/`retour` with more convenient APIs, stable Rust support, and support for different hook types (trampoline, vtable, IAT, EAT)

Since development has just started, the project is not usable at the moment. A first `0.1.0` beta release is planned once the following features have been implemented:

- [ ] Windows x86 (32 and 64-bit) support
- [ ] Linux x86 (32 and 64-bit) support
- [ ] Trampoline hooks for above architectures
- [ ] Perfect closure forwarding via `closure-ffi`

Other planned features:

- [ ] High-level API over different hook types: vtable, IAT and EAT hooks
- [ ] Support for Aarch64 (and maybe ARMv7)
- [ ] APIs for managing hooks in bulk
- [ ] Platform-specific features, such as thread suspension with post-patch EIP/RIP relocation on windows