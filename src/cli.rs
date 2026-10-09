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

#[derive(Clone)]
pub struct Options {
    pub connection: Connection,
    pub theme: Option<String>,
    pub color: bool,
    pub json: bool,
    pub splash: bool,
    pub poll: bool,
    pub command: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut opts = Options {
        connection: Connection::default(),
        theme: None,
        color: std::env::var_os("NO_COLOR").is_none(),
        json: false,
        splash: std::env::var_os("K8BORG_NO_SPLASH").is_none(),
        poll: false,
        command: vec![],
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            "--demo" => opts.connection.demo = true,
            "--json" => opts.json = true,
            "--no-splash" => opts.splash = false,
            "--poll" => opts.poll = true,
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
    if !opts
        .command
        .first()
        .is_some_and(|c| matches!(c.as_str(), "theme" | "copilot"))
        && opts.command.iter().any(|s| s.starts_with('-'))
    {
        return Err("unknown option; see --help".into());
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
    let cmd = opts.command.first().map(String::as_str).unwrap_or("ui");
    match cmd {
        "help" => {
            println!("{}", include_str!("../docs/CLI_HELP.txt"));
            return Ok(());
        }
        "version" => {
            println!("K8Borg {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "theme" => return theme_command(&opts),
        "copilot" => return copilot_command(&opts),
        "startup" => {
            match opts.command.get(1).map(String::as_str) {
                Some("on") => themes::set_animation(true)?,
                Some("off") => themes::set_animation(false)?,
                _ => return Err("usage: startup on|off".into()),
            }
            println!("Startup preference saved.");
            return Ok(());
        }
        "contexts" => {
            let list = contexts(&opts.connection)?;
            if opts.json {
                print_json(&serde_json::json!(list))?;
            } else {
                for c in list {
                    println!("{}", safety::safe_text(&c, 100));
                }
            }
            return Ok(());
        }
        "ui" => return tui::run(opts),
        "list" | "watch" | "metrics" | "get" | "logs" | "resources" | "doctor" | "scale"
        | "restart" | "delete" => {}
        _ => return Err("unknown command; see --help".into()),
    }
    let rt = runtime()?;
    rt.block_on(async {
        let mut gateway=tokio::time::timeout(Duration::from_secs(15),Gateway::connect(opts.connection.clone())).await.map_err(|_|"connection timed out")??;
        let ns=gateway.namespace.clone();
        let args=&opts.command[1..];
        if matches!(cmd, "list" | "get" | "watch") && let Some(resource) = args.first() {
            discover_if_needed(&mut gateway, resource).await?;
        }
        match cmd {
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
                let value=crate::metrics::collect(&gateway,crate::metrics::MetricsScope::parse(&args[0])?,&ns).await?;
                if opts.json {print_json(&serde_json::to_value(value).map_err(|_| "metrics output failed")?)?;} else {print!("{}",value.text());}
            },
            "doctor"=>{println!("K8Borg {}\ncontext: {}\nserver: {}\nnamespace: {}\nmode: {}\nauth: certificate/token; helpers blocked\nAI/plugins/exec/forwarding: disabled",env!("CARGO_PKG_VERSION"),safety::safe_text(&gateway.context,100),safety::safe_text(&gateway.server,200),safety::safe_text(&ns,63),if opts.connection.writable{"WRITE ENABLED"}else{"READ ONLY"});
                let result=tokio::time::timeout(Duration::from_secs(15),gateway.list("pods",&ns)).await.map_err(|_|"API timed out")??;
                println!("API pods read: OK ({} objects)",result["items"].as_array().map_or(0,Vec::len));},
            "resources"=>{let resources=tokio::time::timeout(Duration::from_secs(15),gateway.discover()).await.map_err(|_|"discovery timed out")??; if opts.json{print_json(&serde_json::to_value(resources).map_err(|_|"serialization failed")?)?;}else{for kind in resources{println!("{}\t{}",kind.key(),if kind.namespaced{"namespaced"}else{"cluster"});}}},
            "list"=>{if args.len()!=1{return Err("usage: list <resource>".into());}let value=tokio::time::timeout(Duration::from_secs(15),gateway.list(&args[0],&ns)).await.map_err(|_|"API timed out")??;
                if opts.json {print_json(&value)?;} else {println!("NAMESPACE\tNAME\tSTATUS");if let Some(items)=value["items"].as_array(){for v in items{println!("{}\t{}\t{}",safety::safe_text(v["metadata"]["namespace"].as_str().unwrap_or("-"),63),safety::safe_text(v["metadata"]["name"].as_str().unwrap_or("-"),253),safety::safe_text(&summary(v),100));}}
if value["truncated"]==true {eprintln!("TRUNCATED: limit reached; narrow namespace scope");}}},
            "get"=>{if args.len()!=2{return Err("usage: get <resource> <name>".into());}let value=tokio::time::timeout(Duration::from_secs(15),gateway.get(&args[0],&ns,&args[1])).await.map_err(|_|"API timed out")??; print_json(&value)?;},
            "logs"=>{if args.is_empty() || args.len()>2{return Err("usage: logs <pod> [container] (requires --allow-logs)".into());}let logs=tokio::time::timeout(Duration::from_secs(15),gateway.logs(&ns,&args[0],args.get(1).cloned(),false)).await.map_err(|_|"logs timed out")??;if opts.json{print_json(&serde_json::json!({"logs":logs,"bounded":true}))?;}else{println!("{logs}");}},
            "scale"|"restart"|"delete"=>{
                if !io::stdin().is_terminal() || !io::stdout().is_terminal(){return Err("noninteractive writes are disabled; use the interactive TUI or terminal confirmation".into());}
                let (resource,name,action)=match cmd {
                    "scale"=>{if args.len()!=2{return Err("usage: scale <deployment> <replicas> --write".into());}("deployments",args[0].as_str(),Action::Scale(args[1].parse().map_err(|_|"invalid replicas")?))},
                    "restart"=>{if args.len()!=1{return Err("usage: restart <deployment> --write".into());}("deployments",args[0].as_str(),Action::Restart)},
                    _=>{if args.len()!=2{return Err("usage: delete <resource> <name> --write".into());}(args[0].as_str(),args[1].as_str(),Action::Delete)},
                };
                let prepared=tokio::time::timeout(Duration::from_secs(15),gateway.prepare(resource,&ns,name,action)).await.map_err(|_|"dry-run timed out")??;
                println!("{}",safety::safe_lines(&prepared.summary,4000));print!("> ");io::stdout().flush().map_err(|_|"output failed")?;
                let mut input=String::new();use std::io::BufRead;io::stdin().lock().take(256).read_line(&mut input).map_err(|_|"input failed")?;
                let approval=Approval::confirm(prepared.target.clone(),prepared.action.clone(),input.trim_end())?;
                println!("{}",gateway.execute(prepared,approval).await?);
            },
            _=>{},
        } Ok(())
    })
}
fn print_json(value: &serde_json::Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|_| "JSON serialization failed")?
    );
    Ok(())
}

fn copilot_command(opts: &Options) -> Result<()> {
    let args = &opts.command[1..];
    match args.first().map(String::as_str) {
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
            println!("Minimized context saved locally. Inspect the JSON before manually sharing with GitHub Copilot. No AI request was made.");
            Ok(())
        }
        _ => Err("usage: copilot login --cli <absolute executable> --host <github.com|org.ghe.com> | copilot context <resource> <name> --output <new JSON file>".into()),
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
                println!(
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
            println!("Theme saved.");
        }
        "reset" => {
            themes::use_theme("catppuccin-mocha")?;
            println!("Appearance reset.");
        }
        "new" => {
            let base = if args.get(2).is_some_and(|s| s == "--base") {
                get(3)?
            } else {
                "k8borg-dark"
            };
            println!("{}", themes::new_theme(get(1)?, base)?.display());
        }
        "validate" => {
            themes::load_file(&PathBuf::from(get(1)?))?;
            println!("VALID: schema, colors and contrast checked.");
        }
        "preview" => {
            let theme = themes::load_file(&PathBuf::from(get(1)?))?.resolve()?;
            tui::preview(theme, opts.color)?;
        }
        "install" => {
            let id = themes::install(&PathBuf::from(get(1)?))?;
            println!("Installed {id}. Activate with: k8borg theme use {id}");
        }
        "export" => {
            if get(2)? != "--output" {
                return Err("usage: theme export <id> --output <path>".into());
            }
            themes::export(get(1)?, &PathBuf::from(get(3)?))?;
            println!("Theme exported.");
        }
        "remove" => {
            themes::remove(get(1)?)?;
            println!("Custom theme removed.");
        }
        _ => return Err("unknown theme command; see --help".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
