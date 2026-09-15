# Architecture

Scadline uses a small layered architecture. The boundaries follow responsibilities that exist in
the application today; they do not assume interchangeable implementations that are not needed.

```text
main
  └─ presentation ──→ application ──→ infrastructure
          │                   │                 │
          └───────────────────┴──────────────→ domain
```

## Layers

- `domain` contains the UI- and I/O-independent model: meshes, vectors, commits, revisions, and
  timeline history.
- `application` coordinates use cases. `ModelService` turns a model rendering request into a
  success or failure outcome and exposes cache-aware prefetching.
- `infrastructure` contains desktop adapters: libgit2 repository access, OpenSCAD execution, STL
  decoding, TOML configuration, temporary files, and the OS cache location.
- `presentation` contains egui state and rendering. `AppState` receives `UiAction` values and
  asynchronous outcomes. `view` draws the current state; `renderer` owns OpenGL resources.

The presentation flow is one-way:

```text
egui input → UiAction → AppState → application service → RenderOutcome → AppState → next frame
```

This resembles MVU more than data-binding MVVM and fits egui's immediate-mode API. Simple visual
state such as zoom or checkbox values can be updated directly inside the presentation layer.

## Abstraction policy

Concrete types are the default. Git and OpenSCAD do not have interfaces solely for hypothetical
replacement. `CacheLocation` is a trait because cache roots genuinely vary by OS and tests may
inject a temporary location. Add another abstraction only when a second implementation or a
necessary test boundary exists.

Temporary repositories and STL files use the `tempfile` crate. Git history and snapshots use the
local-only `git2` API with vendored libgit2, so the application does not require a `git` or `tar`
executable. OpenSCAD is the default required host command; selecting the optional OpenRSCAD backend
uses its separately installed `openrscad` command instead.
