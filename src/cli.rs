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
    pub all_containers: bool,
    /// Explicit opt-in: announced Pod joins/departures for followed workload logs.
    pub dynamic_membership: bool,
    /// `--contexts a,b`: open the TUI directly in the dashboard (k10s style).
    pub dashboard: Vec<String>,
    /// `kborg dashboard` / `--dashboard`: reopen the last dashboard, or pick
    /// clusters on first use.
    pub dashboard_last: bool,
    /// Opt-in: Ctrl-E in the YAML editor hands the draft to $VISUAL/$EDITOR.
    pub external_editor: bool,
    pub helm_storage: Option<crate::helm::Storage>,
    pub helm_tool: Option<PathBuf>,
    pub helm_values: Option<PathBuf>,
    pub helm_registry_ca: Option<PathBuf>,
    pub helm_registry_auth: Option<PathBuf>,
    pub helm_keyring: Option<PathBuf>,
    pub helm_sigstore_policy: Option<PathBuf>,
    pub copilot_policy: Option<PathBuf>,
    pub helm_timeout_seconds: u16,
    pub log_seconds: u32,
    pub log_read: crate::logs::ReadOptions,
    pub selector: String,
    pub interactive: bool,
    pub metrics_resources: bool,
    pub session_seconds: u16,
    /// Image for node shell Pods (`--node-shell-image`).
    pub node_shell_image: String,
    /// `cp-from` / `cp-to --recursive`: copy a directory.
    pub recursive: bool,
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
        all_containers: false,
        dynamic_membership: false,
        dashboard: Vec::new(),
        dashboard_last: false,
        external_editor: false,
        helm_storage: None,
        helm_tool: None,
        helm_values: None,
        helm_registry_ca: None,
        helm_registry_auth: None,
        helm_keyring: None,
        helm_sigstore_policy: None,
        copilot_policy: None,
        helm_timeout_seconds: 300,
        log_seconds: crate::logs::DEFAULT_FOLLOW_SECONDS,
        log_read: crate::logs::ReadOptions::default(),
        selector: String::new(),
        interactive: false,
        metrics_resources: false,
        session_seconds: 900,
        node_shell_image: crate::node_shell::DEFAULT_IMAGE.into(),
        recursive: false,
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
            "--all-containers" => opts.all_containers = true,
            "--dynamic-membership" => opts.dynamic_membership = true,
            "--dashboard" => opts.dashboard_last = true,
            "--allow-external-editor" => opts.external_editor = true,
            "--contexts" => {
                let names: Vec<String> = args
                    .next()
                    .ok_or("--contexts needs a comma-separated list")?
                    .split(',')
                    .map(|name| name.trim().to_owned())
                    .collect();
                if names.len() > crate::overview::MAX_LIVE_CONTEXTS {
                    return Err("--contexts opens the dashboard with 1..9 contexts".into());
                }
                crate::overview::validate(&names)?;
                opts.dashboard = names;
            }
            "--allow-helm" => opts.connection.allow_helm = true,
            "--allow-helm-write" => opts.connection.allow_helm_write = true,
            "--allow-yaml-edit" => opts.connection.allow_yaml_edit = true,
            "--allow-force-kill" => opts.connection.allow_force_kill = true,
            "--allow-secret-reveal" => opts.connection.allow_secret_reveal = true,
            "--allow-helm-hooks" => opts.connection.allow_helm_hooks = true,
            "--allow-helm-crds" => opts.connection.allow_helm_crds = true,
            "--copilot-policy" => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--copilot-policy needs an absolute policy file")?,
                );
                if !path.is_absolute() {
                    return Err("--copilot-policy requires an absolute path".into());
                }
                opts.copilot_policy = Some(path);
            }
            "--helm-sigstore-policy" => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--helm-sigstore-policy needs an absolute policy file")?,
                );
                if !path.is_absolute() {
                    return Err("--helm-sigstore-policy requires an absolute policy path".into());
                }
                opts.helm_sigstore_policy = Some(path);
            }
            "--helm-keyring" => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--helm-keyring needs an absolute public keyring path")?,
                );
                if !path.is_absolute() {
                    return Err("--helm-keyring requires an absolute public keyring path".into());
                }
                opts.helm_keyring = Some(path);
            }
            "--helm-registry-ca" | "--helm-registry-auth" => {
                let value = PathBuf::from(
                    args.next()
                        .ok_or("Helm registry option needs an absolute file path")?,
                );
                if !value.is_absolute() {
                    return Err("Helm registry CA/auth files require absolute paths".into());
                }
                if arg == "--helm-registry-ca" {
                    opts.helm_registry_ca = Some(value);
                } else {
                    opts.helm_registry_auth = Some(value);
                }
            }
            "--helm-tool" => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("--helm-tool needs an absolute executable path")?,
                );
                if !path.is_absolute() {
                    return Err("--helm-tool requires an absolute path".into());
                }
                opts.helm_tool = Some(path);
            }
            "--helm-values" => {
                opts.helm_values = Some(PathBuf::from(
                    args.next().ok_or("--helm-values needs a file")?,
                ))
            }
            "--helm-timeout" => {
                opts.helm_timeout_seconds = args
                    .next()
                    .ok_or("--helm-timeout needs seconds")?
                    .parse()
                    .map_err(|_| "invalid Helm timeout")?;
                if !(1..=900).contains(&opts.helm_timeout_seconds) {
                    return Err("Helm timeout must be 1..900 seconds".into());
                }
            }
            "--helm-storage" => {
                opts.helm_storage = Some(match args.next().as_deref() {
                    Some("secret" | "secrets") => crate::helm::Storage::Secret,
                    Some("configmap" | "configmaps") => crate::helm::Storage::ConfigMap,
                    _ => return Err("--helm-storage requires secret|configmap".into()),
                })
            }
            "--head" => opts.log_read.head = true,
            "--tail" => {
                opts.log_read.tail_lines = args
                    .next()
                    .ok_or("--tail needs a line count")?
                    .parse()
                    .map_err(|_| "invalid log tail")?
            }
            "--since-seconds" => {
                opts.log_read.since_seconds = Some(
                    args.next()
                        .ok_or("--since-seconds needs a value")?
                        .parse()
                        .map_err(|_| "invalid log lookback")?,
                )
            }
            "--selector" | "-l" => {
                opts.selector = args.next().ok_or("--selector needs a label selector")?
            }
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
                let namespace = args.next().ok_or("--namespace needs a value")?;
                // k9s and the docs use `all` for every namespace.
                opts.connection.namespace = if namespace == "all" {
                    "*".into()
                } else {
                    namespace
                };
            }
            "--all-namespaces" | "-A" => opts.connection.namespace = "*".into(),
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
            "--allow-file-copy" => opts.connection.allow_file_copy = true,
            "--allow-apply" => opts.connection.allow_apply = true,
            "--recursive" => opts.recursive = true,
            "--allow-node-shell" => opts.connection.allow_node_shell = true,
            "--allow-debug" => opts.connection.allow_debug = true,
            "--node-shell-image" => {
                let image = args
                    .next()
                    .ok_or("--node-shell-image needs an image reference")?;
                crate::node_shell::validate_image(&image)?;
                opts.node_shell_image = image;
            }
            "--allow-node-maintenance" => opts.connection.allow_node_maintenance = true,
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
    if matches!(
        opts.command.first().map(String::as_str),
        Some("helm" | "history")
    ) {
        let request = HelmCommand::parse(opts.command[0].as_str(), &opts.command[1..])?;
        if request.is_mutation() && opts.json {
            return Err(
                "Helm lifecycle actions require a complete interactive review; --json is read-only"
                    .into(),
            );
        }
        if opts.helm_values.is_some()
            && !matches!(request, HelmCommand::Install(..) | HelmCommand::Upgrade(..))
        {
            return Err("--helm-values requires Helm install or upgrade".into());
        }
    }
    if opts
        .command
        .first()
        .is_some_and(|command| command == "logs")
    {
        LogCommand::parse(&opts.command[1..], &opts.selector, opts.all_containers)?;
    } else if opts.all_containers {
        return Err("--all-containers requires logs without an explicit container".into());
    }
    if (opts.helm_tool.is_some()
        || opts.helm_values.is_some()
        || opts.helm_timeout_seconds != 300
        || opts.helm_registry_ca.is_some()
        || opts.helm_registry_auth.is_some()
        || opts.helm_keyring.is_some()
        || opts.helm_sigstore_policy.is_some()
        || opts.connection.allow_helm_hooks
        || opts.connection.allow_helm_crds)
        && opts
            .command
            .first()
            .is_some_and(|command| !matches!(command.as_str(), "ui" | "helm"))
    {
        return Err("Helm lifecycle, registry, hook and CRD options require ui or helm".into());
    }
    if opts.copilot_policy.is_some()
        && opts
            .command
            .first()
            .is_some_and(|command| !matches!(command.as_str(), "ui" | "copilot"))
    {
        return Err("--copilot-policy requires ui or copilot".into());
    }
    if opts.helm_storage.is_some()
        && opts
            .command
            .first()
            .is_some_and(|command| !matches!(command.as_str(), "ui" | "helm" | "history"))
    {
        return Err("--helm-storage requires ui, helm or history".into());
    }
    opts.log_read.validate()?;
    if opts.log_read != crate::logs::ReadOptions::default()
        && opts.command.first().is_none_or(|v| v != "logs")
    {
        return Err("--head/--tail/--since-seconds require logs".into());
    }
    if opts.log_read.head && opts.follow {
        return Err("--head is a snapshot and cannot be followed".into());
    }
    if opts.dynamic_membership && (!opts.follow || opts.command.first().is_none_or(|v| v != "logs"))
    {
        return Err(
            "--dynamic-membership requires logs --follow on a workload or --selector".into(),
        );
    }
    crate::selectors::validate(&opts.selector)?;
    if !opts.selector.is_empty()
        && opts
            .command
            .first()
            .is_some_and(|c| !matches!(c.as_str(), "ui" | "list" | "watch" | "logs"))
    {
        return Err("--selector requires ui, list, watch or logs".into());
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
                    | "helm"
                    | "history"
                    | "lint"
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
                Some("preview") => tui::play_startup_animation(opts.theme.as_deref(), opts.color)?,
                _ => return Err("usage: startup on|off|preview".into()),
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
        "dir" => {
            if opts.command.len() > 2 {
                return Err("usage: dir [PATH]".into());
            }
            let path = std::path::Path::new(opts.command.get(1).map(String::as_str).unwrap_or("."));
            if path.is_file() {
                outputln!("{}", crate::navigation_views::preview_manifest(path)?);
            } else {
                let directory = crate::navigation_views::directory(path)?;
                outputln!(
                    "LOCAL MANIFESTS | {}",
                    safety::safe_text(&directory.path.display().to_string(), 4096)
                );
                for entry in directory.entries {
                    outputln!("{}{}", entry.name, if entry.directory { "/" } else { "" });
                }
                if directory.truncated {
                    outputln!("TRUNCATED directory listing; select a narrower path");
                }
            }
            return Ok(());
        }
        "ui" => return tui::run(opts),
        "dashboard" | "dash" => {
            let mut opts = opts;
            opts.dashboard_last = true;
            return tui::run(opts);
        }
        "helm" | "history" | "list" | "watch" | "metrics" | "get" | "view" | "logs"
        | "resources" | "doctor" | "scale" | "restart" | "delete" | "kill" | "set-image"
        | "trigger" | "suspend" | "resume" | "rollback" | "set-resources" | "exec" | "attach"
        | "forward" | "cp-from" | "cp-to" | "apply" | "node-shell" | "debug" | "lint"
        | "pulses" | "xray" | "workloads" | "can" | "yaml-template" | "yaml-edit" | "cordon"
        | "uncordon" | "drain" => {}
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
        if matches!(cmd, "list" | "get" | "view" | "watch" | "xray" | "yaml-template" | "yaml-edit") && let Some(resource) = args.first() {
            discover_if_needed(&mut gateway, resource).await?;
        }
        match cmd {
            "helm"|"history"=>helm_command(&gateway,cmd,args,&opts).await?,
            "can" => {
                if args.len() != 1 { return Err("usage: can u:NAME|g:NAME|s:NAME [-n NAMESPACE]".into()); }
                outputln!("{}", crate::navigation_views::can(&gateway, &ns, &args[0]).await?);
            },
            "workloads" => {
                if !args.is_empty() { return Err("usage: workloads [-n NAMESPACE]".into()); }
                outputln!("{}", crate::insights::workloads(&gateway,&ns).await?);
            },
            "pulses" => {
                if !args.is_empty() { return Err("usage: pulses [-n NAMESPACE]".into()); }
                outputln!("{}", crate::insights::pulses(&gateway, &ns).await?);
            },
            "xray" => {
                if args.len() != 1 { return Err("usage: xray RESOURCE [-n NAMESPACE]".into()); }
                outputln!("{}", crate::insights::xray(&gateway, &args[0], &ns).await?);
            },
            "cordon"|"uncordon"|"drain" => {
                if args.is_empty() || args.len()>2 || (cmd!="drain" && args.len()!=1) {return Err("usage: cordon NODE | uncordon NODE | drain NODE [SECONDS] --write --allow-node-maintenance".into());}
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("node maintenance requires interactive terminal approval".into());}
                let request=match cmd {"cordon"=>crate::node_maintenance::Request::Cordon,"uncordon"=>crate::node_maintenance::Request::Uncordon,_=>crate::node_maintenance::Request::Drain{seconds:args.get(1).map(|s|s.parse::<u16>().map_err(|_|"invalid drain duration")).transpose()?.unwrap_or(120)}};
                let reviewed=crate::node_maintenance::prepare(&gateway,&args[0],request).await?;
                outputln!("{}",reviewed.summary()); output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(322).read_line(&mut input).map_err(|_|"input failed")?;
                let report=crate::node_maintenance::execute(&gateway,reviewed,input.trim_end()).await?;
                outputln!("{}",report.summary());
                if !report.complete {return Err("node maintenance incomplete; inspect the reported pending targets and scheduling state".into());}
            },
            "lint" => {
                if !args.is_empty() {return Err("usage: lint [-n NAMESPACE] [--json]".into());}
                let (found,notes)=tokio::time::timeout(Duration::from_secs(30),crate::lint::collect(&gateway,&ns)).await.map_err(|_|"lint timed out")??;
                if opts.json {print_json(&serde_json::json!({"findings":found,"notes":notes}))?;}
                else {output!("{}",crate::lint::report(&gateway.context,&ns,&found,&notes));}
            },
            "debug" => {
                if args.is_empty() || args.len()>2 {return Err("usage: debug POD [TARGET_CONTAINER] --write --allow-debug [--node-shell-image IMAGE]".into());}
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("debug requires interactive terminal approval".into());}
                let prepared=crate::debug_container::prepare(&gateway,&ns,&args[0],args.get(1).map(String::as_str),&opts.node_shell_image,opts.session_seconds).await?;
                outputln!("{}",safety::review_display(prepared.summary())?); output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(320).read_line(&mut input).map_err(|_|"input failed")?;
                outputln!("Adding the debug container and waiting for it to start…");
                let reviewed=crate::debug_container::start(&gateway,prepared,input.trim_end()).await?;
                let typed=reviewed.confirmation();
                outputln!("{}",crate::ops::run(&gateway,reviewed,&typed).await?);
            },
            "node-shell" => {
                if args.len()!=1 {return Err("usage: node-shell NODE --write --allow-node-shell [-n NAMESPACE] [--node-shell-image IMAGE]".into());}
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("node shell requires interactive terminal approval".into());}
                let namespace=if ns=="*"||ns.is_empty(){"default".to_owned()}else{ns.clone()};
                let prepared=crate::node_shell::prepare(&gateway,&args[0],&namespace,&opts.node_shell_image,opts.session_seconds).await?;
                outputln!("{}",safety::review_display(prepared.summary())?); output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(320).read_line(&mut input).map_err(|_|"input failed")?;
                outputln!("Creating the node shell Pod and waiting for it to start…");
                let reviewed=crate::node_shell::start(&gateway,prepared,input.trim_end()).await?;
                let typed=reviewed.confirmation();
                outputln!("{}",crate::ops::run(&gateway,reviewed,&typed).await?);
            },
            "apply" => {
                if args.len()!=1 {return Err("usage: apply FILE.yaml --write --allow-apply [-n NAMESPACE]".into());}
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("apply requires interactive terminal approval".into());}
                let prepared=tokio::time::timeout(Duration::from_secs(120),gateway.prepare_apply(std::path::Path::new(&args[0]),&ns)).await.map_err(|_|"apply review timed out")??;
                outputln!("{}",safety::review_display(prepared.summary())?); output!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(256).read_line(&mut input).map_err(|_|"input failed")?;
                outputln!("{}",gateway.execute_apply(prepared,input.trim_end()).await?);
            },
            "exec"|"attach"|"forward"|"cp-from"|"cp-to" => {
                if !io::stdin().is_terminal() || !io::stdout().is_terminal() {return Err("subresource operations require interactive terminal approval".into());}
                let request=match cmd {
                    "exec"=>{if args.len()<3{return Err("usage: exec POD CONTAINER -- COMMAND [ARG...] --write --allow-exec (put flags before --)".into());}
                    if opts.interactive {crate::ops::Request::InteractiveExec{container:args[1].clone(),argv:args[2..].to_vec(),seconds:opts.session_seconds}} else {crate::ops::Request::Exec{container:args[1].clone(),argv:args[2..].to_vec()}}},
                    "cp-from"=>{if args.len()!=4{return Err("usage: cp-from POD CONTAINER /CONTAINER/FILE LOCAL_FILE_OR_DIR --write --allow-file-copy".into());}
                    crate::ops::Request::CopyFrom{container:args[1].clone(),remote:args[2].clone(),local:std::path::PathBuf::from(&args[3]),recursive:opts.recursive}},
                    "cp-to"=>{if args.len()!=4{return Err("usage: cp-to POD CONTAINER LOCAL_FILE /CONTAINER/FILE --write --allow-file-copy".into());}
                    crate::ops::Request::CopyTo{container:args[1].clone(),upload:if opts.recursive {crate::file_copy::read_upload_dir(std::path::Path::new(&args[2]))?} else {crate::file_copy::read_upload(std::path::Path::new(&args[2]))?},remote:args[3].clone()}},
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
                let mut feed = gateway.watch_selector(&args[0], &ns, &opts.selector)?;
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
            "doctor"=>{outputln!("KBorg {}\ncontext: {}\nserver: {}\nnamespace: {}\nmode: {}\nauth: certificate/token; reviewed helpers require explicit trust policy\nAI diagnosis: disabled; registered extensions require explicit opt-in\nexec/attach/forward require separate explicit capability and --write",env!("CARGO_PKG_VERSION"),safety::safe_text(&gateway.context,100),safety::safe_text(&gateway.server,200),safety::safe_text(&ns,63),if opts.connection.writable{"WRITE ENABLED"}else{"READ ONLY"});
                let result=tokio::time::timeout(Duration::from_secs(15),gateway.list("pods",&ns)).await.map_err(|_|"API timed out")??;
                outputln!("API pods read: OK ({} objects)",result["items"].as_array().map_or(0,Vec::len));},
            "resources"=>{let resources=tokio::time::timeout(Duration::from_secs(15),gateway.discover()).await.map_err(|_|"discovery timed out")??; if opts.json{print_json(&serde_json::to_value(resources).map_err(|_|"serialization failed")?)?;}else{for kind in resources{outputln!("{}\t{}",kind.key(),if kind.namespaced{"namespaced"}else{"cluster"});}}},
            "list"=>{if args.len()!=1{return Err("usage: list <resource>".into());}let value=tokio::time::timeout(Duration::from_secs(15),gateway.list_selector(&args[0],&ns,&opts.selector)).await.map_err(|_|"API timed out")??;
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
            "yaml-template" | "yaml-edit" => {
                let needed = if cmd == "yaml-template" { 2 } else { 3 };
                if args.len() != needed { return Err("usage: yaml-template RESOURCE NAME | yaml-edit RESOURCE NAME FILE --write --allow-yaml-edit".into()); }
                if cmd == "yaml-edit" && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) { return Err("YAML writes require interactive terminal confirmation".into()); }
                let draft = tokio::time::timeout(Duration::from_secs(15), gateway.yaml_draft(&args[0], &ns, &args[1])).await.map_err(|_| "YAML read timed out")??;
                if cmd == "yaml-template" { output!("{}", draft.text()); }
                else {
                    let bytes = safety::read_small(std::path::Path::new(&args[2]), crate::manifest_edit::MAX_DOCUMENT as u64)?;
                    let edited = std::str::from_utf8(&bytes).map_err(|_| "YAML file must be UTF-8")?;
                    let prepared = tokio::time::timeout(Duration::from_secs(30), gateway.prepare_yaml(&draft, edited)).await.map_err(|_| "YAML review timed out")??;
                    outputln!("{}", prepared.summary()); output!("> "); io::stdout().flush().map_err(|_| "output failed")?;
                    let mut typed = String::new(); use std::io::BufRead;
                    io::stdin().lock().take(512).read_line(&mut typed).map_err(|_| "input failed")?;
                    outputln!("{}", gateway.execute_yaml(prepared, typed.trim_end()).await?);
                }
            },
            "logs"=>logs_command(&gateway,args,&opts).await?,
            "scale"|"restart"|"delete"|"kill"|"set-image"|"set-resources"|"trigger"|"suspend"|"resume"|"rollback"=>{
                if !io::stdin().is_terminal() || !io::stdout().is_terminal(){return Err("noninteractive writes are disabled; use the interactive TUI or terminal confirmation".into());}
                let (resource,name,action)=match cmd {
                    "set-image"=>{let (resource,args)=write_args(args,3,"set-image [RESOURCE] NAME CONTAINER IMAGE --write")?;(resource,args[0].as_str(),Action::SetImage{container:args[1].clone(),image:args[2].clone()})},
                    "set-resources"=>{let (resource,args)=write_args(args,6,"set-resources [RESOURCE] NAME CONTAINER CPU_REQUEST MEMORY_REQUEST CPU_LIMIT MEMORY_LIMIT --write")?;(resource,args[0].as_str(),Action::SetResources{container:args[1].clone(),resources:crate::edit::ResourceEdit{cpu_request:args[2].clone(),memory_request:args[3].clone(),cpu_limit:args[4].clone(),memory_limit:args[5].clone()}})},
                    "scale"=>{let (resource,args)=write_args(args,2,"scale [RESOURCE] NAME REPLICAS --write")?;(resource,args[0].as_str(),Action::Scale(args[1].parse().map_err(|_|"invalid replicas")?))},
                    "restart"=>{let (resource,args)=write_args(args,1,"restart [RESOURCE] NAME --write")?;(resource,args[0].as_str(),Action::Restart)},
                    "trigger"|"suspend"|"resume"=>{if args.len()!=1{return Err(format!("usage: {cmd} CRONJOB --write"));}("cronjobs",args[0].as_str(),match cmd {"trigger"=>Action::Trigger,"suspend"=>Action::Suspend(true),_=>Action::Suspend(false)})},
                    "rollback"=>{
                        let (resource,rest)=match args.len() {1|2 if args[0].parse::<u64>().is_err() && !["deployments","statefulsets","daemonsets","deploy","sts","ds"].contains(&args[0].as_str()) => ("deployments",args), 2|3 => (args[0].as_str(),&args[1..]), _ => return Err("usage: rollback [RESOURCE] NAME [REVISION] --write".into())};
                        let revision=rest.get(1).map(|r|r.parse::<u64>().map_err(|_|"invalid revision")).transpose()?;
                        (resource,rest[0].as_str(),Action::Rollback(revision))
                    },
                    "kill"=>{if args.len()!=1{return Err("usage: kill POD --write --allow-force-kill".into());}("pods",args[0].as_str(),Action::Kill)},
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
#[derive(Debug, PartialEq, Eq)]
enum LogCommand {
    Pod {
        name: String,
        container: Option<String>,
    },
    Workload {
        resource: String,
        name: String,
    },
    Selector,
}
impl LogCommand {
    fn parse(args: &[String], selector: &str, all_containers: bool) -> Result<Self> {
        if !selector.is_empty() {
            if !args.is_empty() {
                return Err("logs --selector takes no Pod, workload or container argument".into());
            }
            return Ok(Self::Selector);
        }
        if args.is_empty() || args.len() > 2 {
            return Err("usage: logs POD [CONTAINER] | logs RESOURCE/NAME | logs --selector LABELS; requires --allow-logs".into());
        }
        let (resource, name) = args[0].split_once('/').unwrap_or(("pods", &args[0]));
        if name.len() > 253 || !name.split('.').all(safety::valid_id) {
            return Err("invalid Pod/workload name".into());
        }
        let kind = crate::gateway::kinds()
            .into_iter()
            .find(|kind| {
                resource.eq_ignore_ascii_case(&kind.kind)
                    || resource == kind.plural
                    || resource == kind.key()
                    || kind.short_names.iter().any(|alias| alias == resource)
            })
            .ok_or("unsupported workload resource for logs")?;
        if kind.plural != "pods" {
            if args.len() != 1 {
                return Err("workload logs merge all ordinary containers; an explicit container is unsupported".into());
            }
            if !matches!(
                kind.plural.as_str(),
                "deployments"
                    | "statefulsets"
                    | "daemonsets"
                    | "replicasets"
                    | "replicationcontrollers"
                    | "jobs"
                    | "cronjobs"
                    | "services"
            ) {
                return Err(
                    "logs require a Pod, supported workload, Service or label selector".into(),
                );
            }
            return Ok(Self::Workload {
                resource: kind.key(),
                name: name.into(),
            });
        }
        if all_containers && args.len() > 1 {
            return Err("--all-containers cannot combine with an explicit container".into());
        }
        if args.get(1).is_some_and(|name| !safety::valid_id(name)) {
            return Err("invalid container name".into());
        }
        Ok(Self::Pod {
            name: name.into(),
            container: args.get(1).cloned(),
        })
    }
}
async fn logs_command(gateway: &Gateway, args: &[String], opts: &Options) -> Result<()> {
    use crate::logs::{FollowOptions, PodLogSelection};
    let namespace = &gateway.namespace;
    let request = LogCommand::parse(args, &opts.selector, opts.all_containers)?;
    let selection = match &request {
        LogCommand::Selector => Some(PodLogSelection::Selector(opts.selector.clone())),
        LogCommand::Workload { resource, name } => {
            let object = gateway.get(resource, namespace, name).await?;
            let uid = object["metadata"]["uid"]
                .as_str()
                .ok_or("Workload UID unavailable")?
                .to_owned();
            Some(PodLogSelection::Workload {
                resource: resource.clone(),
                name: name.clone(),
                uid,
            })
        }
        _ => None,
    };
    if selection.is_none() && opts.dynamic_membership {
        return Err("--dynamic-membership requires a workload or --selector, not one Pod".into());
    }
    if let Some(selection) = selection {
        let membership = gateway
            .capture_log_membership(namespace, selection.clone())
            .await?;
        let membership = if opts.dynamic_membership {
            membership.into_dynamic(selection)
        } else {
            membership
        };
        let description = membership.description();
        if !opts.json {
            outputln!("{description}");
        }
        if opts.follow {
            let follow = FollowOptions {
                seconds: opts.log_seconds,
            };
            let mut feed = if membership.is_dynamic() {
                crate::logs::WorkloadLogFeed::Dynamic(Box::new(
                    gateway
                        .follow_dynamic_logs(&membership, follow, opts.log_read)
                        .await?,
                ))
            } else {
                crate::logs::WorkloadLogFeed::Merged(
                    gateway
                        .follow_membership_logs(&membership, follow, opts.log_read)
                        .await?,
                )
            };
            let pod_scope = if membership.is_dynamic() {
                "dynamic-membership"
            } else {
                "fixed-membership"
            };
            while let Some(line) = feed.next().await? {
                let record = if opts.json {
                    serde_json::json!({"line":line,"container_scope":"all-ordinary","pod_scope":pod_scope,"membership":membership}).to_string()
                } else {
                    line
                };
                if let Err(error) = writeln!(io::stdout().lock(), "{record}") {
                    if error.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    return Err("log output failed".into());
                }
            }
        } else {
            let logs = gateway
                .membership_logs(&membership, opts.previous, opts.log_read)
                .await?;
            if opts.json {
                print_json(
                    &serde_json::json!({"logs":logs,"bounded":true,"container_scope":"all-ordinary","pod_scope":"fixed-membership","membership":membership}),
                )?;
            } else {
                outputln!("{logs}");
            }
        }
        return Ok(());
    }
    let LogCommand::Pod { name, container } = request else {
        return Err("invalid log target".into());
    };
    if opts.all_containers {
        let pod = gateway.get("pods", namespace, &name).await?;
        let uid = pod["metadata"]["uid"]
            .as_str()
            .ok_or("Pod UID unavailable; merged logs require captured identity")?;
        if opts.follow {
            let mut feed = gateway
                .follow_merged_logs(
                    namespace,
                    &name,
                    uid,
                    FollowOptions {
                        seconds: opts.log_seconds,
                    },
                    opts.log_read,
                )
                .await?;
            while let Some(line) = feed.next().await? {
                let record = if opts.json {
                    serde_json::json!({"line":line,"container_scope":"all-ordinary","namespace":namespace,"pod":name,"uid":uid}).to_string()
                } else {
                    line
                };
                if let Err(error) = writeln!(io::stdout().lock(), "{record}") {
                    if error.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    return Err("log output failed".into());
                }
            }
        } else {
            let logs = gateway
                .merged_logs(namespace, &name, uid, opts.previous, opts.log_read)
                .await?;
            if opts.json {
                print_json(
                    &serde_json::json!({"logs":logs,"bounded":true,"container_scope":"all-ordinary","namespace":namespace,"pod":name,"uid":uid}),
                )?;
            } else {
                outputln!("{logs}");
            }
        }
    } else if opts.follow {
        let mut feed = gateway
            .follow_logs_with_read_options(
                namespace,
                &name,
                container,
                false,
                FollowOptions {
                    seconds: opts.log_seconds,
                },
                opts.log_read,
            )
            .await?;
        while let Some(line) = feed.next().await? {
            let line = if opts.json {
                serde_json::json!({"line":line}).to_string()
            } else {
                line
            };
            if let Err(error) = writeln!(io::stdout().lock(), "{line}") {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    return Ok(());
                }
                return Err("log output failed".into());
            }
        }
    } else {
        let logs = tokio::time::timeout(
            Duration::from_secs(15),
            gateway.logs_with_options(namespace, &name, container, opts.previous, opts.log_read),
        )
        .await
        .map_err(|_| "logs timed out")??;
        if opts.json {
            print_json(&serde_json::json!({"logs":logs,"bounded":true}))?;
        } else {
            outputln!("{logs}");
        }
    }
    Ok(())
}
async fn helm_mutation_command(
    gateway: &Gateway,
    command: HelmCommand,
    opts: &Options,
) -> Result<()> {
    use crate::helm_lifecycle::{self, Request};
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "Helm lifecycle actions require interactive terminal review and confirmation".into(),
        );
    }
    let (release, request) = match command {
        HelmCommand::Install(name, chart) => (
            name,
            Request::Install {
                chart,
                values: opts.helm_values.clone(),
            },
        ),
        HelmCommand::Upgrade(name, chart) => (
            name,
            Request::Upgrade {
                chart,
                values: opts.helm_values.clone(),
            },
        ),
        HelmCommand::Crds(name, chart) => (name, Request::InstallCrds { chart }),
        HelmCommand::UpgradeCrds(name, chart) => (name, Request::UpgradeCrds { chart }),
        HelmCommand::DeleteCrds(name, chart) => (name, Request::DeleteCrds { chart }),
        HelmCommand::MigrateCrds(name, chart) => (name, Request::MigrateCrds { chart }),
        HelmCommand::Test(name) => (name, Request::Test),
        HelmCommand::Rollback(name, revision) => (name, Request::Rollback { revision }),
        HelmCommand::Uninstall(name) => (name, Request::Uninstall),
        _ => return Err("invalid Helm lifecycle action".into()),
    };
    let executable = opts
        .helm_tool
        .clone()
        .ok_or("Helm lifecycle requires --helm-tool ABSOLUTE_PATH")?;
    let preparation = helm_lifecycle::prepare(
        gateway,
        helm_lifecycle::Options {
            executable,
            namespace: gateway.namespace.clone(),
            release,
            storage: opts.helm_storage.unwrap_or(crate::helm::Storage::Secret),
            seconds: opts.helm_timeout_seconds,
            allow_hooks: opts.connection.allow_helm_hooks,
            allow_crds: opts.connection.allow_helm_crds,
            keyring: opts.helm_keyring.clone(),
            sigstore_policy: opts.helm_sigstore_policy.clone(),
            registry: crate::helm_oci::Options {
                ca_file: opts.helm_registry_ca.clone(),
                auth_file: opts.helm_registry_auth.clone(),
            },
        },
        request,
    );
    let prepared = tokio::select! {
        result = preparation => result?,
        _ = tokio::signal::ctrl_c() => return Err("Helm preparation canceled; no lifecycle action was submitted".into()),
    };
    outputln!("{}", prepared.summary());
    output!("> ");
    io::stdout().flush().map_err(|_| "output failed")?;
    let confirmation = helm_terminal_confirmation().await?;
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let operation = helm_lifecycle::run(gateway, prepared, &confirmation, Some(cancel_rx));
    tokio::pin!(operation);
    let outcome = tokio::select! {
        result = &mut operation => result?,
        _ = tokio::signal::ctrl_c() => {
            let _ = cancel_tx.send(());
            eprintln!("Canceling Helm; waiting for process cleanup and outcome...");
            operation.await?
        },
    };
    outputln!("{}", outcome.summary);
    if !outcome.success {
        return Err("Helm action did not complete successfully; inspect the reported release state before retrying".into());
    }
    Ok(())
}
async fn helm_terminal_confirmation() -> Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    struct RawInput;
    impl Drop for RawInput {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    crossterm::terminal::enable_raw_mode().map_err(|_| "cannot read terminal confirmation")?;
    let raw = RawInput;
    let mut input = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(15 * 60);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("Helm confirmation expired; prepare again".into());
        }
        if event::poll(Duration::ZERO).map_err(|_| "terminal confirmation failed")?
            && let Event::Key(key) = event::read().map_err(|_| "terminal confirmation failed")?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Esc => return Err("Helm action canceled before submission".into()),
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err("Helm action canceled before submission".into());
                }
                KeyCode::Enter => {
                    drop(raw);
                    outputln!("");
                    return Ok(input);
                }
                KeyCode::Backspace => {
                    if input.pop().is_some() {
                        output!("\x08 \x08");
                    }
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && ch.is_ascii()
                        && !ch.is_control()
                        && input.len() < 253 =>
                {
                    input.push(ch);
                    output!("{ch}");
                }
                _ => {}
            }
            io::stdout().flush().map_err(|_| "output failed")?;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {},
            _ = tokio::signal::ctrl_c() => return Err("Helm action canceled before submission".into()),
        }
    }
}
fn write_args<'a>(
    args: &'a [String],
    count: usize,
    usage: &str,
) -> Result<(&'a str, &'a [String])> {
    match args.len() {
        n if n == count => Ok(("deployments", args)),
        n if n == count + 1 => Ok((&args[0], &args[1..])),
        _ => Err(format!("usage: {usage}")),
    }
}
#[derive(Debug, PartialEq, Eq)]
enum HelmCommand {
    List,
    History(String),
    Detail(String, u32),
    Resources(String, u32),
    Values(String, u32),
    Install(String, PathBuf),
    Upgrade(String, PathBuf),
    Crds(String, PathBuf),
    UpgradeCrds(String, PathBuf),
    DeleteCrds(String, PathBuf),
    MigrateCrds(String, PathBuf),
    Test(String),
    Rollback(String, u32),
    Uninstall(String),
}
impl HelmCommand {
    fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::Install(..)
                | Self::Upgrade(..)
                | Self::Crds(..)
                | Self::UpgradeCrds(..)
                | Self::DeleteCrds(..)
                | Self::MigrateCrds(..)
                | Self::Test(..)
                | Self::Rollback(..)
                | Self::Uninstall(..)
        )
    }
    fn parse(command: &str, args: &[String]) -> Result<Self> {
        let usage = "usage: helm [list] | helm history RELEASE | helm detail|resources|values RELEASE REVISION | helm install|upgrade|crds|crds-upgrade|crds-delete|crds-migrate RELEASE CHART.tgz_OR_OCI | helm rollback RELEASE REVISION | helm uninstall|test RELEASE (lifecycle requires --write --allow-helm --allow-helm-write --helm-tool ABS; schema actions need --allow-helm-crds; test needs --allow-helm-hooks)";
        let name = |name: &String| {
            if crate::helm::valid_release_name(name) {
                Ok(name.clone())
            } else {
                Err("Invalid Helm release name.".to_owned())
            }
        };
        let revision = |value: &String| {
            value
                .parse::<u32>()
                .ok()
                .filter(|revision| *revision > 0)
                .ok_or_else(|| "Helm revision must be a positive integer.".to_owned())
        };
        if command == "history" {
            return match args {
                [release] => Ok(Self::History(name(release)?)),
                _ => Err(usage.into()),
            };
        }
        match args {
            [] => Ok(Self::List),
            [list] if list == "list" => Ok(Self::List),
            [history, release] if history == "history" => Ok(Self::History(name(release)?)),
            [mode, release] if mode == "uninstall" => Ok(Self::Uninstall(name(release)?)),
            [mode, release] if mode == "test" => Ok(Self::Test(name(release)?)),
            [mode, release, chart]
                if matches!(
                    mode.as_str(),
                    "install"
                        | "upgrade"
                        | "crds"
                        | "crds-upgrade"
                        | "crds-delete"
                        | "crds-migrate"
                ) =>
            {
                if chart.starts_with("oci://") {
                    crate::helm_oci::Reference::parse(chart)?;
                } else if chart.is_empty() || chart.contains("://") || !chart.ends_with(".tgz") {
                    return Err("Helm charts require a local packaged .tgz file or oci:// reference with a tag or sha256 digest".into());
                }
                let release = name(release)?;
                let chart = PathBuf::from(chart);
                Ok(match mode.as_str() {
                    "install" => Self::Install(release, chart),
                    "upgrade" => Self::Upgrade(release, chart),
                    "crds" => Self::Crds(release, chart),
                    "crds-upgrade" => Self::UpgradeCrds(release, chart),
                    "crds-migrate" => Self::MigrateCrds(release, chart),
                    _ => Self::DeleteCrds(release, chart),
                })
            }
            [mode, release, rev] => {
                let release = name(release)?;
                let revision = revision(rev)?;
                match mode.as_str() {
                    "detail" => Ok(Self::Detail(release, revision)),
                    "resources" => Ok(Self::Resources(release, revision)),
                    "values" => Ok(Self::Values(release, revision)),
                    "rollback" => Ok(Self::Rollback(release, revision)),
                    _ => Err(usage.into()),
                }
            }
            _ => Err(usage.into()),
        }
    }
}
async fn helm_command(
    gateway: &Gateway,
    command: &str,
    args: &[String],
    opts: &Options,
) -> Result<()> {
    use crate::helm;
    let request = HelmCommand::parse(command, args)?;
    if request.is_mutation() {
        return helm_mutation_command(gateway, request, opts).await;
    }
    let latest = helm::list_with_storage(gateway, &gateway.namespace, opts.helm_storage).await?;
    let envelope = |kind: &str, data: serde_json::Value| serde_json::json!({"schema_version":1,"kind":kind,"bounded":true,"data":data});
    if request == HelmCommand::List {
        if opts.json {
            print_json(&envelope(
                "helm-list",
                serde_json::to_value(&latest).map_err(|_| "Helm list encoding failed")?,
            ))?;
        } else {
            outputln!("{}", latest.text());
        }
        return Ok(());
    }
    let release_name = match &request {
        HelmCommand::History(name)
        | HelmCommand::Detail(name, _)
        | HelmCommand::Resources(name, _)
        | HelmCommand::Values(name, _) => name,
        _ => return Err("Invalid Helm read request".into()),
    };
    let release = helm::select_release(&latest, release_name, opts.helm_storage)?;
    let history = helm::history(gateway, &release).await?;
    if matches!(request, HelmCommand::History(_)) {
        if opts.json {
            print_json(&envelope(
                "helm-history",
                serde_json::to_value(&history).map_err(|_| "Helm history encoding failed")?,
            ))?;
        } else {
            outputln!(
                "HELM HISTORY {}/{} | {} storage\n{}",
                release.namespace,
                release.name,
                release.storage.name(),
                history.text()
            );
        }
        return Ok(());
    }
    let revision = match request {
        HelmCommand::Detail(_, revision)
        | HelmCommand::Resources(_, revision)
        | HelmCommand::Values(_, revision) => revision,
        _ => return Err("Invalid Helm revision request".into()),
    };
    if history.truncated || !history.issues.is_empty() {
        return Err("Helm revision history is incomplete; narrow the namespace/storage and retry before selecting values.".into());
    }
    let selected = history
        .releases
        .iter()
        .find(|release| release.revision == revision)
        .ok_or("Requested Helm revision not found in the selected namespace and storage driver.")?;
    if matches!(request, HelmCommand::Values(..)) {
        let values = helm::values(gateway, selected).await?;
        if opts.json {
            print_json(&envelope(
                "helm-values",
                serde_json::to_value(&values).map_err(|_| "Helm values encoding failed")?,
            ))?;
        } else {
            outputln!("{}", values.text()?);
        }
    } else {
        let detail = helm::detail(gateway, selected).await?;
        let kind = if matches!(request, HelmCommand::Resources(..)) {
            "helm-resources"
        } else {
            "helm-detail"
        };
        if opts.json {
            print_json(&envelope(
                kind,
                serde_json::to_value(&detail).map_err(|_| "Helm revision encoding failed")?,
            ))?;
        } else {
            outputln!("{}", detail.text());
        }
    }
    Ok(())
}
fn print_json(value: &serde_json::Value) -> Result<()> {
    let encoded = serde_json::to_string_pretty(value).map_err(|_| "JSON serialization failed")?;
    write_text(&format!("{encoded}\n"))
}
fn copilot_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    match args.first().map(String::as_str) {
        Some("diagnose") if args.len() == 3 => {
            if opts.json || !io::stdin().is_terminal() || !io::stdout().is_terminal() { return Err("Copilot diagnosis requires an interactive transfer review; --json is unavailable".into()); }
            let path = opts.copilot_policy.as_ref().ok_or("Copilot diagnosis requires --copilot-policy ABS; see docs/COPILOT_INTEGRATION.md")?;
            let options = crate::advice::load_options(path)?;
            runtime()?.block_on(async {
                let mut gateway = tokio::time::timeout(Duration::from_secs(20),Gateway::connect(opts.connection.clone())).await.map_err(|_|"connection timed out")??;
                if gateway.is_demo() { return Err("Demo diagnosis is local preview only; use copilot preview".into()); }
                discover_if_needed(&mut gateway,&args[1]).await?;
                let object = tokio::time::timeout(Duration::from_secs(15),gateway.get(&args[1],&gateway.namespace,&args[2])).await.map_err(|_|"diagnostic read timed out")??;
                let prepared = crate::advice::prepare(&object,options).await?;
                outputln!("{}\n\nEXACT DIAGNOSTIC PAYLOAD\n{}",prepared.summary(),prepared.preview()?);
                output!("> "); io::stdout().flush().map_err(|_|"output failed")?;
                let typed = helm_terminal_confirmation().await?;
                let (cancel,receiver) = tokio::sync::oneshot::channel();
                let operation = crate::advice::run(prepared,&typed,Some(receiver));
                tokio::pin!(operation);
                let result = tokio::select! {
                    result = &mut operation => result?,
                    _ = tokio::signal::ctrl_c() => { let _=cancel.send(()); operation.await? },
                };
                outputln!("COPILOT ADVICE / review independently\n{}",safety::safe_lines(&result.answer,64*1024));
                Ok(())
            })
        }
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
            outputln!("Use copilot diagnose with an explicit --copilot-policy for a separately approved advisory request.");
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
        _ => Err("usage: copilot diagnose RESOURCE NAME --copilot-policy ABS | copilot preflight --cli ABS_NATIVE --host HOST [--expected-sha256 SHA] [--credential-file FILE] | copilot preview RESOURCE NAME --host HOST | copilot login --cli <absolute executable> --host <github.com|org.ghe.com> | copilot context <resource> <name> --output <new JSON file>".into()),
    }
}
async fn discover_if_needed(gateway: &mut Gateway, resource: &str) -> Result<()> {
    if resource.is_empty()
        || resource.len() > 253
        || !resource.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'/')
        })
        || resource.split('.').next() == Some("secrets")
    {
        return Err("invalid or disabled resource kind".into());
    }
    let known = |g: &Gateway| g.find(resource).is_ok();
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
        Some("import-k9s") if args.len()==2 && !opts.json => {
            let path=std::path::Path::new(&args[1]);
            let bytes=safety::read_small(path,256*1024)?;
            output!("{}",crate::extensions::import_k9s(std::str::from_utf8(&bytes).map_err(|_|"plugins.yaml must be UTF-8")?)?);
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
        _=>Err("usage: extensions list [--json] | register NAME --tool ABS [--tool-arg ARG] | remove NAME | run NAME RESOURCE OBJECT --allow-extensions --allow-tools | import-k9s PLUGINS.yaml".into()),
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
            themes::use_theme(themes::DEFAULT_THEME)?;
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
    fn all_namespaces_spellings_select_every_namespace() -> Result<()> {
        for args in [["-n", "all"], ["--namespace", "all"], ["-A", "pods"]] {
            let opts = parse(args.iter().map(|a| (*a).to_owned()).collect())?;
            assert_eq!(opts.connection.namespace, "*", "{args:?}");
        }
        assert_eq!(
            parse(vec!["-n".into(), "shop".into()])?
                .connection
                .namespace,
            "shop"
        );
        Ok(())
    }
    #[test]
    fn node_commands_route_to_terminal_approval_without_any_live_connection() {
        for command in ["cordon", "uncordon", "drain"] {
            let error = run_args(vec!["--demo".into(), command.into(), "node".into()])
                .err()
                .unwrap_or_default();
            assert!(
                error.contains("interactive terminal approval"),
                "{command}: {error}"
            );
        }
    }
    #[test]
    fn explicit_signature_and_advice_policies_have_strict_scope() -> Result<()> {
        assert!(
            parse(
                "--helm-sigstore-policy relative.json ui"
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            )
            .is_err()
        );
        assert!(
            parse(
                "--helm-sigstore-policy /tmp/p.json list pods"
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            )
            .is_err()
        );
        assert!(
            parse(
                "--copilot-policy /tmp/p.json list pods"
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            )
            .is_err()
        );
        assert!(
            parse(
                "--json helm crds-migrate web /tmp/chart.tgz"
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            )
            .is_err()
        );
        let options = parse(
            "--copilot-policy /tmp/p.json copilot diagnose pods web"
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )?;
        assert_eq!(options.copilot_policy, Some("/tmp/p.json".into()));
        let options = parse(
            "--helm-sigstore-policy /tmp/p.json helm crds-migrate web /tmp/chart.tgz"
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
        )?;
        assert_eq!(options.helm_sigstore_policy, Some("/tmp/p.json".into()));
        assert!(HelmCommand::parse("helm", &options.command[1..])?.is_mutation());
        Ok(())
    }
    #[test]
    fn oci_and_helm_extension_flags_validate_before_connecting() -> Result<()> {
        let pinned = format!(
            "oci://registry.example:5443/team/chart@sha256:{}",
            "a".repeat(64)
        );
        for mode in [
            "install",
            "upgrade",
            "crds",
            "crds-upgrade",
            "crds-delete",
            "crds-migrate",
        ] {
            let options = parse(vec![
                "--allow-helm-hooks".into(),
                "--allow-helm-crds".into(),
                "--helm-registry-ca".into(),
                "/tmp/registry-ca.pem".into(),
                "--helm-registry-auth".into(),
                "/tmp/registry-auth.json".into(),
                "helm".into(),
                mode.into(),
                "my.api".into(),
                pinned.clone(),
            ])?;
            assert!(options.connection.allow_helm_hooks);
            assert!(options.connection.allow_helm_crds);
            assert_eq!(
                options.helm_registry_ca,
                Some("/tmp/registry-ca.pem".into())
            );
            assert_eq!(
                options.helm_registry_auth,
                Some("/tmp/registry-auth.json".into())
            );
            let command = HelmCommand::parse("helm", &options.command[1..])?;
            assert!(command.is_mutation());
            match command {
                HelmCommand::Install(name, chart)
                | HelmCommand::Upgrade(name, chart)
                | HelmCommand::Crds(name, chart)
                | HelmCommand::UpgradeCrds(name, chart)
                | HelmCommand::DeleteCrds(name, chart)
                | HelmCommand::MigrateCrds(name, chart) => {
                    assert_eq!(name, "my.api");
                    assert_eq!(chart.to_str(), Some(pinned.as_str()));
                }
                _ => return Err("unexpected chart command".into()),
            }
        }
        for command in [
            "helm install web oci://registry.example/team/chart",
            "--allow-helm-hooks list pods",
            "--allow-helm-crds history web",
            "--helm-registry-ca relative.pem helm list",
            "--helm-registry-auth relative.json helm list",
            "--helm-keyring relative.gpg helm list",
            "--helm-keyring /tmp/public.gpg list pods",
            "--json helm test web",
            "--json helm crds-delete web /tmp/chart.tgz",
            "--helm-values /tmp/values.yaml helm crds-upgrade web /tmp/chart.tgz",
            "--helm-values /tmp/values.yaml helm crds web /tmp/chart.tgz",
            "--json helm crds web /tmp/chart.tgz",
        ] {
            assert!(
                parse(command.split_whitespace().map(str::to_owned).collect()).is_err(),
                "{command}"
            );
        }
        let tagged = parse("--helm-keyring /tmp/public.gpg helm install web oci://registry.example/team/chart:1.2.3".split_whitespace().map(str::to_owned).collect())?;
        assert_eq!(tagged.helm_keyring, Some("/tmp/public.gpg".into()));
        assert_eq!(
            HelmCommand::parse("helm", &["test".into(), "web".into()])?,
            HelmCommand::Test("web".into())
        );
        let defaults = parse(vec!["ui".into()])?;
        assert!(!defaults.connection.allow_helm_hooks);
        assert!(!defaults.connection.allow_helm_crds);
        Ok(())
    }
    #[test]
    fn lifecycle_and_workload_scopes_validate_before_connecting() -> Result<()> {
        let words = |s: &str| parse(s.split_whitespace().map(str::to_owned).collect());
        let options = words(
            "--write --allow-helm --allow-helm-write --helm-tool /tmp/helm --helm-values /tmp/values.yaml helm install web ./web.tgz",
        )?;
        assert!(options.connection.allow_helm_write);
        assert_eq!(options.helm_timeout_seconds, 300);
        assert_eq!(options.helm_values, Some(PathBuf::from("/tmp/values.yaml")));
        assert_eq!(
            HelmCommand::parse("helm", &options.command[1..])?,
            HelmCommand::Install("web".into(), PathBuf::from("./web.tgz"))
        );
        assert!(
            matches!(LogCommand::parse(&["deploy/web".into()],"",false)?,LogCommand::Workload{resource,..} if resource == "deployments.apps")
        );
        assert_eq!(
            LogCommand::parse(&[], "app=web", false)?,
            LogCommand::Selector
        );
        assert!(matches!(
            LogCommand::parse(&["pod/web".into(), "app".into()], "", false)?,
            LogCommand::Pod { .. }
        ));
        for command in [
            "helm rollback web 0",
            "helm install web https://example.org/chart.tgz",
            "helm install web ./chart",
            "--json helm uninstall web",
            "--helm-tool helm helm list",
            "--helm-timeout 0 helm list",
            "--helm-timeout 901 helm list",
            "--helm-values values.yaml helm rollback web 1",
            "--helm-tool /tmp/helm list pods",
            "logs deploy/web app",
            "logs web --selector app=web",
            "logs nodes/node",
            "logs deploy/a/b",
            "logs --selector app=((bad))",
        ] {
            assert!(words(command).is_err(), "{command}");
        }
        assert!(words("logs --selector app=web --all-containers --follow").is_ok());
        assert!(!words("helm list")?.connection.allow_helm_write);
        assert!(words("logs pod/api.worker --all-containers").is_ok());
        Ok(())
    }
    #[test]
    fn helm_and_merged_log_flags_have_explicit_scopes() -> Result<()> {
        let options = parse(
            [
                "--allow-helm",
                "--helm-storage",
                "configmap",
                "--json",
                "helm",
                "values",
                "my.api",
                "2",
            ]
            .map(String::from)
            .to_vec(),
        )?;
        assert!(options.connection.allow_helm);
        assert_eq!(options.helm_storage, Some(crate::helm::Storage::ConfigMap));
        assert_eq!(
            HelmCommand::parse("helm", &options.command[1..])?,
            HelmCommand::Values("my.api".into(), 2)
        );
        assert!(
            parse(
                ["--helm-storage", "invalid", "helm"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
        assert!(parse(["helm", "values", "web", "0"].map(String::from).to_vec()).is_err());
        assert!(
            parse(
                ["--all-containers", "logs", "web", "app"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
        assert!(
            parse(
                ["--all-containers", "list", "pods"]
                    .map(String::from)
                    .to_vec()
            )
            .is_err()
        );
        assert!(
            parse(
                ["--all-containers", "--allow-logs", "logs", "web"]
                    .map(String::from)
                    .to_vec()
            )?
            .all_containers
        );
        assert!(!parse(vec![])?.connection.allow_helm);
        Ok(())
    }
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
    fn label_and_log_ranges_validate_before_connecting() -> Result<()> {
        let parse_words = |s: &str| parse(s.split_whitespace().map(str::to_owned).collect());
        let options = parse_words("list po --selector app=web,tier!=batch")?;
        assert_eq!(options.selector, "app=web,tier!=batch");
        let options = parse_words("logs pod --since-seconds 300 --tail 500")?;
        assert_eq!(options.log_read.since_seconds, Some(300));
        for command in [
            "list pods --selector app=((web))",
            "logs pod --tail 0",
            "logs pod --head --follow",
            "list pods --head",
            "metrics pods --selector app=web",
        ] {
            assert!(parse_words(command).is_err(), "{command}");
        }
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
