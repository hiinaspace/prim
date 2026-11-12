//! `prim ctl <subcommand>`: a thin client of the daemon's local socket.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use prim_room::local::{self, b64, unb64, Client, Event, ReplyJson, Request, UpdateJson};

use crate::Args;

const HELP: &str = "\
prim ctl [--socket PATH] <subcommand>

  status [--json]                      room, members, per-peer route (path, datagram size)
  invite [--expiry 1h]                 print an invite token
  grant keyholder|inviter|member KEY [--expiry 1h]
  revoke KEY | retire KEY [--keep-all]
  close | reopen | leave
  write SCOPE NAME VALUE [--live] [--persistent]   scope: member or room
  sub PATTERN [--count N] [--timeout SECS]         print updates
  send TO CHANNEL TEXT [--reliable]                TO: all, coordinator or a member key
  listen CHANNEL [--count N] [--timeout SECS]      register the channel, print messages
";

pub async fn run(args: Vec<String>) -> Result<ExitCode> {
    let args = Args::parse(args, &["socket", "expiry", "count", "timeout"])?;
    if args.has("help") || args.positional.is_empty() {
        eprint!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    let socket = args
        .value("socket")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PRIM_SOCKET").map(PathBuf::from))
        .unwrap_or_else(local::default_socket_path);
    let mut c = Client::connect(&socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let p = &args.positional;
    let arg = |i: usize, what: &str| -> Result<String> {
        p.get(i).cloned().with_context(|| format!("missing {what}"))
    };
    match p[0].as_str() {
        "status" => {
            let r = c.call(|req| Request::Status { req }).await?;
            match r {
                ReplyJson::Status { status } => {
                    if args.has("json") {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        print_status(&status);
                    }
                    Ok(ExitCode::SUCCESS)
                }
                other => reply_result(other),
            }
        }
        "invite" => {
            let expiry_in_ms = expiry(&args)?;
            let r = c
                .call(|req| Request::CreateInvite {
                    req,
                    expiry_in_ms,
                    relays: vec![],
                })
                .await?;
            match r {
                ReplyJson::Invite { token } => {
                    println!("{token}");
                    Ok(ExitCode::SUCCESS)
                }
                other => reply_result(other),
            }
        }
        "grant" => {
            let kind = arg(1, "kind (keyholder, inviter, member)")?;
            let subject = arg(2, "member key")?;
            let expiry_in_ms = expiry(&args)?;
            let r = c
                .call(|req| Request::Grant {
                    req,
                    kind,
                    subject,
                    expiry_in_ms,
                })
                .await?;
            reply_result(r)
        }
        "revoke" => {
            let subject = arg(1, "member key")?;
            reply_result(c.call(|req| Request::Revoke { req, subject }).await?)
        }
        "retire" => {
            let subject = arg(1, "member key")?;
            let keep_all = args.has("keep-all");
            reply_result(
                c.call(|req| Request::Retire {
                    req,
                    subject,
                    keep_all,
                })
                .await?,
            )
        }
        "close" => reply_result(c.call(|req| Request::Close { req }).await?),
        "reopen" => reply_result(c.call(|req| Request::Reopen { req }).await?),
        "leave" => reply_result(c.call(|req| Request::Leave { req }).await?),
        "write" => {
            let scope = arg(1, "scope")?;
            let name = arg(2, "name")?;
            let value = arg(3, "value")?;
            let (live, persistent) = (args.has("live"), args.has("persistent"));
            reply_result(
                c.call(|req| Request::Write {
                    req,
                    scope,
                    name,
                    live,
                    persistent,
                    value: b64(value.as_bytes()),
                })
                .await?,
            )
        }
        "sub" => {
            let pattern = arg(1, "pattern")?;
            let r = c
                .call(|req| Request::Subscribe {
                    req,
                    sub: 1,
                    pattern,
                })
                .await?;
            if !matches!(r, ReplyJson::Ok) {
                return reply_result(r);
            }
            let want = count(&args)?;
            let mut seen = 0usize;
            stream(&mut c, &args, |ev| {
                if let Event::Updates { updates, .. } = ev {
                    for u in updates {
                        print_update(u);
                        if matches!(u, UpdateJson::Value { .. }) {
                            seen += 1;
                        }
                    }
                }
                want.is_some_and(|n| seen >= n)
            })
            .await?;
            Ok(if want.is_some_and(|n| seen < n) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        "send" => {
            let to = arg(1, "target")?;
            let channel: u16 = arg(2, "channel")?.parse().context("channel")?;
            let text = arg(3, "text")?;
            let reliable = args.has("reliable");
            let r = c
                .call(|req| Request::Send {
                    req,
                    to,
                    channel,
                    reliable,
                    payload: b64(text.as_bytes()),
                })
                .await?;
            let (msg_id, targets) = match r {
                ReplyJson::Sending { msg_id, targets } => (msg_id, targets),
                other => return reply_result(other),
            };
            let mut pending = targets.len();
            let mut ok = true;
            let timeout = Duration::from_secs(
                args.value("timeout")
                    .map(str::parse)
                    .transpose()?
                    .unwrap_or(10),
            );
            let _ = tokio::time::timeout(timeout, async {
                while pending > 0 {
                    match c.next_event().await {
                        Ok(Some(Event::MessageResult {
                            msg_id: id,
                            to,
                            status,
                        })) if id == msg_id => {
                            println!("result to={to} status={status}");
                            ok &= status == "delivered";
                            pending -= 1;
                        }
                        Ok(Some(_)) => {}
                        _ => break,
                    }
                }
            })
            .await;
            Ok(if ok && pending == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        "dgram" => {
            // prim ctl dgram <to> <channel> <size> [--count N] [--max-age ms]
            let to = arg(1, "target")?;
            let channel: u16 = arg(2, "channel")?.parse().context("channel")?;
            let size: usize = arg(3, "size")?.parse().context("size")?;
            let n = count(&args)?.unwrap_or(1);
            let max_age_ms: u32 = args
                .value("max-age")
                .map(str::parse)
                .transpose()?
                .unwrap_or(2_000);
            for i in 0..n {
                let mut body = format!("dgram-{i}-").into_bytes();
                body.resize(size.max(body.len()), b'.');
                c.send(&Request::SendDatagram {
                    to: to.clone(),
                    channel,
                    payload: b64(&body),
                    max_age_ms,
                })
                .await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            println!("sent {n} datagrams of {size} B");
            Ok(ExitCode::SUCCESS)
        }
        "listen" => {
            let channel: u16 = arg(1, "channel")?.parse().context("channel")?;
            let r = c
                .call(|req| Request::RegisterChannel { req, channel })
                .await?;
            if !matches!(r, ReplyJson::Ok) {
                return reply_result(r);
            }
            let want = count(&args)?;
            let mut seen = 0usize;
            stream(&mut c, &args, |ev| {
                if let Event::Datagram {
                    from,
                    channel: ch,
                    payload,
                    age_ms,
                } = ev
                {
                    if *ch == channel {
                        println!(
                            "datagram from={from} channel={ch} bytes={} age_ms={age_ms}",
                            unb64(payload).map(|p| p.len()).unwrap_or(0)
                        );
                        seen += 1;
                    }
                }
                if let Event::Message {
                    from,
                    channel: ch,
                    payload,
                } = ev
                {
                    if *ch == channel {
                        println!(
                            "message from={from} channel={ch} text={}",
                            String::from_utf8_lossy(&unb64(payload).unwrap_or_default())
                        );
                        seen += 1;
                    }
                }
                want.is_some_and(|n| seen >= n)
            })
            .await?;
            Ok(if want.is_some_and(|n| seen < n) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        other => bail!("unknown subcommand {other:?}; try --help"),
    }
}

fn count(args: &Args) -> Result<Option<usize>> {
    args.value("count")
        .map(str::parse)
        .transpose()
        .context("--count")
}

/// Read events until `f` returns true, the timeout passes, or the socket closes.
async fn stream(c: &mut Client, args: &Args, mut f: impl FnMut(&Event) -> bool) -> Result<()> {
    let timeout = args.value("timeout").map(str::parse::<u64>).transpose()?;
    let run = async {
        while let Some(ev) = c.next_event().await? {
            if f(&ev) {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    match timeout {
        Some(s) => {
            let _ = tokio::time::timeout(Duration::from_secs(s), run).await;
            Ok(())
        }
        None => run.await,
    }
}

fn expiry(args: &Args) -> Result<u64> {
    match args.value("expiry") {
        None => Ok(0),
        Some(s) => parse_duration_ms(s),
    }
}

fn parse_duration_ms(s: &str) -> Result<u64> {
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = num.parse().with_context(|| format!("bad duration {s:?}"))?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => bail!("bad duration {s:?} (use s, m, h or d)"),
    };
    Ok(n * mult)
}

fn reply_result(r: ReplyJson) -> Result<ExitCode> {
    match r {
        ReplyJson::Ok => {
            println!("ok");
            Ok(ExitCode::SUCCESS)
        }
        ReplyJson::Error { code } => {
            eprintln!("error: {code}");
            Ok(ExitCode::FAILURE)
        }
        other => {
            println!("{}", serde_json::to_string(&other)?);
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn print_update(u: &UpdateJson) {
    match u {
        UpdateJson::Reset => println!("reset"),
        UpdateJson::Value {
            path,
            writer,
            version,
            class,
            value,
        } => println!(
            "value path={path} writer={} version={}.{}.{} class={class} text={}",
            &writer[..8.min(writer.len())],
            version.generation,
            version.term,
            version.seq,
            String::from_utf8_lossy(&unb64(value).unwrap_or_default())
        ),
        UpdateJson::Removed { path } => println!("removed path={path}"),
        UpdateJson::SnapshotDone => println!("snapshot_done"),
    }
}

fn print_status(s: &local::StatusJson) {
    println!(
        "room {} lifecycle={} generation={}",
        s.room.as_deref().unwrap_or("-"),
        s.lifecycle,
        s.generation
            .map(|g| g.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!("me {}", s.me);
    match &s.coordinator {
        Some(c) => println!("coordinator {} epoch={}.{}", c.holder, c.generation, c.term),
        None => println!("coordinator -"),
    }
    println!(
        "records {} durable_bytes_room={} durable_bytes_mine={}",
        s.records, s.durable_bytes_room, s.durable_bytes_mine
    );
    for m in &s.members {
        println!(
            "member {} keyholder={} inviter={} connected={} online={}",
            m.key, m.keyholder, m.inviter, m.connected, m.online
        );
    }
    for r in &s.routes {
        println!(
            "route peer={} path={} max_datagram_size={} rtt_ms={} outbound={} datagrams_sent={} dropped_age={} dropped_queue={} dropped_transport={} too_large={}",
            r.peer,
            r.path,
            r.max_datagram_size.map(|n| n.to_string()).unwrap_or_else(|| "none".into()),
            r.rtt_ms.map(|n| n.to_string()).unwrap_or_else(|| "-".into()),
            r.outbound,
            r.datagrams_sent,
            r.datagrams_dropped_age,
            r.datagrams_dropped_queue,
            r.datagrams_dropped_transport,
            r.datagrams_too_large,
        );
    }
    if let Some(rt) = &s.rt {
        let holders: Vec<String> = rt
            .holders
            .iter()
            .map(|h| {
                format!(
                    "{}:session={},incarnation={}",
                    h.role, h.session, h.incarnation
                )
            })
            .collect();
        println!(
            "rt holders=[{}] queue_depth={} sent={} datagrams_enqueued={} received={}",
            holders.join(" "),
            rt.queue_depth,
            rt.sent,
            rt.datagrams_enqueued,
            rt.received
        );
        let d = &rt.drops;
        println!(
            "rt drops no_lease={} not_member={} decode={} queue_full={} per_sender={} age={} age_media={} out_age={} wrong_incarnation={} channel_full={} bad_role={} viseme={} pose_replaced={} pose_dropped={}",
            d.no_lease, d.not_member, d.decode, d.queue_full, d.per_sender, d.age, d.age_media, d.out_age,
            d.wrong_incarnation, d.channel_full, d.bad_role, d.viseme, d.pose_replaced, d.pose_dropped
        );
        for (name, h) in [
            ("out_hop", &rt.out_hop),
            ("in_queue", &rt.in_queue),
            ("pose_hop", &rt.pose_hop),
        ] {
            println!(
                "rt {name} n={} p50_us={} p99_us={} p999_us={} max_us={}",
                h.count, h.p50_us, h.p99_us, h.p999_us, h.max_us
            );
        }
    }
}
