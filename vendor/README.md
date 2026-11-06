# Haiku Vulkan backend

`wgpu`, `wgpu-core` and `wgpu-core-deps-windows-linux-android` 30.0.1 are copied from crates.io,
with their upstream MIT/Apache-2.0 licenses. The only changes add `target_os = "haiku"` alongside
FreeBSD in platform dependency and backend cfg lists (`Cargo.toml`, `Cargo.toml.orig`,
`build.rs`). This makes their existing Vulkan backend available on air/OS (a Haiku distribution).
There is no new Vulkan implementation or window surface: the native host
(`apps/effectcraft-haiku`) renders offscreen and presents the finished frame. These are the same
patches the PhotoCraft, LightCraft and office ports of air/OS carry.
