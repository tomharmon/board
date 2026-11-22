mod access_cli;
mod attachments;
mod automation_cli;
mod cli;
mod conformance;
mod init;
mod roles_cli;
mod skill_install;
mod swarm_cli;
mod sync_cli;

use anyhow::{Context, Result, bail, ensure};
use board_local::errors::{Kind, classify};
use board_local::swarm::WorkerBinding;
use board_local::{Board, Filter, Message, PostOptions, local};
use clap::Parser;
use cli::{Args, Body, ChannelCommand, Command, Query, SchemaCommand, WriteOptions};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command as Process, ExitCode},
    thread,
    time::{Duration, Instant},
};

type Labels = BTreeMap<String, String>;

fn pairs(items: &[String], bare: bool) -> Result<Labels> {
    let mut out = Labels::new();
    for item in items {
        let (key, value) = match item.split_once('=') {
            Some((k, v)) => (k, v),
            None if bare => (item.as_str(), ""),
            _ => bail!("expected key=value, got {item:?}"),
        };
        ensure!(
            !key.is_empty() && out.insert(key.into(), value.into()).is_none(),
            "empty or repeated label key: {key:?}"
        );
    }
    Ok(out)
}

fn message_id(target: &str) -> Option<i64> {
    parse_id(target).ok()
}

fn parse_id(raw: &str) -> Result<i64, String> {
    raw.strip_prefix('#')
        .unwrap_or(raw)
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| "expected a positive message ID".into())
}

fn load_text(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }
}

impl Body {
    fn read(self) -> Result<String> {
        let raw = match (self.text.as_deref(), self.file) {
            (_, Some(path)) => load_text(&path)?,
            (Some("-"), _) => load_text(Path::new("-"))?,
            (text, _) => text.unwrap_or("").to_owned(),
        };
        if let Some(schema) = self.schema {
            schema_id(&schema)?;
            let data: Value = serde_json::from_str(&raw).context("invalid JSON for block")?;
            Ok(format!("```json#{schema}\n{data}\n```"))
        } else {
            Ok(raw)
        }
    }
}

impl WriteOptions {
    fn prepare(self, board: &Board, actor: &str, target: &str) -> Result<(String, PostOptions)> {
        let key = automation_cli::output_key(board, actor, self.key.as_deref())?;
        let text = self.body.read()?;
        let labels = pairs(&self.label, false)?;
        // A retried, committed post must not upload a second file, even if the local path is gone.
        // The actual post still verifies the target, writer and current permissions.
        let attachments = if let Some(key) = &key
            && (!self.attach.is_empty() || !self.attachment_id.is_empty())
            && match board.posted(actor, key) {
                Ok(_) => true,
                Err(e) if classify(&e).0 == Kind::Missing => false,
                Err(e) => return Err(e),
            } {
            PostOptions::default()
        } else {
            attachments::placements(board, actor, target, &self.attach, &self.attachment_id)?
        };
        Ok((
            text,
            PostOptions {
                no_sync: self.no_sync,
                labels,
                key,
                after: self.after,
                ..attachments
            },
        ))
    }
}

impl Query {
    fn filter(self, channel: Option<String>, actor: &str) -> Result<Filter> {
        ensure!(
            self.after.is_none_or(|a| a >= 0),
            "cursor must be nonnegative"
        );
        let mut f = Filter {
            channel,
            from: self.from,
            by: self.by,
            text: self.text,
            re: self.re,
            after: self.after,
            mentions: self.mentions,
            labels: Default::default(),
            label_terms: board_local::LabelTerm::parse(&self.label)?,
        };
        f.bind_actor(actor);
        Ok(f)
    }
}

fn schema_id(s: &str) -> Result<(&str, u32)> {
    let (name, v) = s.rsplit_once('@').context("expected schema name@version")?;
    ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "invalid schema name"
    );
    Ok((
        name,
        v.parse().context("expected an integer schema version")?,
    ))
}

fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() && !matches!(c, '\n' | '\t') {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn show_message(m: &Message) {
    let mut badges = String::new();
    for (k, v) in &m.labels {
        badges.push_str(&format!(" [{k}={v}]"));
    }
    println!(
        "#{}  {}  {}  {}{}",
        m.id,
        safe(&m.channel),
        safe(&m.actor),
        safe(&m.created_at),
        safe(&badges)
    );
    println!(
        "    {}",
        if m.deleted {
            "[deleted]".to_owned()
        } else {
            safe(&m.text).replace('\n', "\n    ")
        }
    );
    for (name, actors) in &m.reactions {
        println!("    :{}: {}", safe(name), safe(&actors.join(", ")));
    }
    for link in &m.links {
        if let Some(quoted) = &link.text {
            println!(
                "    > {}: {}\n      {}",
                safe(link.author.as_deref().unwrap_or("Source")),
                safe(quoted).replace('\n', "\n    > "),
                safe(&link.url)
            );
        }
    }
    for file in &m.attachments {
        println!(
            "    file #{}  {} ({} bytes)",
            file.id,
            safe(&file.name),
            file.size
        );
    }
    for file in &m.file_notices {
        println!(
            "    file not downloaded: {} ({})",
            safe(&file.name),
            safe(&file.reason)
        );
    }
    if let Some(t) = &m.task {
        println!("    task: {}", safe(&automation_cli::summary(t)));
    }
}

fn written(m: Message, outcome: &str, json_output: bool) {
    if json_output {
        println!("{}", json!({"message":m,"outcome":outcome}));
    } else if outcome == "posted" {
        println!("{}", m.id);
    } else {
        println!("{outcome} {}", m.id);
    }
}

enum Follow {
    Tail { last: Option<usize> },
    Wait { timeout: Duration },
}

fn resume_hint(cursor: i64) -> String {
    let fallback = || format!("resume with --after {cursor}");
    let mut words = vec!["board".to_string()];
    let mut args = env::args_os().skip(1);
    let mut positional = false;
    let mut appended = false;
    while let Some(raw) = args.next() {
        let Ok(arg) = raw.into_string() else {
            return fallback();
        };
        if arg.chars().any(char::is_control) {
            return fallback();
        }
        if !positional && arg == "--after" {
            args.next();
            continue;
        }
        if !positional && arg.starts_with("--after=") {
            continue;
        }
        if !positional && arg == "--" {
            positional = true;
            words.extend(["--after".into(), cursor.to_string()]);
            appended = true;
        }
        words.push(arg);
    }
    if !appended {
        words.extend(["--after".into(), cursor.to_string()]);
    }
    shlex::try_join(words.iter().map(String::as_str))
        .map(|cmd| format!("next: {cmd}"))
        .unwrap_or_else(|_| fallback())
}

fn stream(
    board: &Board,
    target: Option<String>,
    q: Query,
    actor: &str,
    follow: Follow,
    all: bool,
    js: bool,
) -> Result<()> {
    let (timeout, last) = match follow {
        Follow::Wait { timeout } => (Some(timeout), None),
        Follow::Tail { last } => (None, last),
    };
    let id = target.as_deref().and_then(message_id);
    let mut f = q.filter(target.filter(|_| id.is_none()), actor)?;
    if let Some(id) = id {
        ensure!(f.re.is_none(), "specify a thread ID or --re, not both");
        let m = board.get(id)?;
        f.re = Some(m.re.unwrap_or(id));
    }
    let mut cursor = f.after.take().or(id).unwrap_or(board.cursor()?);
    let output = |ev: board_local::Event| -> Result<()> {
        if js {
            if timeout.is_some() {
                println!("{}", json!({"event":ev}));
            } else {
                println!("{}", serde_json::to_string(&ev)?);
            }
        } else {
            println!("[{}] {} by {}", ev.cursor, safe(&ev.kind), safe(&ev.actor));
            if let Some(m) = ev.message {
                show_message(&m);
            }
            if timeout.is_some() {
                println!("{}", resume_hint(ev.cursor));
            }
        }
        io::stdout().flush()?;
        Ok(())
    };
    if let Some(last) = last {
        let (head, rows) = board.tail_feed(last, &f, all)?;
        for event in rows {
            output(event)?;
        }
        cursor = head;
    }
    let started = Instant::now();
    loop {
        let (pos, batch) = board.event_feed(
            cursor,
            if timeout.is_some() { 1 } else { 200 },
            &f,
            None,
            all,
        )?;
        cursor = pos;
        let len = batch.len();
        for ev in batch {
            output(ev)?;
            if timeout.is_some() {
                return Ok(());
            }
        }
        if timeout.is_some_and(|t| started.elapsed() >= t) {
            bail!(Kind::Timeout.error("timeout", "no matching event"));
        }
        if len < 200 {
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn inspector(
    db: PathBuf,
    profile: Option<String>,
    avatar: Option<String>,
    thread: Option<i64>,
) -> Result<()> {
    let beside = env::current_exe()?.with_file_name(format!("board-ui{}", env::consts::EXE_SUFFIX));
    let mut cmd = Process::new(if beside.is_file() {
        beside
    } else {
        PathBuf::from("board-ui")
    });
    cmd.env("BOARD_DB", db);
    if let Some(id) = thread {
        cmd.arg("--thread").arg(id.to_string());
    }
    if let Some(p) = profile {
        cmd.env("BOARD_PROFILE", p);
    }
    if let Some(url) = avatar {
        cmd.env("BOARD_AVATAR", url);
    }
    let status = cmd
        .status()
        .context("could not start board-ui; reinstall the Board desktop package")?;
    ensure!(status.success(), "inspector exited with {status}");
    Ok(())
}

fn accepts_worker(cmd: &Command) -> bool {
    use Command::*;
    matches!(
        cmd,
        Swarm { .. }
            | Mcp
            | Whoami
            | Discovery
            | Commands
            | Typing { .. }
            | Channels { .. }
            | Channel(_)
            | Actors { .. }
            | Profile { .. }
            | Post { .. }
            | Posted { .. }
            | Read { .. }
            | Reply { .. }
            | Edit { .. }
            | Delete { .. }
            | React { .. }
            | Label { .. }
            | Download { .. }
            | Upload { .. }
            | Tail { .. }
            | Wait { .. }
            | Automation(_)
            | Roles {
                command: roles_cli::Roles::List | roles_cli::Roles::Show { .. }
            }
            | Skills {
                command: roles_cli::Skills::List | roles_cli::Skills::Show { .. }
            }
            | Schemas {
                cmd: None | Some(SchemaCommand::List | SchemaCommand::Inspect { .. })
            }
    )
}
fn run(mut args: Args) -> Result<()> {
    if args.worker.is_some() && !accepts_worker(&args.cmd) {
        return Err(WorkerBinding::coordinator_only());
    }
    if let Command::Init(opts) = args.cmd {
        return init::run(opts, args.json);
    }
    if let Command::Skill { command } = args.cmd {
        return skill_install::run(command, args.json);
    }
    if matches!(args.cmd, Command::Conformance) {
        return conformance::run(args.json);
    }
    if matches!(args.cmd, Command::Commands) {
        use clap::CommandFactory;
        fn describe(c: &clap::Command) -> Value {
            json!({"name":c.get_name(),"about":c.get_about().map(ToString::to_string),"arguments":c.get_arguments().map(|a|json!({"id":a.get_id().as_str(),"long":a.get_long(),"required":a.is_required_set(),"help":a.get_help().map(ToString::to_string)})).collect::<Vec<_>>(),"commands":c.get_subcommands().map(describe).collect::<Vec<_>>()})
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&describe(&Args::command()))?
        );
        return Ok(());
    }
    if let Some(n) = &args.name {
        ensure!(
            !n.trim().is_empty() && n.len() <= 256 && !n.chars().any(char::is_control),
            "invalid board name"
        );
    }
    let path = args
        .db
        .or_else(|| args.name.as_deref().map(local::named_database_path))
        .unwrap_or_else(local::database_path);
    let in_swarm = matches!(args.cmd, Command::Swarm { .. } | Command::Mcp)
        || args.worker.is_some()
        || env::var_os("BOARD_SWARM_RUN").is_some();
    if args.worker.is_none() {
        if args.profile.is_none() {
            args.profile = local::identity_env("BOARD_PROFILE")?;
        }
        args.avatar = args.avatar.or_else(|| env::var("BOARD_AVATAR").ok());
    }
    let assigned = args
        .worker
        .as_deref()
        .map(WorkerBinding::selector)
        .transpose()?
        .map(|(_, name)| name);
    ensure!(
        assigned.is_none() || args.profile.as_deref().is_none_or(|p| Some(p) == assigned),
        Kind::Forbidden.error("actor_mismatch", "--as conflicts with the assigned worker")
    );
    let profile = args
        .profile
        .as_deref()
        .or(assigned)
        .map(|name| {
            if in_swarm {
                local::agent_profile_name(name)
            } else {
                local::profile_name(name)
            }
        })
        .transpose()?;
    let editing_profile = matches!(args.cmd, Command::Profile { .. });
    ensure!(
        args.avatar.is_none() || profile.is_some() || editing_profile,
        Kind::Invalid.error(
            "invalid_profile",
            "--avatar requires --as (or the profile command)"
        )
    );
    if let Command::Inspect { thread } = args.cmd {
        return inspector(path, profile, args.avatar, thread);
    }
    let mut board = Board::open(&path).with_context(|| format!("open {}", path.display()))?;
    let fixed = board.bind_env()?;
    let binding = args
        .worker
        .as_deref()
        .map(|s| board.worker_binding(s))
        .transpose()?;
    let actor = if let Some(binding) = &binding {
        binding.actor.clone()
    } else if let Some(fixed) = fixed {
        ensure!(
            profile
                .as_ref()
                .is_none_or(|p| fixed.split_once('/').is_some_and(|(_, name)| name == p)),
            Kind::Forbidden.error("actor_mismatch", "credential actor cannot be overridden")
        );
        fixed
    } else {
        profile
            .map(|p| format!("{}/{p}", local::person_name()))
            .unwrap_or_else(local::person_name)
    };
    if !editing_profile && let Some(url) = args.avatar.as_deref() {
        board.set_write_avatar(&actor, url)?;
    }
    let js = args.json;
    match args.cmd {
        Command::Init(_) | Command::Skill { .. } => unreachable!(),
        Command::Swarm { mut command } => {
            command.select_run(binding.as_ref())?;
            swarm_cli::run(&board, &actor, command, js)?;
        }
        Command::Roles { command } => roles_cli::roles(&board, &actor, command, js)?,
        Command::Skills { command } => roles_cli::skills(&board, &actor, command, js)?,
        Command::Mcp => {
            return board_mcp::serve_stdio_bound(&board, &actor, args.worker.as_deref());
        }
        Command::Discovery => println!(
            "{}",
            serde_json::to_string_pretty(&board_api::discovery_document())?
        ),
        Command::Commands | Command::Conformance => unreachable!(),
        Command::Typing {
            id,
            status,
            clear,
            list,
        } => {
            if !list {
                board.set_typing(
                    id,
                    &actor,
                    (!clear).then_some(status.as_deref().unwrap_or("Typing")),
                    15,
                )?;
            }
            let active = board.typing(id)?;
            if js {
                println!("{}", json!({"typing":active}));
            } else {
                for t in active {
                    println!("{}  {}", safe(&t.actor), safe(&t.status));
                }
            }
        }
        Command::Sources { command } => sync_cli::sources(&board, &actor, command, js)?,
        Command::Sync { command } => sync_cli::sync(&board, &actor, command, js)?,
        Command::Inbox { command } => sync_cli::inbox(&board, &actor, command, js)?,
        Command::Grants { command } => access_cli::grants(&board, &actor, command, js)?,
        Command::Credentials { command } => access_cli::credentials(&board, &actor, command, js)?,
        Command::Serve { port } => {
            board.require_owner()?;
            return board_api::serve(board, actor, port);
        }
        Command::Backup { destination } => {
            board.backup(&destination)?;
            if js {
                println!("{}", json!({"backup":destination}));
            } else {
                println!("{}", safe(&destination.display().to_string()));
            }
        }
        Command::Migrate {
            command: cli::Migration::ExportSchedules,
        } => board_local::automation::export_legacy_schedules(&board, std::io::stdout().lock())?,
        Command::Profile { handle, name } => {
            board.set_actor_profile_with_avatar(&actor, &handle, &name, args.avatar.as_deref())?;
            if js {
                println!("{}", json!({"actor":actor,"handle":handle,"name":name}));
            } else {
                println!("{}  {}", safe(&handle), safe(&name));
            }
        }
        Command::Actors {
            details,
            query,
            ids,
        } => {
            let (cursor, rows) = board.actor_directory_page(query.as_deref(), &ids)?;
            if details {
                if js {
                    println!("{}", json!({"actors":rows,"cursor":cursor}));
                } else {
                    for a in rows {
                        println!(
                            "{}  {}  {}  {}",
                            safe(&a.id),
                            safe(&a.handle),
                            safe(&a.name),
                            a.kind
                        );
                    }
                }
                return Ok(());
            }
            let actors = rows.into_iter().map(|a| a.id).collect::<Vec<_>>();
            if js {
                println!("{}", json!({"actors":actors,"cursor":cursor}));
            } else {
                for a in actors {
                    println!("{}", safe(&a));
                }
            }
        }
        Command::Inspect { .. } => unreachable!(),
        Command::Whoami => {
            if js {
                let mut me = json!(board.viewer(&actor)?);
                me["database"] = json!(path);
                me["worker"] = json!(binding);
                println!("{me}");
            } else {
                println!("{}  {}", safe(&actor), safe(&path.display().to_string()));
            }
        }
        Command::Channel(input)
        | Command::Channels {
            command: Some(ChannelCommand::Create(input)),
            ..
        } => {
            let name = input.name;
            let mut body = if input.private {
                json!({"created":true,"grants":board.create_private_channel(&name,&actor)?})
            } else {
                json!({"created":board.create_channel_as(&name,&actor)?})
            };
            body["channel"] = json!(name);
            if js {
                println!("{body}");
            } else if input.private {
                println!("Created private channel {}", safe(&name));
            } else {
                println!("{}", safe(&name));
            }
        }
        Command::Channels {
            command: None,
            path_prefix,
            prefix,
            scope,
            after,
            limit,
        } => {
            let (cursor, channels, next) = board.channels_window(
                after.as_deref().unwrap_or(""),
                scope.as_deref(),
                path_prefix.as_deref().or(prefix.as_deref()),
                limit,
            )?;
            if js {
                println!(
                    "{}",
                    json!({"channels":channels,"cursor":cursor,"next_after":next})
                );
            } else {
                for c in channels {
                    println!(
                        "{}  {} threads  {} messages",
                        safe(&c.name),
                        c.threads,
                        c.messages
                    );
                }
                if let Some(after) = next {
                    println!("more channels: --after {}", safe(&after));
                }
            }
        }
        Command::Post {
            target,
            options,
            sync_targets,
        } => {
            let id = message_id(&target);
            ensure!(
                id.is_none() || sync_targets.is_empty(),
                "--sync is for new threads, not replies"
            );
            let (text, o) = options.prepare(&board, &actor, &target)?;
            let m = if let Some(id) = id {
                board.reply(id, &actor, &text, &o)?
            } else {
                board
                    .post_with_targets(&target, &actor, &text, &o, &sync_targets)?
                    .0
            };
            written(m, "posted", js);
        }
        Command::Posted { key } => {
            let m = board.posted(&actor, &key)?;
            if js {
                println!("{}", json!({"message": m}));
            } else {
                println!("{}", m.id);
            }
        }
        Command::Reply { id, options } => {
            let (text, o) = options.prepare(&board, &actor, &id.to_string())?;
            written(board.reply(id, &actor, &text, &o)?, "posted", js);
        }
        Command::Edit {
            id,
            body,
            if_revision,
        } => written(
            board.edit(id, &actor, &body.read()?, if_revision)?,
            "edited",
            js,
        ),
        Command::Delete { id, if_revision } => {
            written(board.delete(id, &actor, if_revision)?, "deleted", js)
        }
        Command::React { id, emoji, remove } => written(
            board.react(id, &actor, &emoji, remove)?,
            if remove { "unreacted" } else { "reacted" },
            js,
        ),
        Command::Label {
            id,
            labels,
            unset,
            conditions,
            if_unset,
        } => {
            let mut cond: BTreeMap<_, _> = pairs(&conditions, false)?
                .into_iter()
                .map(|(k, v)| (k, Some(v)))
                .collect();
            for key in if_unset {
                ensure!(cond.insert(key, None).is_none(), "conflicting conditions");
            }
            ensure!(
                !labels.is_empty() || !unset.is_empty(),
                "specify labels to set or --unset"
            );
            written(
                board.labels(id, &actor, &pairs(&labels, false)?, &unset, &cond)?,
                "labeled",
                js,
            );
        }
        Command::Upload {
            channel,
            file,
            content_type,
        } => {
            let uploaded = attachments::upload(&board, &actor, &channel, &file, &content_type)?;
            if js {
                println!("{}", json!({"attachment":uploaded}));
            } else {
                println!("{}", uploaded.key);
            }
        }
        Command::Download { id, path } => {
            let (file, bytes) = board.download_as(&actor, board.file_id(&actor, &id)?)?;
            let dest = match path {
                Some(p) => p,
                None => Path::new(&file.name)
                    .file_name()
                    .context("invalid attachment filename")?
                    .into(),
            };
            if dest == Path::new("-") {
                ensure!(!js, "cannot combine --json with binary stdout");
                io::stdout().lock().write_all(&bytes)?;
            } else {
                let mut f = fs::File::create_new(&dest)
                    .with_context(|| format!("create {} (will not overwrite)", dest.display()))?;
                f.write_all(&bytes)?;
                if js {
                    println!("{}", json!({"file":file,"path":dest}));
                } else {
                    println!("{}", safe(&dest.display().to_string()));
                }
            }
        }
        Command::Read {
            target,
            query,
            limit,
            flat,
            before,
        } => {
            ensure!(
                limit.is_none_or(|n| (1..=500).contains(&n)),
                "--limit must be 1 to 500"
            );
            let id = match target.as_deref() {
                Some(url)
                    if url.split_once("://").is_some_and(|(scheme, _)| {
                        scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
                    }) =>
                {
                    Some(board.locate_link(url)?.message)
                }
                other => other.and_then(message_id),
            };
            let filter = query.filter(target.filter(|_| id.is_none()), &actor)?;
            if let Some(id) = id {
                ensure!(
                    before.is_none() && !flat,
                    "--before and --flat apply to lists, not whole threads"
                );
                ensure!(
                    filter.by.is_none()
                        && filter.text.is_none()
                        && filter.mentions.is_none()
                        && filter.from.is_none()
                        && filter.re.is_none(),
                    "a thread read accepts --after, --label and --limit; use a channel read to search for threads"
                );
                let thread = board.conversation_window(id, &filter, limit)?;
                if js {
                    println!("{}", serde_json::to_string(&thread)?);
                } else {
                    show_message(&thread.message);
                    for m in &thread.replies {
                        show_message(m);
                    }
                    println!("cursor {}", thread.cursor);
                }
            } else if flat {
                let p = board.messages_window(&filter, before, limit.unwrap_or(50))?;
                if js {
                    println!(
                        "{}",
                        json!({"messages":p.rows,"cursor":p.cursor,"snapshot":p.snapshot,"older":p.older})
                    );
                } else {
                    for m in p.rows {
                        show_message(&m);
                    }
                    println!("cursor {}", p.cursor);
                    if let Some(older) = p.older {
                        println!("older: --before {older}");
                    }
                }
            } else {
                let p = board.threads_window(&filter, before, limit.unwrap_or(50))?;
                if js {
                    println!(
                        "{}",
                        json!({"threads":p.rows,"cursor":p.cursor,"snapshot":p.snapshot,"older":p.older})
                    );
                } else {
                    for t in p.rows {
                        show_message(&t.message);
                        if let Some(m) = t.last {
                            println!(
                                "    {} replies, latest #{} {}: {}",
                                t.replies,
                                m.id,
                                safe(&m.actor),
                                safe(m.text.lines().next().unwrap_or(""))
                            );
                        } else {
                            println!("    no replies");
                        }
                    }
                    println!("cursor {}", p.cursor);
                    if let Some(older) = p.older {
                        println!("older: --before {older}");
                    }
                }
            }
        }
        Command::Tail {
            target,
            query,
            last,
            all,
        } => stream(
            &board,
            target,
            query,
            &actor,
            Follow::Tail { last },
            all,
            js,
        )?,
        Command::Wait {
            target,
            query,
            timeout,
            all,
        } => {
            let dur = timeout
                .parse::<u64>()
                .map(Duration::from_secs)
                .or_else(|_| humantime::parse_duration(&timeout))?;
            stream(
                &board,
                target,
                query,
                &actor,
                Follow::Wait { timeout: dur },
                all,
                js,
            )?;
        }
        Command::Schemas { cmd } => match cmd.unwrap_or(SchemaCommand::List) {
            SchemaCommand::List => {
                let schemas = board.schemas()?;
                if js {
                    println!("{}", json!({"schemas":schemas}));
                } else {
                    for s in schemas {
                        println!("{}@{}  {}", safe(&s.name), s.version, safe(&s.description));
                    }
                }
            }
            SchemaCommand::Inspect { schema } => {
                let (n, v) = schema_id(&schema)?;
                let s = board.schema(n, v)?;
                println!("{}", serde_json::to_string_pretty(&s)?);
            }
            SchemaCommand::Create {
                schema,
                file,
                data,
                description,
            } => {
                let (n, v) = schema_id(&schema)?;
                let raw = match file {
                    Some(f) => load_text(&f)?,
                    None => data.context("provide --file or --data")?,
                };
                let created = board.register_schema_as(
                    n,
                    v,
                    &description,
                    &serde_json::from_str(&raw)?,
                    &actor,
                )?;
                if js {
                    println!("{}", json!({"schema":schema,"created":created}));
                } else {
                    println!("{schema}");
                }
            }
        },
        Command::Automation(cmd) => cmd.run(&mut board, &actor, js, &path, binding.as_ref())?,
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) if e.use_stderr() && env::args_os().any(|a| a == "--json") => {
            eprintln!(
                "{}",
                json!({"status":"error","code":"invalid_arguments","message":e.to_string()})
            );
            return ExitCode::from(2);
        }
        Err(e) => e.exit(),
    };
    let js = args.json;
    if let Err(e) = run(args) {
        if let Some(code) = e.downcast_ref::<automation_cli::RunExit>() {
            return ExitCode::from(code.0);
        }
        let (kind, code) = classify(&e);
        if js {
            eprintln!(
                "{}",
                json!({"status":"error","code":code,"message":format!("{e:#}")})
            );
        } else {
            eprintln!("error: {}", safe(&format!("{e:#}")));
        }
        ExitCode::from(if kind == Kind::Invalid { 2 } else { 1 })
    } else {
        ExitCode::SUCCESS
    }
}
