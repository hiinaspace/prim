//! Advanced view: the live room as an inspectable tree, like a scene tree or a DOM
//! inspector. Read-only. Everything shown comes from the daemon's status and from the
//! `**` subscription (so it is exactly what this daemon knows).

use std::time::Duration;

use eframe::egui::{self, Color32, RichText, Ui};

use crate::app::Snapshot;
use crate::chat::{self, CHAT_CHANNEL};
use crate::client::{short, KeyEntry};

const DIM: Color32 = Color32::from_rgb(138, 146, 160);
const GREEN: Color32 = Color32::from_rgb(88, 200, 130);
const AMBER: Color32 = Color32::from_rgb(232, 178, 70);

pub fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0 => format!("{} ms", d.as_millis()),
        1..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

/// A short human form of a state value, by well-known key name.
pub fn fmt_value(path: &str, v: &[u8]) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "profile" => chat::decode_profile(v).map(|n| format!("name = {n:?}")),
        "voice" => chat::decode_voice(v).map(|(l, m)| {
            let inc = v.get(2..6).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
            format!("listening={l} mic={m} incarnation={inc}")
        }),
        "presence" => (v.len() >= 10).then(|| {
            let since = u64::from_le_bytes(v[2..10].try_into().unwrap_or([0; 8]));
            format!("online={} since_ms={since}", v[1] == 1)
        }),
        _ => None,
    }
    .or_else(|| {
        std::str::from_utf8(v)
            .ok()
            .filter(|s| !s.is_empty() && s.chars().all(|c| !c.is_control()))
            .map(str::to_string)
    })
    .unwrap_or_else(|| {
        if v.is_empty() {
            "(empty)".into()
        } else {
            let hex: String = v.iter().take(24).map(|b| format!("{b:02x}")).collect();
            format!("{hex}{} ({} B)", if v.len() > 24 { "…" } else { "" }, v.len())
        }
    })
}

fn mono(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).monospace().size(12.0)
}

fn keys_table(ui: &mut Ui, id: &str, rows: &[&(String, KeyEntry)], strip: &str, s: &Snapshot) {
    if rows.is_empty() {
        ui.label(RichText::new("no keys").color(DIM));
        return;
    }
    egui::Grid::new(id).striped(true).num_columns(6).spacing([14.0, 3.0]).show(ui, |ui| {
        for h in ["key", "value", "class", "writer", "version", "age"] {
            ui.label(RichText::new(h).color(DIM).size(11.5));
        }
        ui.end_row();
        for (path, e) in rows {
            let key = path.strip_prefix(strip).unwrap_or(path);
            ui.label(mono(key));
            ui.label(mono(fmt_value(path, &e.value)));
            ui.label(mono(&e.class));
            ui.label(mono(s.name(&e.writer))).on_hover_text(&e.writer);
            ui.label(mono(format!("{}.{}.{}", e.version.generation, e.version.term, e.version.seq)));
            ui.label(mono(fmt_age(e.changed.elapsed())));
            ui.end_row();
        }
    });
}

pub(crate) fn show(ui: &mut Ui, s: &Snapshot) {
    let Some(st) = &s.status else { return };
    ui.label(RichText::new("Advanced view").strong().size(15.0));
    ui.label(
        RichText::new("The live room as this computer sees it. Read-only. \"age\" is time since this computer last saw the value change.")
            .color(DIM)
            .size(12.0),
    );
    ui.add_space(4.0);
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let room_keys: Vec<_> = s.keys.iter().filter(|(p, _)| p.starts_with("room/")).collect();
        egui::CollapsingHeader::new(RichText::new(format!("Room  {}", short(&s.room))).strong())
            .id_salt("room")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(mono(format!(
                    "lifecycle: {}  generation: {}",
                    s.lifecycle,
                    s.generation.map(|g| g.to_string()).unwrap_or("-".into())
                )));
                match &st.coordinator {
                    Some(c) => ui.label(mono(format!(
                        "coordinator: {} ({})  term {}.{}",
                        s.name(&c.holder),
                        short(&c.holder),
                        c.generation,
                        c.term
                    ))),
                    None => ui.label(mono("coordinator: none")),
                };
                ui.label(mono(format!(
                    "grant records: {}   durable bytes: room {} / mine {}",
                    st.records, st.durable_bytes_room, st.durable_bytes_mine
                )));
                egui::CollapsingHeader::new(mono(format!("room/* keys ({})", room_keys.len())))
                    .id_salt("roomkeys")
                    .default_open(true)
                    .show(ui, |ui| keys_table(ui, "roomkeys_t", &room_keys, "room/", s));
            });
        egui::CollapsingHeader::new(RichText::new(format!("Members ({})", s.members.len())).strong())
            .id_salt("members")
            .default_open(true)
            .show(ui, |ui| {
                for (i, m) in s.members.iter().enumerate() {
                    let sm = st.members.iter().find(|x| x.key == m.key);
                    let title = format!("{}{}  {}", m.name, if m.is_me { " (you)" } else { "" }, short(&m.key));
                    egui::CollapsingHeader::new(mono(title))
                        .id_salt(("member", i))
                        .default_open(m.is_me)
                        .show(ui, |ui| {
                            if let Some(sm) = sm {
                                ui.label(mono(format!(
                                    "keyholder={} inviter={} connected={} online={} fresh={}",
                                    sm.keyholder, sm.inviter, sm.connected, sm.online, sm.fresh
                                )));
                                if let Some(ms) = sm.since_ms {
                                    ui.label(mono(format!("present since unix ms {ms}")));
                                }
                            }
                            let prefix = format!("member/{}/", m.key);
                            let rows: Vec<_> = s.keys.iter().filter(|(p, _)| p.starts_with(&prefix)).collect();
                            egui::CollapsingHeader::new(mono(format!("state keys ({})", rows.len())))
                                .id_salt(("mkeys", i))
                                .default_open(true)
                                .show(ui, |ui| keys_table(ui, &format!("mk{i}"), &rows, &prefix, s));
                            if let Some(r) = st.routes.iter().find(|r| r.peer == m.key) {
                                ui.label(mono(format!(
                                    "route: {} rtt={} max_datagram={}",
                                    r.path,
                                    r.rtt_ms.map(|v| format!("{v} ms")).unwrap_or("-".into()),
                                    r.max_datagram_size.map(|v| v.to_string()).unwrap_or("-".into())
                                )));
                            }
                        });
                }
            });
        egui::CollapsingHeader::new(RichText::new("Channels").strong())
            .id_salt("channels")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(mono(format!("{CHAT_CHANNEL}  room chat  (registered by this window)")));
                ui.label(
                    RichText::new("Channels other programs registered are not visible here: the daemon status does not list them.")
                        .color(DIM)
                        .size(11.5),
                );
            });
        egui::CollapsingHeader::new(RichText::new("Leases").strong())
            .id_salt("leases")
            .default_open(true)
            .show(ui, |ui| match st.rt.as_ref().map(|r| &r.holders) {
                None => {
                    ui.label(RichText::new("this daemon runs no realtime lane").color(DIM));
                }
                Some(h) if h.is_empty() => {
                    ui.label(mono("none held (no voice client, no tracking source)"));
                }
                Some(h) => {
                    for l in h {
                        ui.label(mono(format!("{}  held by session {}  incarnation {}", l.role, l.session, l.incarnation)));
                    }
                }
            });
        egui::CollapsingHeader::new(RichText::new(format!("Connections ({})", st.routes.len())).strong())
            .id_salt("routes")
            .default_open(true)
            .show(ui, |ui| {
                if st.routes.is_empty() {
                    ui.label(RichText::new("no live peer connections").color(DIM));
                }
                for (i, r) in st.routes.iter().enumerate() {
                    let col = if r.path == "direct" { GREEN } else { AMBER };
                    egui::CollapsingHeader::new(
                        mono(format!(
                            "{}  {}  {}",
                            s.name(&r.peer),
                            r.path,
                            r.rtt_ms.map(|v| format!("{v} ms")).unwrap_or_default()
                        ))
                        .color(col),
                    )
                    .id_salt(("route", i))
                    .show(ui, |ui| {
                        ui.label(mono(format!("dialled by: {}", if r.outbound { "us" } else { "them" })));
                        ui.label(mono(format!("datagrams sent: {}", r.datagrams_sent)));
                        ui.label(mono(format!(
                            "dropped: age {}  queue {}  transport {}  too_large {}  inbound {}",
                            r.datagrams_dropped_age,
                            r.datagrams_dropped_queue,
                            r.datagrams_dropped_transport,
                            r.datagrams_too_large,
                            r.datagrams_dropped_in
                        )));
                    });
                }
            });
        if let Some(rt) = &st.rt {
            egui::CollapsingHeader::new(RichText::new("Realtime lane (voice)").strong())
                .id_salt("rt")
                .show(ui, |ui| {
                    ui.label(mono(format!(
                        "sent {}  received {}  queue {}  stalls {}",
                        rt.sent, rt.received, rt.queue_depth, rt.stalls
                    )));
                    let drops = serde_json::to_value(&rt.drops).unwrap_or_default();
                    let nz: Vec<String> = drops
                        .as_object()
                        .map(|o| {
                            o.iter()
                                .filter(|(_, v)| v.as_u64().unwrap_or(0) > 0)
                                .map(|(k, v)| format!("{k}={v}"))
                                .collect()
                        })
                        .unwrap_or_default();
                    ui.label(mono(if nz.is_empty() { "drops: none".to_string() } else { format!("drops: {}", nz.join("  ")) }));
                });
        }
    });
}
