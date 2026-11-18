//! The stats JSON: written every second and at exit.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::clock::mono_us;
use crate::shared::Shared;

pub fn snapshot(sh: &Shared) -> Value {
    let (hists, counters, tones) = {
        let g = sh.stats.inner.lock().unwrap();
        (
            json!({
                "ipc_delay_us": g.ipc_delay_us.to_json(),
                "listener_age_us": g.listener_age_us.to_json(),
                "mixer_block_us": g.mixer_block_us.to_json(),
                "callback_gap_us": sh.stats.callback_gap_us.to_json(),
            }),
            g.counters.clone(),
            g.tone_seconds.iter().cloned().collect::<Vec<_>>(),
        )
    };
    let c = |n: &str| counters.get(n).copied().unwrap_or(0);

    let mut peers = serde_json::Map::new();
    {
        let list: Vec<_> = sh
            .senders
            .lock()
            .unwrap()
            .map
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (k, p) in list {
            let p = p.lock().unwrap();
            let rs = p.receiver.as_ref().map(|r| r.stats());
            let mut v = json!({
                "incarnation": p.incarnation,
                "packets_accepted": p.packets_accepted,
                "new_streams": p.new_streams,
                "stale_dropped": p.stale,
                "push_errors": p.push_errors,
                "blocks_pulled": p.blocks_pulled,
                "active": p.active,
            });
            if let Some(s) = rs {
                // Key names the R0 oracles read stay (`neteq_output_samples` is now the
                // receiver's output count); the rest are the timeline receiver's own.
                v["receiver"] = json!({
                    "output_samples": s.output_samples,
                    "neteq_output_samples": s.output_samples,
                    "concealed_samples": s.plc_samples,
                    "plc_samples": s.plc_samples,
                    "fec_samples": s.fec_samples,
                    "silence_samples": s.silence_samples,
                    "idle_samples": s.idle_samples,
                    "concealed_pct": s.concealed_pct(),
                    "packets_in": s.packets_in,
                    "packets_decoded": s.packets_decoded,
                    "late": s.late,
                    "late_packets_discarded": s.late,
                    "duplicate": s.duplicate,
                    "reordered_accepted": s.reordered_accepted,
                    "rejected": s.rejected,
                    "local_late": s.local_late,
                    "local_delay_max_ms": s.local_delay_max_ms,
                    "accelerate_samples": s.accelerate_samples,
                    "preemptive_samples": s.preemptive_samples,
                    "accelerate_ops": s.accelerate_ops,
                    "preemptive_ops": s.preemptive_ops,
                    "removed_samples_for_acceleration": s.accelerate_samples,
                    "inserted_samples_for_deceleration": s.preemptive_samples,
                    "talkspurts": s.talkspurts,
                    "talkspurt_flushes": s.target_resets,
                    "target_resets": s.target_resets,
                    "dtx_refreshes": s.dtx_refreshes,
                    "prebuffers": s.prebuffers,
                    "skipped_samples": s.skipped_samples,
                    "buffer_ms": s.buffer_ms,
                    "target_delay_ms": s.target_delay_ms,
                    "peak_delay_ms": s.peak_delay_ms,
                    "playout_timestamp": s.playout_timestamp,
                    "idle": s.idle,
                    "decode_errors": s.decode_errors,
                });
            }
            peers.insert(k.hex(), v);
        }
    }

    let mut doc = json!({
        "opus_version": voice_core::opus_version(),
        "self_key": sh.self_key.hex(),
        "incarnation": sh.incarnation,
        "device_generation": sh.gen.device_gen.load(Ordering::Relaxed),
        "uptime_s": (mono_us().saturating_sub(sh.start_us)) as f64 / 1e6,
        "histograms": hists,
        "peers": peers,
        "fence": { "new_stream": c("fence_new_stream"), "stale": c("fence_stale") },
        "drops": {
            "age": c("drop_age"),
            "stale_incarnation": c("fence_stale"),
            "rt_decode": c("drop_rt_decode"),
            "no_slot": c("drop_no_slot"),
            "push_error": c("drop_push_error"),
            "send_socket_full": c("drop_send_full"),
        },
        "clock_mismatch": c("clock_mismatch"),
        "underruns": sh.stats.underruns.load(Ordering::Relaxed),
        "sent": {
            "packets": c("sent_packets"),
            "bytes": c("sent_bytes"),
            "keepalives": c("keepalives"),
            "test_dropped": c("test_dropped"),
            "device_changes": c("device_changes"),
        },
        "device_stall_reopen": c("device_stall_reopen"),
        "device_open_failures": c("device_open_failures"),
        "voice_in": c("voice_in"),
    });
    if sh.cfg.stats_tone {
        doc["tone_seconds"] = Value::Array(tones);
    }
    doc
}

pub fn write(sh: &Shared, path: &Path) {
    let doc = serde_json::to_string_pretty(&snapshot(sh)).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        if f.write_all(doc.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}
