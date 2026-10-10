use crate::{
    Result,
    gateway::{Connection, Gateway, contexts, summary},
    policy::{Action, Approval},
    safety, themes, tui,
};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    time::Duration,
};

macro_rules! output {
    ($($arg:tt)*) => { write_text(&format!($($arg)*))? };
}
macro_rules! outputln {
    ($($arg:tt)*) => { write_text(&format!("{}\n", format_args!($($arg)*)))? };
}
fn write_text(text: &str) -> Result<()> {
    write_to(&mut io::stdout().lock(), text.as_bytes())
}
fn write_to(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    match writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(_) => Err("output failed".into()),
    }
}

#[derive(Clone)]
pub struct Options {
    pub connection: Connection,
    pub theme: Option<String>,
    pub color: bool,
    pub mouse: bool,
    pub json: bool,
    pub allow_tools: bool,
    pub allow_extensions: bool,
    pub splash: bool,
    pub poll: bool,
    pub yaml: bool,
    pub follow: bool,
    pub previous: bool,
    pub log_seconds: u32,
    pub interactive: bool,
    pub metrics_resources: bool,
    pub session_seconds: u16,
    pub command: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut opts = Options {
        connection: Connection::default(),
        theme: None,
        color: std::env::var_os("NO_COLOR").is_none(),
        mouse: true,
        json: false,
        allow_tools: false,
        allow_extensions: false,
        splash: std::env::var_os("KBORG_NO_SPLASH").is_none()
            && std::env::var_os("K8BORG_NO_SPLASH").is_none(),
        poll: false,
        yaml: false,
        follow: false,
        previous: false,
        log_seconds: crate::logs::DEFAULT_FOLLOW_SECONDS,
        interactive: false,
        metrics_resources: false,
        session_seconds: 900,
        command: vec![],
    };
    let mut args = args.into_iter();
    let mut config_path = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                config_path = Some(PathBuf::from(args.next().ok_or("--config needs a file")?))
            }
            "--yaml" => opts.yaml = true,
            "--follow" => opts.follow = true,
            "--previous" => opts.previous = true,
            "--log-seconds" => {
                opts.log_seconds = args
                    .next()
                    .ok_or("--log-seconds needs a value")?
                    .parse()
                    .map_err(|_| "invalid log duration")?;
                crate::logs::FollowOptions {
                    seconds: opts.log_seconds,
                }
                .validate()?;
            }
            "--interactive" => opts.interactive = true,
            "--metrics-resources" => opts.metrics_resources = true,
            "--session-seconds" => {
                opts.session_seconds = args
                    .next()
                    .ok_or("--session-seconds needs a value")?
                    .parse()
                    .map_err(|_| "invalid session duration")?;
                if !(1..=3600).contains(&opts.session_seconds) {
                    return Err("session duration must be 1..3600 seconds".into());
                }
            }
            "--allow-auth-helper" => opts.connection.allow_auth_helper = true,
            "--auth-helper-policy" => {
                opts.connection.auth_helper_policy = Some(PathBuf::from(
                    args.next().ok_or("--auth-helper-policy needs a file")?,
                ))
            }
            "--context" => {
                opts.connection.context = Some(args.next().ok_or("--context needs a value")?)
            }
            "--namespace" | "-n" => {
                opts.connection.namespace = args.next().ok_or("--namespace needs a value")?
            }
            "--kubeconfig" => {
                opts.connection.kubeconfig = Some(PathBuf::from(
                    args.next().ok_or("--kubeconfig needs a value")?,
                ))
            }
            "--theme" => opts.theme = Some(args.next().ok_or("--theme needs a value")?),
            "--write" => opts.connection.writable = true,
            "--readonly" => opts.connection.writable = false,
            "--allow-logs" => opts.connection.logs = true,
            "--allow-tools" => opts.allow_tools = true,
            "--allow-extensions" => opts.allow_extensions = true,
            "--tool" | "--tool-arg" => {
                let value = args.next().ok_or("--tool/--tool-arg needs a value")?;
                opts.command.extend([arg, value]);
            }
            "--allow-exec" => opts.connection.allow_exec = true,
            "--allow-attach" => opts.connection.allow_attach = true,
            "--allow-forward" => opts.connection.allow_port_forward = true,
            "--" => {
                opts.command.extend(args);
                break;
            }
            "--demo" => opts.connection.demo = true,
            "--json" => opts.json = true,
            "--no-splash" => opts.splash = false,
            "--poll" => opts.poll = true,
            "--no-mouse" => opts.mouse = false,
            "--color=never" => opts.color = false,
            "--color=always" => opts.color = true,
            "--help" | "-h" => {
                opts.command = vec!["help".into()];
                break;
            }
            "--version" | "-V" => {
                opts.command = vec!["version".into()];
                break;
            }
            _ => opts.command.push(arg),
        }
    }
    if opts.command.first().is_some_and(|c| c == "demo") {
        opts.connection.demo = true;
        opts.command.remove(0);
    }
    if !opts.command.first().is_some_and(|c| {
        matches!(
            c.as_str(),
            "theme" | "copilot" | "exec" | "view" | "auth-helper" | "extensions"
        )
    }) && opts.command.iter().any(|s| s.starts_with('-'))
    {
        return Err("unknown option; see --help".into());
    }
    if opts.yaml
        && (opts.json
            || opts
                .command
                .first()
                .is_none_or(|v| !matches!(v.as_str(), "get" | "view")))
    {
        return Err("--yaml requires get/view and cannot be combined with --json".into());
    }
    if opts.previous && opts.command.first().is_none_or(|v| v != "logs") {
        return Err("--previous requires logs".into());
    }
    if opts.follow
        && opts
            .command
            .first()
            .is_none_or(|v| !matches!(v.as_str(), "logs" | "overview"))
    {
        return Err("--follow requires logs or overview".into());
    }
    if opts.interactive
        && opts
            .command
            .first()
            .is_none_or(|command| !matches!(command.as_str(), "exec" | "attach" | "view"))
    {
        return Err("--interactive requires exec, attach or view".into());
    }
    if opts.follow && opts.previous {
        return Err("previous logs cannot be followed".into());
    }
    if opts.json
        && opts.command.first().is_none_or(|command| {
            !matches!(
                command.as_str(),
                "audit"
                    | "extensions"
                    | "contexts"
                    | "overview"
                    | "resources"
                    | "list"
                    | "get"
                    | "watch"
                    | "metrics"
                    | "logs"
            )
        })
    {
        return Err("--json requires a supported read command; see docs/CLI_OUTPUT.md".into());
    }
    if let Some(path) = config_path {
        crate::config::apply(crate::config::load(&path)?, &mut opts)?;
    }
    Ok(opts)
}
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "cannot start I/O runtime".into())
}
pub fn run() -> Result<()> {
    run_args(std::env::args().skip(1).collect())
}
pub fn run_args(args: Vec<String>) -> Result<()> {
    let opts = parse(args)?;
    // Crossterm independently memoizes NO_COLOR. Respect the fully resolved
    // option so an explicit --color=always also overrides that internal gate.
    crossterm::style::force_color_output(opts.color);
    let cmd = opts.command.first().map(String::as_str).unwrap_or("ui");
    match cmd {
        "audit" => {
            if opts.command.as_slice() != ["audit", "pending"] {
                return Err("usage: audit pending [--json]".into());
            }
            let records = crate::audit::pending()?;
            print_json(&serde_json::to_value(records).map_err(|_| "audit encoding failed")?)?;
            return Ok(());
        }
        "config" => {
            let args = &opts.command[1..];
            if args.len() != 2 {
                return Err("usage: config init|validate FILE".into());
            }
            match args[0].as_str() {
                "init" => crate::config::init(std::path::Path::new(&args[1]))?,
                "validate" => {
                    crate::config::load(std::path::Path::new(&args[1]))?;
                }
                _ => return Err("usage: config init|validate FILE".into()),
            }
            outputln!("Configuration OK (schema 1).");
            return Ok(());
        }
        "completions" => {
            if opts.command.len() != 2 {
                return Err("usage: completions bash|zsh|fish|powershell".into());
            }
            output!("{}", crate::completions::script(&opts.command[1])?);
            return Ok(());
        }
        "overview" => {
            if opts.follow {
                return runtime()?.block_on(async {
                    let mut feed =
                        crate::overview::OverviewFeed::new(&opts.connection, &opts.command[1..])?;
                    loop {
                        let rows = feed.next().await?;
                        let record = if opts.json {
                            serde_json::to_string(&rows).map_err(|_| "overview encoding failed")?
                        } else {
                            crate::overview::live_text(&rows)
                        };
                        if let Err(error) = writeln!(io::stdout().lock(), "{record}") {
                            if error.kind() == io::ErrorKind::BrokenPipe {
                                return Ok(());
                            }
                            return Err("overview output failed".into());
                        }
                    }
                });
            }
            let rows = runtime()?.block_on(crate::overview::collect(
                &opts.connection,
                &opts.command[1..],
            ))?;
            if opts.json {
                print_json(&serde_json::to_value(rows).map_err(|_| "overview encoding failed")?)?;
            } else {
                output!("{}", crate::overview::text(&rows));
            }
            return Ok(());
        }
        "help" => {
            outputln!("{}", include_str!("../docs/CLI_HELP.txt"));
            return Ok(());
        }
        "version" => {
            outputln!("KBorg {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "auth-helper" => return auth_helper_command(&opts),
        "extensions" => return extensions_command(&opts),
        "theme" => return theme_command(&opts),
        "copilot" => return copilot_command(&opts),
        "startup" => {
            match opts.command.get(1).map(String::as_str) {
                Some("on") => themes::set_animation(true)?,
                Some("off") => themes::set_animation(false)?,
                _ => return Err("usage: startup on|off".into()),
            }
            outputln!("Startup preference saved.");
            return Ok(());
        }
        "contexts" => {
            let list = contexts(&opts.connection)?;
            if opts.json {
                print_json(&serde_json::json!(list))?;
            } else {
                for c in list {
                    outputln!("{}", safety::safe_text(&c, 100));
                }
            }
            return Ok(());
        }
        "ui" => return tui::run(opts),
        "list" | "watch" | "metrics" | "get" | "view" | "logs" | "resources" | "doctor"
        | "scale" | "restart" | "delete" | "set-image" | "set-resources" | "exec" | "attach"
        | "forward" => {}
        _ => return Err("unknown command; see --help".into()),
    }
    if cmd == "view"
        && ((!opts.allow_tools && !opts.interactive)
            || !io::stdin().is_terminal()
            || !io::stdout().is_terminal())
    {
        return Err("view requires --interactive or --allow-tools, and a terminal".into());
    }
    let rt = runtime()?;
    rt.block_on(async {
        let mut gateway=tokio::time::timeout(Duration::from_secs(15),Gateway::connect(opts.connection.clone())).await.map_err(|_|"connection timed out")??;
        let ns=gateway.namespace.clone();
        let args=&opts.command[1..];
        if matches!(cmd, "list" | "get" | "view" | "watch") && let Some(resource) = args.first() {
            discover_if_needed(&mut gateway, resource).await?;
        }
        match cmd {
            "exec"|"attach"|"forward" => {
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("subresource operations require interactive terminal approval".into());}
                let request=match cmd {
                    "exec"=>{if args.len()<3{return Err("usage: exec POD CONTAINER -- COMMAND [ARG...] --write --allow-exec (put flags before --)".into());}
                    if opts.interactive {crate::ops::Request::InteractiveExec{container:args[1].clone(),argv:args[2..].to_vec(),seconds:opts.session_seconds}} else {crate::ops::Request::Exec{container:args[1].clone(),argv:args[2..].to_vec()}}},
                    "attach"=>{if args.len()!=2{return Err("usage: attach POD CONTAINER --write --allow-attach (bounded output only)".into());}
                    if opts.interactive {crate::ops::Request::InteractiveAttach{container:args[1].clone(),seconds:opts.session_seconds}} else {crate::ops::Request::Attach{container:args[1].clone()}}},
                    _=>{if args.len()!=4{return Err("usage: forward POD LOCAL_PORT REMOTE_PORT SECONDS --write --allow-forward".into());}
                    crate::ops::Request::PortForward{local_port:args[1].parse().map_err(|_|"invalid local port")?,remote_port:args[2].parse().map_err(|_|"invalid remote port")?,seconds:args[3].parse().map_err(|_|"invalid duration")?}},
                };
                let reviewed=tokio::time::timeout(Duration::from_secs(15),crate::ops::prepare(&gateway,&ns,&args[0],request)).await.map_err(|_|"operation review timed out")??;
                outputln!("{}",reviewed.summary()); output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(256).read_line(&mut input).map_err(|_|"input failed")?;
                outputln!("{}",crate::ops::run(&gateway,reviewed,input.trim_end()).await?);
            },
            "watch" => {
                if args.len() != 1 { return Err("usage: watch <resource> [--json]".into()); }
                let mut feed = gateway.watch(&args[0], &ns)?;
                loop {
                    let snapshot = feed.next().await;
                    let record = serde_json::json!({"items":snapshot.objects,"resourceVersion":snapshot.resource_version,"stale":snapshot.stale,"truncated":snapshot.truncated,"terminal":snapshot.terminal,"status":snapshot.message});
                    let line = if opts.json { serde_json::to_string(&record).map_err(|_| "watch output failed")? }
                        else { format!("{} | {} objects", record["status"].as_str().unwrap_or("WATCH"), record["items"].as_array().map_or(0, Vec::len)) };
                    if let Err(error) = writeln!(io::stdout().lock(), "{line}") {
                        if error.kind() == io::ErrorKind::BrokenPipe { return Ok(()); }
                        return Err("watch output failed".into());
                    }
                    if snapshot.terminal { return Err("watch stopped; select a valid narrower scope or reauthenticate".into()); }
                }
            },
            "metrics" => {
                if args.len()!=1 {return Err("usage: metrics pods|nodes [--json]".into());}
                let scope=crate::metrics::MetricsScope::parse(&args[0])?;
                let value=if opts.metrics_resources {crate::metrics::collect_with_allocations(&gateway,scope,&ns).await?} else {crate::metrics::collect(&gateway,scope,&ns).await?};
                if opts.json {print_json(&serde_json::to_value(value).map_err(|_| "metrics output failed")?)?;} else {output!("{}",value.text());}
            },
            "doctor"=>{outputln!("KBorg {}\ncontext: {}\nserver: {}\nnamespace: {}\nmode: {}\nauth: certificate/token; reviewed helpers require explicit trust policy\nAI/extensions: disabled; exec/attach/forward require separate explicit capability and --write",env!("CARGO_PKG_VERSION"),safety::safe_text(&gateway.context,100),safety::safe_text(&gateway.server,200),safety::safe_text(&ns,63),if opts.connection.writable{"WRITE ENABLED"}else{"READ ONLY"});
                let result=tokio::time::timeout(Duration::from_secs(15),gateway.list("pods",&ns)).await.map_err(|_|"API timed out")??;
                outputln!("API pods read: OK ({} objects)",result["items"].as_array().map_or(0,Vec::len));},
            "resources"=>{let resources=tokio::time::timeout(Duration::from_secs(15),gateway.discover()).await.map_err(|_|"discovery timed out")??; if opts.json{print_json(&serde_json::to_value(resources).map_err(|_|"serialization failed")?)?;}else{for kind in resources{outputln!("{}\t{}",kind.key(),if kind.namespaced{"namespaced"}else{"cluster"});}}},
            "list"=>{if args.len()!=1{return Err("usage: list <resource>".into());}let value=tokio::time::timeout(Duration::from_secs(15),gateway.list(&args[0],&ns)).await.map_err(|_|"API timed out")??;
                if opts.json {print_json(&value)?;} else {outputln!("NAMESPACE\tNAME\tSTATUS");if let Some(items)=value["items"].as_array(){for v in items{outputln!("{}\t{}\t{}",safety::safe_text(v["metadata"]["namespace"].as_str().unwrap_or("-"),63),safety::safe_text(v["metadata"]["name"].as_str().unwrap_or("-"),253),safety::safe_text(&summary(v),100));}}
if value["truncated"]==true {eprintln!("TRUNCATED: limit reached; narrow namespace scope");}}},
            "get"=>{if args.len()!=2{return Err("usage: get <resource> <name>".into());}let value=tokio::time::timeout(Duration::from_secs(15),gateway.get(&args[0],&ns,&args[1])).await.map_err(|_|"API timed out")??; if opts.yaml {output!("{}",crate::inspect::yaml(&value)?);}else{print_json(&value)?;}},
            "view" => {
                if opts.interactive {
                    if args.len()!=2 {return Err("native interactive view accepts RESOURCE NAME, without an external --tool".into());}
                    let object=tokio::time::timeout(Duration::from_secs(15),gateway.get(&args[0],&ns,&args[1])).await.map_err(|_|"API timed out")??;
                    let theme=if opts.color {Some(themes::find(opts.theme.as_deref().unwrap_or(&themes::selected()))?)}else{None};
                    crate::pager::run_with_theme(&object,if opts.yaml {crate::handoff::Format::Yaml} else {crate::handoff::Format::Json},opts.session_seconds,theme.as_ref())?;
                    return Ok(());
                }
                if args.len() < 4 || args[2] != "--tool" {
                    return Err("usage: view RESOURCE NAME --tool ABSOLUTE_PATH [--tool-arg ARG] --allow-tools [--yaml]".into());
                }
                let mut argv = Vec::new();
                for pair in args[4..].chunks(2) {
                    if pair.len() != 2 || pair[0] != "--tool-arg" {
                        return Err("tool arguments require one --tool-arg ARG pair each".into());
                    }
                    argv.push(pair[1].clone());
                }
                let object = tokio::time::timeout(Duration::from_secs(15), gateway.get(&args[0], &ns, &args[1])).await.map_err(|_| "API timed out")??;
                let format = if opts.yaml { crate::handoff::Format::Yaml } else { crate::handoff::Format::Json };
                let prepared = crate::handoff::prepare(std::path::Path::new(&args[3]), &argv, &object, format)?;
                outputln!("{}\n{}", prepared.summary(), prepared.input_preview());
                output!("> ");
                io::stdout().flush().map_err(|_| "output failed")?;
                let mut confirmation = String::new();
                use std::io::BufRead;
                io::stdin().lock().take(256).read_line(&mut confirmation).map_err(|_| "input failed")?;
                let outcome = crate::handoff::run(prepared, confirmation.trim_end(), opts.allow_tools)?;
                output!("{}", outcome.stdout);
                if !outcome.stderr.is_empty() { outputln!("Tool stderr:\n{}", outcome.stderr); }
                if !outcome.success { return Err("external tool exited unsuccessfully".into()); }
            },
            "logs"=>{
                if args.is_empty() || args.len()>2{return Err("usage: logs <pod> [container] [--follow|--previous] (requires --allow-logs)".into());}
                if opts.follow {
                    let mut feed=gateway.follow_logs_with_options(&ns,&args[0],args.get(1).cloned(),false,crate::logs::FollowOptions{seconds:opts.log_seconds}).await?;
                    while let Some(line)=feed.next().await? {
                        let line=if opts.json {serde_json::json!({"line":line}).to_string()}else{line};
                        if let Err(error)=writeln!(io::stdout().lock(),"{line}") {if error.kind()==io::ErrorKind::BrokenPipe {return Ok(());}return Err("log output failed".into());}
                    }
                }else{
                    let logs=tokio::time::timeout(Duration::from_secs(15),gateway.logs(&ns,&args[0],args.get(1).cloned(),opts.previous)).await.map_err(|_|"logs timed out")??;
                    if opts.json{print_json(&serde_json::json!({"logs":logs,"bounded":true}))?;}else{outputln!("{logs}");}
                }
            },
            "scale"|"restart"|"delete"|"set-image"|"set-resources"=>{
                if !io::stdin().is_terminal() || !io::stdout().is_terminal(){return Err("noninteractive writes are disabled; use the interactive TUI or terminal confirmation".into());}
                let (resource,name,action)=match cmd {
                    "set-image"=>{if args.len()!=3{return Err("usage: set-image <deployment> <container> <image> --write".into());}("deployments",args[0].as_str(),Action::SetImage{container:args[1].clone(),image:args[2].clone()})},
                    "set-resources"=>{if args.len()!=6{return Err("usage: set-resources DEPLOYMENT CONTAINER CPU_REQUEST MEMORY_REQUEST CPU_LIMIT MEMORY_LIMIT --write".into());}("deployments",args[0].as_str(),Action::SetResources{container:args[1].clone(),resources:crate::edit::ResourceEdit{cpu_request:args[2].clone(),memory_request:args[3].clone(),cpu_limit:args[4].clone(),memory_limit:args[5].clone()}})},
                    "scale"=>{if args.len()!=2{return Err("usage: scale <deployment> <replicas> --write".into());}("deployments",args[0].as_str(),Action::Scale(args[1].parse().map_err(|_|"invalid replicas")?))},
                    "restart"=>{if args.len()!=1{return Err("usage: restart <deployment> --write".into());}("deployments",args[0].as_str(),Action::Restart)},
                    _=>{if args.len()!=2{return Err("usage: delete <resource> <name> --write".into());}(args[0].as_str(),args[1].as_str(),Action::Delete)},
                };
                let prepared=tokio::time::timeout(Duration::from_secs(15),gateway.prepare(resource,&ns,name,action)).await.map_err(|_|"dry-run timed out")??;
                outputln!("{}",safety::review_text(&prepared.summary,16_384)?);output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(256).read_line(&mut input).map_err(|_|"input failed")?;
                let approval=Approval::confirm(prepared.target.clone(),prepared.action.clone(),input.trim_end())?;
                outputln!("{}",gateway.execute(prepared,approval).await?);
            },
            _=>{},
        } Ok(())
    })
}
fn print_json(value: &serde_json::Value) -> Result<()> {
    let encoded = serde_json::to_string_pretty(value).map_err(|_| "JSON serialization failed")?;
    write_text(&format!("{encoded}\n"))
}
fn copilot_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    match args.first().map(String::as_str) {
        Some("preflight") => {
            let mut executable=None; let mut host=None; let mut expected=None; let mut token=None;
            for pair in args[1..].chunks(2) {
                if pair.len()!=2 {return Err("Copilot preflight options need values".into());}
                match pair[0].as_str() {
                    "--cli" if executable.is_none()=>executable=Some(PathBuf::from(&pair[1])),
                    "--host" if host.is_none()=>host=Some(pair[1].clone()),
                    "--expected-sha256" if expected.is_none()=>expected=Some(pair[1].clone()),
                    "--credential-file" if token.is_none()=>token=Some(PathBuf::from(&pair[1])),
                    _=>return Err("unknown or duplicate Copilot preflight option".into()),
                }
            }
            let executable=executable.ok_or("copilot preflight requires --cli ABSOLUTE_NATIVE_SDK")?;
            let host=host.ok_or("copilot preflight requires --host github.com|org.ghe.com")?;
            let report=crate::advice::preflight(&executable,&host,expected.as_deref(),token.as_deref())?;
            print_json(&serde_json::to_value(report).map_err(|_|"cannot encode Copilot preflight")?)?;
            Ok(())
        },
        Some("preview") if args.len()==5 && args[3]=="--host" => {
            let object=runtime()?.block_on(async {
                let mut gateway=tokio::time::timeout(Duration::from_secs(20),Gateway::connect(opts.connection.clone())).await.map_err(|_|"connection timed out")??;
                discover_if_needed(&mut gateway,&args[1]).await?;
                gateway.get(&args[1],&gateway.namespace,&args[2]).await
            })?;
            let preview=crate::advice::prepare_preview(&object,&args[4])?;
            outputln!("LOCAL PREVIEW ONLY; no request is sent.\n{}",preview.preview()?);
            outputln!("Automated diagnosis remains disabled until hostname-bound egress and Copilot credential authority are validated.");
            Ok(())
        },
        Some("login") if args.len() == 5 && args[1] == "--cli" && args[3] == "--host" => {
            crate::copilot::login(std::path::Path::new(&args[2]), &args[4])
        }
        Some("context") if args.len() == 5 && args[3] == "--output" => {
            let value = runtime()?.block_on(async {
                let mut gateway = tokio::time::timeout(Duration::from_secs(15), Gateway::connect(opts.connection.clone())).await.map_err(|_| "connection timed out")??;
                discover_if_needed(&mut gateway, &args[1]).await?;
                crate::copilot::context(&gateway, &args[1], &gateway.namespace, &args[2]).await
            })?;
            safety::write_new(std::path::Path::new(&args[4]), &serde_json::to_vec_pretty(&value).map_err(|_| "cannot encode diagnostic context")?)?;
            outputln!("Minimized context saved locally. Inspect the JSON before manually sharing with GitHub Copilot. No AI request was made.");
            Ok(())
        }
        _ => Err("usage: copilot preflight --cli ABS_NATIVE --host HOST [--expected-sha256 SHA] [--credential-file FILE] | copilot preview RESOURCE NAME --host HOST | copilot login --cli <absolute executable> --host <github.com|org.ghe.com> | copilot context <resource> <name> --output <new JSON file>".into()),
    }
}
async fn discover_if_needed(gateway: &mut Gateway, resource: &str) -> Result<()> {
    if resource.is_empty()
        || resource.len() > 253
        || !resource
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
        || resource.split('.').next() == Some("secrets")
    {
        return Err("invalid or disabled resource kind".into());
    }
    let known = |g: &Gateway| {
        g.kinds
            .iter()
            .any(|k| k.key() == resource || k.plural == resource)
    };
    if !known(gateway) {
        gateway.discover().await?;
    }
    if !known(gateway) {
        return Err("resource kind unavailable after bounded discovery".into());
    }
    Ok(())
}

fn extensions_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    match args.first().map(String::as_str) {
        Some("list") if args.len()==1 => {
            let entries=crate::extensions::list()?;
            if opts.json { print_json(&serde_json::to_value(entries).map_err(|_|"cannot encode extensions")?)?; }
            else { for entry in entries {outputln!("{}\t{}\t{}",entry.id,entry.executable.display(),entry.sha256);} }
            Ok(())
        },
        Some("remove") if args.len()==2 && !opts.json => {
            crate::extensions::remove(&args[1])?; outputln!("Local extension registration removed."); Ok(())
        },
        Some("register") if args.len()>=4 && args[2]=="--tool" && !opts.json => {
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("extension registration requires an interactive review".into());}
            let mut argv=Vec::new();
            for pair in args[4..].chunks(2) {
                if pair.len()!=2 || pair[0]!="--tool-arg" {return Err("literal extension arguments require --tool-arg ARG pairs".into());}
                argv.push(pair[1].clone());
            }
            let review=crate::extensions::prepare_registration(&args[1],std::path::Path::new(&args[3]),&argv)?;
            outputln!("{}",review.summary());output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
            let mut typed=String::new();use std::io::BufRead;
            io::stdin().lock().take(256).read_line(&mut typed).map_err(|_|"input failed")?;
            review.save(&crate::extensions::registry_dir()?,typed.trim_end())?;
            outputln!("Local read-only extension registered; every run still requires explicit capability and transfer review."); Ok(())
        },
        Some("run") if args.len()==4 && !opts.json => {
            if !opts.allow_extensions || !opts.allow_tools || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Err("extensions run requires --allow-extensions --allow-tools and interactive whole-input review".into());
            }
            let registered=crate::extensions::load(&args[1])?;
            let object=runtime()?.block_on(async {
                let mut gateway=tokio::time::timeout(Duration::from_secs(20),Gateway::connect(opts.connection.clone())).await.map_err(|_|"connection timed out")??;
                discover_if_needed(&mut gateway,&args[2]).await?;
                tokio::time::timeout(Duration::from_secs(15),gateway.get(&args[2],&gateway.namespace,&args[3])).await.map_err(|_|"API timed out")?
            })?;
            let review=registered.prepare(&object,crate::handoff::Format::Json)?;
            outputln!("{}\n{}\n{}",registered.summary(),review.summary(),review.input_preview());output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
            let mut typed=String::new();use std::io::BufRead;
            io::stdin().lock().take(256).read_line(&mut typed).map_err(|_|"input failed")?;
            let outcome=crate::handoff::run(review,typed.trim_end(),opts.allow_tools && opts.allow_extensions)?;
            output!("{}",outcome.stdout);
            if !outcome.stderr.is_empty() {outputln!("Tool stderr:\n{}",outcome.stderr);}
            if !outcome.success {return Err("extension tool exited unsuccessfully".into());}
            Ok(())
        },
        _=>Err("usage: extensions list [--json] | register NAME --tool ABS [--tool-arg ARG] | remove NAME | run NAME RESOURCE OBJECT --allow-extensions --allow-tools".into()),
    }
}

fn auth_helper_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    if args.first().map(String::as_str) != Some("review")
        || args.len() < 3
        || !io::stdin().is_terminal()
        || !io::stdout().is_terminal()
    {
        return Err("usage: auth-helper review [--executable ABS] [--env NAME=VALUE] --output NEW_POLICY.json (interactive terminal required)".into());
    }
    let mut executable = None;
    let mut output = None;
    let mut environment = Vec::new();
    for pair in args[1..].chunks(2) {
        if pair.len() != 2 {
            return Err("auth-helper review options need values".into());
        }
        match pair[0].as_str() {
            "--executable" if executable.is_none() => executable = Some(PathBuf::from(&pair[1])),
            "--output" if output.is_none() => output = Some(PathBuf::from(&pair[1])),
            "--env" => {
                let (key, value) = pair[1].split_once('=').ok_or("--env requires NAME=VALUE")?;
                environment.push((key.to_string(), value.to_string()));
            }
            _ => return Err("invalid or duplicate auth-helper review option".into()),
        }
    }
    let output = output.ok_or("auth-helper review needs --output NEW_POLICY.json")?;
    let prepared =
        crate::gateway::review_auth_helper(&opts.connection, executable.as_deref(), &environment)?;
    outputln!("{}", prepared.summary());
    output!("> ");
    io::stdout().flush().map_err(|_| "output failed")?;
    let mut confirmation = String::new();
    use std::io::BufRead;
    io::stdin()
        .lock()
        .take(256)
        .read_line(&mut confirmation)
        .map_err(|_| "input failed")?;
    prepared.save(&output, confirmation.trim_end())?;
    outputln!(
        "Auth helper trust policy saved. Enable explicitly with --allow-auth-helper --auth-helper-policy FILE. No helper was executed during review."
    );
    Ok(())
}

fn theme_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    let get = |index: usize| {
        args.get(index)
            .map(String::as_str)
            .ok_or("missing theme argument".to_string())
    };
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            for t in themes::all()? {
                outputln!(
                    "{}\t{}{}",
                    t.id,
                    t.name,
                    if themes::selected() == t.id {
                        " [selected]"
                    } else {
                        ""
                    }
                );
            }
        }
        "use" => {
            themes::use_theme(get(1)?)?;
            outputln!("Theme saved.");
        }
        "reset" => {
            themes::use_theme("catppuccin-mocha")?;
            outputln!("Appearance reset.");
        }
        "new" => {
            let base = if args.get(2).is_some_and(|s| s == "--base") {
                get(3)?
            } else {
                "k8borg-dark"
            };
            outputln!("{}", themes::new_theme(get(1)?, base)?.display());
        }
        "validate" => {
            themes::load_file(&PathBuf::from(get(1)?))?;
            outputln!("VALID: schema, colors and contrast checked.");
        }
        "preview" => {
            let theme = themes::load_file(&PathBuf::from(get(1)?))?.resolve()?;
            tui::preview(theme, opts.color)?;
        }
        "replace" => {
            outputln!(
                "Theme replaced; backup retained: {}",
                themes::replace(&PathBuf::from(get(1)?))?
            );
        }
        "clone-current" => {
            outputln!("{}", themes::create_from_current(get(1)?)?.display());
        }
        "install" => {
            let id = themes::install(&PathBuf::from(get(1)?))?;
            outputln!("Installed {id}. Activate with: kborg theme use {id}");
        }
        "export" => {
            if get(2)? != "--output" {
                return Err("usage: theme export <id> --output <path>".into());
            }
            themes::export(get(1)?, &PathBuf::from(get(3)?))?;
            outputln!("Theme exported.");
        }
        "remove" => {
            themes::remove(get(1)?)?;
            outputln!("Custom theme removed.");
        }
        _ => return Err("unknown theme command; see --help".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downstream_close_is_normal_but_other_write_failures_are_errors() {
        struct Failing(io::ErrorKind);
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(self.0))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(write_to(&mut Failing(io::ErrorKind::BrokenPipe), b"safe").is_ok());
        assert!(write_to(&mut Failing(io::ErrorKind::PermissionDenied), b"safe").is_err());
    }
    #[test]
    fn tool_argv_preserves_literal_flags_and_capabilities_stay_explicit() -> Result<()> {
        let options = parse(
            [
                "view",
                "pods",
                "api",
                "--tool",
                "/usr/bin/cat",
                "--tool-arg",
                "--write",
                "--allow-tools",
            ]
            .map(String::from)
            .to_vec(),
        )?;
        assert!(options.allow_tools);
        assert!(!options.connection.writable);
        assert_eq!(
            &options.command[4..],
            &["/usr/bin/cat", "--tool-arg", "--write"]
        );
        assert!(!parse(vec![])?.allow_tools);
        assert!(
            parse(
                ["get", "pods", "api", "--tool", "/usr/bin/cat"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn flags_preserve_explicit_scope_and_default_read_only() {
        let p = parse(vec![
            "--context".into(),
            "local".into(),
            "list".into(),
            "pods".into(),
            "-n".into(),
            "test".into(),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.connection.context.as_deref(), Some("local"));
        assert_eq!(p.connection.namespace, "test");
        assert!(!p.connection.writable);
    }
    #[test]
    fn refuses_unknown_flags() {
        assert!(
            parse(vec![
                "--yes".into(),
                "delete".into(),
                "pods".into(),
                "x".into()
            ])
            .is_err()
        );
        assert!(parse(vec!["--json".into(), "doctor".into()]).is_err());
        assert!(parse(vec!["--json".into()]).is_err());
    }
}
