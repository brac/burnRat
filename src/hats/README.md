# Hats

Per-model hats for the rat, overlaid on top of the sprite. Auto-discovered by
filename = model **family**, matching what Rust emits in `GameState.model`
(`src-tauri/src/lib.rs::model_family`):

```
opus.png      sonnet.png      haiku.png      fable.png      other.png
```

Drop a transparent PNG in here named for the family and it lights up
automatically (via the `import.meta.glob("./hats/*.png")` in `main.ts`) — no
code changes. If there's no file for the current model, the hat overlay stays
hidden. Size/position the hat within a 150×150 transparent canvas so it sits on
the rat's head (the overlay is `object-fit: contain` over the sprite box).

TODO: no hat art exists yet — this is the wiring only.
