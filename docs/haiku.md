# EffectCraft on air/OS

This independent fork of EffectCraft 0.21.0 (upstream `68a637ad`) runs the full Rust
motion graphics and effects compositor on air/OS (a Haiku distribution), x86_64 and
arm64. The engine, the effects, the expression and script engines, the media decoders
and encoders (FilmCraft's codecs, AV1/HEVC/VP9/Opus), Roto Brush with SAM 2, the
plug-ins and the whole egui interface are upstream's, unchanged. What differs is the host:
upstream's desktop app (`apps/effectcraft`) runs on eframe and winit, which have no Haiku
backend; `apps/effectcraft-haiku` hosts the same `EffectcraftApp` in native Be API windows.

## The host

`apps/effectcraft-haiku` is PhotoCraft's air/OS host (the native window, input,
presentation and multi-window code, shared by every Craft port) around EffectCraft:

- `native/host.cpp`: the `BApplication`, a `BDirectWindow` per egui viewport, keyboard,
  mouse, wheel and touchpad input, the clipboard, file panels, Tracker documents, the
  native menu bar, cursors, the pointer warp of the numeric scrub.
- `src/viewports.rs`, `src/platform.rs`: every egui viewport in a native window of its own
  (floating panels torn out of the dock, the video preview window), as eframe gives them.
- `src/shell.rs`: the interface thread's loop. It runs upstream's `App::logic` and `App::ui`
  in the root viewport's pass, in eframe's order, with a frame that carries the shared wgpu
  device (`crates/ui-egui/src/integration.rs`).
- `src/services.rs`: upstream's `Hooks` (file panels, the clipboard, the appearance,
  audio), file types of what EffectCraft writes, and the project's document type.
- `src/audio.rs` + `native/audio.cpp`: preview audio through the Media Kit.
- `src/media_kit.rs` + `native/media.cpp`: footage through the Media Kit's codecs.
- `src/native_menu.rs`: the menu bar.

Upstream's `crates/ui-egui` builds without eframe on Haiku: `integration.rs` re-exports
eframe's `App` and `Frame` elsewhere and defines a frame of the same shape here, which the
host fills in. Other upstream changes: the settings folder (`crates/host/src/app_dirs.rs`),
the font folders (`crates/text/src/fonts.rs`), and `effectcraft_sam2::gpu::Gpu::install`,
which gives SAM 2 the host's device.

## Rendering and the GPU

The windows, and everything EffectCraft runs on the GPU, use one Vulkan device:

- egui's triangles and the viewer's GPU-composited frames are drawn into a presenter's
  frame texture (`src/present.rs`), which gets them onto the window. `EFFECTCRAFT_PRESENT`
  picks how: `readback` (the default) compares the frame with the last one 64 px tile by
  tile and copies back only the changed tiles; `full` copies the whole frame;
  `direct` (experimental) imports the screen's frame buffer
  (`VkImportScanoutMemoryHAIKU`) and copies into it on the GPU, as PhotoCraft's host does.
- The same device is the window device upstream's desktop app gets from eframe, so the
  GPU compositor (`crates/gpu`: layer compositing, the effects with GPU kernels, motion blur,
  the display pass of 16/32 bpc projects, scopes) and the Render Queue's exports render on it,
  and Roto Brush's SAM 2 runs its compute kernels on it too instead of creating a second
  device.
- `EFFECTCRAFT_RENDERER=auto|cpu|vulkan`. `auto` (the default) uses a hardware Vulkan
  adapter and falls back to software rendering (the windows by a CPU rasteriser,
  compositing on the processor, SAM 2 on the CPU) when there is none, as on boards without
  Vulkan. Settings ▸ Startup & Repair ▸ Window Graphics set to OpenGL means software here
  (there is no OpenGL backend). `vulkan` fails the launch without a device.
- A lost device or an uncaptured GPU error at run time switches the compositor and every
  window to software at the next frame. The project, its undo history and the open panels
  stay; the status bar says the GPU preview was retired. `EFFECTCRAFT_TEST_GPU_FAULT=<seconds>`
  fakes a lost device that long after the start, to try it.
- On the x399's NVIDIA card NVK is used. It spills WGSL functions that index arrays of
  accumulators to memory, which makes such kernels about 20× slower than on other drivers.

Heavy CPU work (rendering, tracking, SAM 2 on the CPU, encoders) shares a global pool of
at most four named worker threads (`ec-worker-N`): air/OS gets slower past about four
threads for this kind of work. `EFFECTCRAFT_THREADS=N` overrides it.

## Media Kit

- **Preview audio** plays through a `BSoundPlayer` node into the system mixer (float stereo
  at Settings ▸ Audio's preview rate; the mixer converts to the output device). It replaces
  upstream's cpal output. Settings ▸ Audio lists the one output, the Media Kit's mixer;
  the Media preferences pick the device.
- **Footage**: a movie or sound EffectCraft's own decoders refuse (MPEG-1/2, MPEG-4 Part 2,
  Windows Media, Theora, VP8, Vorbis, FLAC…) is decoded by the Media Kit's codec plugins
  through `BMediaFile`/`BMediaTrack`. Its footage item's codec ends with `· Media Kit`, which
  is how a saved project knows to read it that way again. Frames are 8-bit RGB; the sound
  is decoded once into memory (up to 64 Mi samples). Few files need it: the app's own
  decoders read the common formats (Help ▸ System Compatibility Report lists them).
- `EFFECTCRAFT_MEDIA_KIT=0` turns the Media Kit's footage off. `EFFECTCRAFT_MEDIA_KIT=prefer`
  sends every movie the Media Kit decodes through it first, so a hardware decoder plugin
  (nvdec, amduvd, Rockchip MPP) decodes H.264 and HEVC. It is not the default: the app's
  own decoders read things the Media Kit's 8-bit RGB path doesn't (10-bit and 4:2:2/4:4:4
  video, colour profiles, timecode, alpha), and seek frame-accurately.

## Integration

- **One running copy** (`B_SINGLE_LAUNCH`): documents opened from Tracker while it runs,
  and a later launch's arguments, open in it (a project opens, asking about unsaved changes
  first; media is imported, as files dropped on the window are). A test copy in another
  folder runs beside the installed one. `--control` needs a copy of its own.
- **Document type**: `application/x-effectcraft` (`.ecproj`, `.ecprojx`), registered by
  the package's post-install script; EffectCraft claims it at start when nothing else is
  its preferred application. Saved projects and Render Queue outputs get their file types
  (`update_mime_info`), which files written with `std::fs` lack.
- **Menu bar**: upstream's menus as a native `BMenuBar` (`EFFECTCRAFT_NATIVE_MENU=0` keeps
  the in-window menu bar). The keys stay with the interface; the menus show them.
- **File panels**: the commands' Open, Import, Save As and folder choices are `BFilePanel`s
  that don't block the interface; the pickers of Settings and effect controls wait for
  theirs.
- **Settings** live in `~/config/settings/EffectCraft` (`EFFECTCRAFT_CONFIG_DIR` overrides
  it); fonts are read from the system's and the user's font folders.
- **Updates**: the package is updated by pkgman and SoftwareUpdater; the start-up update
  check is off. The community links stay.
- **Plug-ins** (WebAssembly, in `Plug-ins`) work as upstream. **Effect helpers** (external
  native programs) start only with `EFFECTCRAFT_HELPERS=1` or Effect ▸ Scan for Effect
  Helpers, as upstream; none are built for air/OS, so the scan finds none.

Not done: input methods (`B_INPUT_METHOD_AWARE`; typed text works, compositions don't),
pen pressure (EffectCraft has no stylus input), printing (EffectCraft doesn't print), the
window's size and place between launches.

## Build and package

```sh
JOBS=24 bash tools/haiku/build.sh           # target/x86_64-unknown-haiku/release/EffectCraft
bash tools/haiku/package.sh --no-build      # artifacts/haiku/effectcraft-<version>-x86_64.hpkg
```

The scripts build the tier-3 Rust target `x86_64-unknown-haiku`, standard library included,
and cross-compile the C++ Be API boundary. `tools/haiku/env.sh` supplies the defaults for
`RUSTUP_HOME`, `CARGO_HOME`, `HAIKU_SYSROOT`, `HAIKU_CROSS` and `CARGO_TARGET_DIR`;
`package.sh` takes `HAIKU_HOST_TOOLS` (Haiku's Linux-hosted `rc`, `xres`, `package`).
EffectCraft embeds no craft-fonts. The air/OS images build it with airos-ci's
`build_effectcraft` (x86_64 and arm64), which reads `apps/effectcraft-haiku/resources`.

`vendor/` holds wgpu 30.0.1 with Haiku added to its Vulkan platform lists
(`vendor/README.md`), as the other Craft ports do.

The icon (`apps/effectcraft-haiku/resources/EffectCraft.hvif`) is original vector art drawn
by `tools/haiku/make-icon.py` (MIT OR Apache-2.0).
