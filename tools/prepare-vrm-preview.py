#!/usr/bin/env python3
"""Prepare an isolated, offline development project for a private VRM comparison."""
import argparse
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('model', type=Path)
a = p.parse_args()
model = a.model.expanduser().resolve()
if not model.is_file() or model.suffix.lower() != '.vrm':
    p.error('model must be an existing .vrm file')
stage = ROOT / '.local/private-vrm-preview/project'
# Never hardlink editable files: changes to this scratch project must not reach
# the normal project. Keep its import cache for subsequent preview launches.
shutil.copytree(ROOT / 'project', stage, dirs_exist_ok=True,
                ignore=shutil.ignore_patterns('.godot', 'private_lobby.json'))
secret = stage / 'private_lobby.json'
if secret.exists():
    secret.unlink()
shutil.copyfile(model, stage / 'avatars/models/private-test.vrm')
catalog = stage / 'avatars/catalog.gd'
s = catalog.read_text().replace('const MODELS := {',
    'const MODELS := {\n\t"private_test": {"name":"Private VRM (local test)", "scene":preload("res://avatars/models/private-test.vrm")},', 1)
catalog.write_text(s)
main = stage / 'main.gd'
s = main.read_text().replace('avatar_id = settings.get_value("avatar", "id", "alicia")', 'avatar_id = "private_test"', 1)
start = s.index('func toggle_connection() -> void:')
end = s.index('func toggle_microphone() -> void:', start)
s = s[:start] + 'func toggle_connection() -> void:\n\tstatus_text = "Private VRM preview — offline only"\n\n' + s[end:]
s = s.replace('menu.connection_button.text = "Disconnect" if session.is_active() else "Connect to friends"',
              'menu.connection_button.text = "Private VRM preview — offline only"\n\tmenu.connection_button.disabled = true')
s = s.replace('local_avatar.configure(avatar_id, avatar_height if xr else 1.6, true)',
              'local_avatar.configure(avatar_id, avatar_height if xr else 1.6, true)\n\tif menu.avatar_vowels_only.button_pressed:\n\t\tlocal_avatar.expressions.configure(local_avatar.model, true)')
main.write_text(s)
# A/B the same geometry and audio, instead of comparing two different authors.
expressions = stage / 'avatars/expressions.gd'
s = expressions.read_text().replace('func configure(model: Node) -> void:', 'func configure(model: Node, vowels_only: bool = false) -> void:')
s = s.replace('if label in ["vrc.v_" + NAMES[i], "viseme_" + NAMES[i]]:',
              'if (not vowels_only or i in VOWELS) and label in ["vrc.v_" + NAMES[i], "viseme_" + NAMES[i]]:')
expressions.write_text(s)
menu = stage / 'ui/world_menu.gd'
s = menu.read_text().replace('var avatar_close_up: CheckButton', 'var avatar_close_up: CheckButton\nvar avatar_vowels_only: CheckButton')
s = s.replace('row.add_child(avatar_close_up)', 'row.add_child(avatar_close_up)\n\tavatar_vowels_only = CheckButton.new()\n\tavatar_vowels_only.text = "Vowels only (compare)"\n\tavatar_vowels_only.toggled.connect(func(_value): avatar_selected.emit(avatar_picker.get_item_metadata(avatar_picker.selected)))\n\trow.add_child(avatar_vowels_only)')
menu.write_text(s)
project = stage / 'project.godot'
s = project.read_text().replace('config/custom_user_dir_name="prim"', 'config/custom_user_dir_name="prim-private-vrm-preview"', 1)
project.write_text(s)
# Only the scratch project imports the private file. Nothing is written alongside
# the original model or into the distributable project/import cache.
def import_project():
    subprocess.run([str(ROOT / '.local/godot/bin/godot'), '--headless', '--xr-mode', 'off',
                    '--path', str(stage), '--editor', '--import'], check=True,
                   stdout=sys.stderr)
sidecar = stage / 'avatars/models/private-test.vrm.import'
if not sidecar.exists():
    import_project()
# Prim's driver requires the same explicit first/third-person layers as bundled
# VRMs. Godot-VRM's default third-person-only import uses a different layer.
s = sidecar.read_text()
import re
s = re.sub(r'vrm/head_hiding_method=\d+', 'vrm/head_hiding_method=3', s)
sidecar.write_text(s)
import_project()
print(stage)
