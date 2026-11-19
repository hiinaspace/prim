# Blind listening packets for the timeline voice receiver

Offline only; nothing here plays audio.

- `dependencies/godot-network-audio/voice-core/examples/listen_render.rs`: `pkts` (encode at 16 kb/s, DTX on, seeded loss pattern, writes an events file), `play` (new PlayoutReceiver on a simulated clock), `codec` (no-loss encode/decode, logs Opus TOC bandwidth and bytes per packet).
- `legacy/legacy_render.rs`: the old NetEq `VoiceReceiver`, same events file. Build in a scratch worktree of the submodule at 56b4c31 with the neteq patch at rev e854b5a (the one prim used), copy the file into `voice-core/examples/`. It runs in real time (the old receiver reads `Instant::now`).
- `build_packets.py`: runs both, aligns each output to the source (lag found by envelope correlation), fades 50 ms, levels to -26 dBFS RMS, writes FLAC + webm-opus 96 kb/s and `KEY-open-only-after-answering.json`.
- `make_pages.py`: the two single-file pages. Keys are never read.

Sources. Packet 1: LibriSpeech test-clean (CC BY 4.0; Panayotov, Chen, Povey, Khudanpur), 6930-75918-0005 and 1320-122617-0036, 16 kHz origin upsampled to 48 kHz. Packet 2: VoiceBank-DEMAND `clean_testset_wav` (CC BY 4.0), which is the VCTK corpus (Veaux, Yamagishi, MacDonald, CSTR, University of Edinburgh) at native 48 kHz: p232_021, p257_023, p232_003, from https://datashare.ed.ac.uk/handle/10283/2791 (download of the first entry of DS_10283_2791.zip, 154 MB).
