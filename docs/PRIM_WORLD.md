# PRIM_world: the world markup (v1, provisional)

A Prim world is a plain glTF 2.0 file (`.glb` or `.gltf`) plus one extension, `PRIM_world`,
that says which nodes are the screen, the speakers and the spawn point. The loader is
`client/world/prim_world.gd`; the shipped example is `client/room/theater.glb`. This is
S's provisional choice (2026-11-18) and is meant to be cheap to change.

Coordinates are glTF's: metres, +Y up, the screen faces +Z (a Godot `QuadMesh` does too).

## Fields

On the **glTF root** (`extensions.PRIM_world`):

| field | type | meaning |
| --- | --- | --- |
| `version` | int | `1`. Anything else loads with a warning. |
| `environment.sky_horizon`, `.ground_horizon` | `[r,g,b]` | procedural sky colours |
| `environment.tonemap` | string | `linear`, `reinhard`, `filmic`, `aces`, `agx` |
| `environment.glow` | bool | bloom on or off |

On a **node** (`nodes[i].extensions.PRIM_world`), one `role` per node:

| role | extra fields | meaning |
| --- | --- | --- |
| `screen` | `width`, `height` (m) | the video surface; its mesh gets the picture; one per world |
| `speaker` | `channel`: `left` or `right` | where that channel's audio is placed |
| `spawn` | none | where a joining member starts; the node's -Z is the way they face |

Lights use `KHR_lights_punctual`, materials use core glTF plus `KHR_materials_unlit` and
`KHR_materials_emissive_strength`; nothing Prim-specific is needed for them.

## Example

```json
{
  "extensionsUsed": ["PRIM_world"],
  "extensions": {"PRIM_world": {"version": 1,
    "environment": {"sky_horizon": [0.66, 0.67, 0.69], "tonemap": "filmic", "glow": true}}},
  "nodes": [
    {"name": "Screen", "mesh": 0, "translation": [0, 1.15, -2],
     "children": [1, 2],
     "extensions": {"PRIM_world": {"role": "screen", "width": 3.555, "height": 2.0}}},
    {"name": "LeftSpeaker", "translation": [-2, 0, 0],
     "extensions": {"PRIM_world": {"role": "speaker", "channel": "left"}}},
    {"name": "RightSpeaker", "translation": [2, 0, 0],
     "extensions": {"PRIM_world": {"role": "speaker", "channel": "right"}}},
    {"name": "Spawn", "extensions": {"PRIM_world": {"role": "spawn"}}}
  ]
}
```

## Loader behaviour

`PrimWorld.load_file(path)` returns `{root, placement}`. `placement` has `screen`
(`path`, `pos`, `normal`, `size`), `speakers` (`path`, `channel`, `pos`), `spawn`
(`path`, `pos`, `yaw`), `environment`, and `warnings` (missing screen, missing spawn,
fewer than two speakers, unknown role, second screen or spawn, wrong version). Positions
are relative to the loaded root. The loader adds the nodes to groups `prim_screen`,
`prim_speaker`, `prim_spawn`, builds a `WorldEnvironment` from `environment`, and turns
shadows on for directional lights (glTF has no shadow flag). It does not validate sizes,
triangle counts or textures yet.

## Tests

    godot --headless --path client --script res://world/tests/run_golden.gd      # markup in, placement out
    godot --path client --script res://world/tests/render_probe.gd -- --world res://room/theater.glb --out frame.png
    godot --headless --path client --script res://world/tests/export_theater.gd  # re-make theater.glb
