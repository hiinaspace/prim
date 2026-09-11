#!/usr/bin/env python3
"""Embed a SceneTree test as the main scene for export templates without CLI overrides."""
import argparse
from pathlib import Path
import shutil
import subprocess
root = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--godot', required=True)
p.add_argument('--platform', choices=['Linux', 'Windows'], required=True)
p.add_argument('--script', required=True, help='Path relative to project/')
p.add_argument('--output', required=True)
a = p.parse_args()
stage = root / '.local' / ('test-export-' + a.platform.lower())
shutil.copytree(root / 'project', stage, dirs_exist_ok=True, ignore=shutil.ignore_patterns('.godot'))
script = (root / 'project' / a.script).read_text()
assert script.startswith('extends SceneTree\n')
script = script.replace('extends SceneTree', 'extends Node', 1).replace('func _initialize()', 'func _ready()')
script = script.replace('await process_frame', 'await get_tree().process_frame')
script = script.replace('await create_timer(', 'await get_tree().create_timer(')
script = script.replace('root.', 'get_tree().root.').replace('\tquit(', '\tget_tree().quit(')
(stage / 'test_entry.gd').write_text(script)
(stage / 'test_entry.tscn').write_text('[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://test_entry.gd" id="1"]\n[node name="Test" type="Node"]\nscript = ExtResource("1")\n')
project = stage / 'project.godot'
project.write_text(project.read_text().replace('run/main_scene="res://main.tscn"', 'run/main_scene="res://test_entry.tscn"'))
command = [a.godot, '--headless', '--xr-mode', 'off', '--path', str(stage)]
subprocess.run(command + ['--editor', '--import'], check=True)
subprocess.run(command + ['--export-pack', a.platform, str(Path(a.output).resolve())], check=True)
