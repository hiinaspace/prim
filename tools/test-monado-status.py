#!/usr/bin/env python3
"""Exercise Envision-style manifest symlink and failed status-library fallback."""
import json, os, subprocess, tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
source=r'''
#include <stdint.h>
#include <stdlib.h>
void mnd_api_get_version(uint32_t*a,uint32_t*b,uint32_t*c){*a=1;*b=0;*c=0;}
int mnd_root_create(void **p){
#ifdef FAIL_CONNECT
return -1;
#else
*p=malloc(1);return 0;
#endif
}
void mnd_root_destroy(void **p){free(*p);*p=0;}
int mnd_root_update_client_list(void*p){return 0;}
int mnd_root_get_number_clients(void*p,uint32_t*n){*n=0;return 0;}
int mnd_root_get_client_id_at_index(void*p,uint32_t i,uint32_t*n){return -1;}
int mnd_root_get_client_state(void*p,uint32_t i,uint32_t*n){return -1;}
int mnd_root_get_client_name(void*p,uint32_t i,const char**n){return -1;}
'''
with tempfile.TemporaryDirectory(prefix='prim-monado-status-') as directory:
 p=Path(directory);lib=p/'envision/lib';lib.mkdir(parents=True)
 manifest=p/'envision/share/openxr/1/runtime.json';manifest.parent.mkdir(parents=True)
 (p/'client.c').write_text(source)
 for output,flags in [(p/'wrong.so',['-DFAIL_CONNECT']), (lib/'libmonado.so.25',[])]:
  subprocess.run(['cc','-shared','-fPIC',*flags,str(p/'client.c'),'-o',str(output)],check=True)
 (lib/'libopenxr_monado.so').touch()
 manifest.write_text(json.dumps({'runtime':{'library_path':'../../../lib/libopenxr_monado.so'}}))
 (p/'active_runtime.json').symlink_to(manifest)
 env=os.environ|{'XR_RUNTIME_JSON':str(p/'active_runtime.json'),'PRIM_LIBMONADO':str(p/'wrong.so'),
  'PRIM_EXPECT_STATUS_LIBRARY':str(lib/'libmonado.so.25'),'XDG_DATA_HOME':str(p/'profile')}
 subprocess.run([str(ROOT/'.local/godot/bin/godot'),'--headless','--xr-mode','off','--path',str(ROOT/'project'),
  '--script','res://tests/xr_monitor.gd'],env=env,check=True,timeout=15)
