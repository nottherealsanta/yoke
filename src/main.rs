//! CLI entry: `yoke`, `yoke login <provider>`, `yoke logout <provider>`, `yoke status`.

mod auth;
mod clock;
mod context;
mod dotenv;
mod r#loop;
mod memory;
mod models;
mod plain;
mod prompt;
mod providers;
mod session;
mod tools;
mod tui;
mod types;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::SystemTime;

use auth::KeySource;
use providers::PROVIDERS;
use session::Session;
use types::Message;

const USAGE: &str = "\
Usage: yoke [options]
       yoke login <provider> [--device] [options]
       yoke logout <provider>
       yoke status

Options:
  --provider <id>  Provider id
  --model <name>   Model name
  --device         Use the device code for codex sign-in
  --yes            Skip approval prompts
  --plain          Plain line-mode REPL
  --resume         Resume the latest session
  -h, --help       Show this help";

#[derive(Debug, Default)]
struct Args {
    command: Command,
    provider: Option<String>,
    model: Option<String>,
    device: bool,
    yes: bool,
    plain: bool,
    resume: bool,
}

#[derive(Debug, Default)]
enum Command {
    #[default]
    Run,
    Login(String),
    Logout(String),
    Status,
    Help,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("yoke: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    load_dotenv();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("yoke: {err}\n\n{USAGE}");
            return Ok(ExitCode::FAILURE);
        }
    };

    match args.command {
        Command::Help => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Status => {
            status()?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Login(provider) => {
            if provider == "codex" {
                codex_login(args.device)?;
            } else {
                login(&provider)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Logout(provider) => {
            logout(&provider)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Run => {
            if args.plain {
                plain_mode(&args)?;
            } else {
                tui_mode(&args)?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Read `./.env` once at startup; existing process variables win.
fn load_dotenv() {
    let Ok(contents) = std::fs::read_to_string(Path::new(".env")) else {
        return;
    };
    for (key, value) in dotenv::parse(&contents) {
        if std::env::var_os(&key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut words = Vec::new();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => {
                parsed.provider = Some(
                    args.next()
                        .ok_or_else(|| "--provider needs a value".to_string())?,
                );
            }
            "--model" => {
                parsed.model = Some(
                    args.next()
                        .ok_or_else(|| "--model needs a value".to_string())?,
                );
            }
            "--yes" => parsed.yes = true,
            "--device" => parsed.device = true,
            "--plain" => parsed.plain = true,
            "--resume" => parsed.resume = true,
            "-h" | "--help" => parsed.command = Command::Help,
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => words.push(other.to_string()),
        }
    }

    if !matches!(parsed.command, Command::Help) {
        parsed.command = match words.as_slice() {
            [] => Command::Run,
            [command] if command == "status" => Command::Status,
            [command, provider] if command == "login" => Command::Login(provider.clone()),
            [command, provider] if command == "logout" => Command::Logout(provider.clone()),
            [command, ..] if command == "login" || command == "logout" => {
                return Err(format!("{command} needs exactly one provider"));
            }
            [command, ..] => return Err(format!("unknown command: {command}")),
        };
    }

    Ok(parsed)
}

fn status() -> anyhow::Result<()> {
    let home = yoke_home()?;
    let project = std::env::current_dir()?;

    println!("home: {}", home.display());
    println!("project slug: {}", memory::slug(&project));
    println!("providers:");
    for info in PROVIDERS {
        let stored = auth::stored_key(&home, info.id);
        let env = info.env.and_then(|name| std::env::var(name).ok());
        let base_url = if info.id == "compat" {
            auth::stored_base_url(&home, info.id)
                .or_else(|| std::env::var(providers::compat::ENV_BASE_URL).ok())
        } else {
            None
        };
        let state = provider_state(&home, info, stored, env, base_url);
        println!("  {:<9} {:<18} {state}", info.id, info.label);
    }
    Ok(())
}

fn provider_state(
    home: &Path,
    info: &providers::ProviderInfo,
    stored: Option<String>,
    env: Option<String>,
    base_url: Option<String>,
) -> String {
    if info.id == "codex" {
        return if auth::codex_logged_in(home) {
            "signed in"
        } else {
            "-"
        }
        .to_string();
    }
    if info.id == "compat" {
        return base_url.unwrap_or_else(|| "-".into());
    }
    match auth::ApiKeyAuth::new(stored, env) {
        Some(auth) => match auth.source() {
            KeySource::Stored => "api key (stored)",
            KeySource::Env => "api key (env)",
        },
        None => "-",
    }
    .to_string()
}

fn yoke_home() -> anyhow::Result<PathBuf> {
    let env_home = std::env::var("YOKE_HOME").ok();
    memory::home(env_home.as_deref())
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory; set YOKE_HOME"))
}

fn provider_info(id: &str) -> anyhow::Result<&'static providers::ProviderInfo> {
    PROVIDERS
        .iter()
        .find(|info| info.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown provider: {id}"))
}

fn login(provider: &str) -> anyhow::Result<()> {
    if provider == "compat" {
        return compat_login();
    }
    let info = provider_info(provider)?;
    let env = info
        .env
        .ok_or_else(|| anyhow::anyhow!("login for `{provider}` is not implemented yet"))?;

    print!("Paste {provider} API key: ");
    std::io::stdout().flush()?;
    let mut key = String::new();
    if std::io::stdin().read_line(&mut key)? == 0 {
        anyhow::bail!("no key read");
    }
    let key = key.trim();
    if key.is_empty() {
        anyhow::bail!("the key is empty");
    }
    if key.chars().any(char::is_whitespace) {
        anyhow::bail!("the key contains whitespace");
    }

    let home = yoke_home()?;
    auth::set_api_key(&home, provider, key)?;
    println!(
        "stored {provider} API key in {} (or set {env})",
        auth::auth_path(&home).display()
    );
    Ok(())
}

fn compat_login() -> anyhow::Result<()> {
    print!("Base URL (for example http://localhost:8000/v1): ");
    std::io::stdout().flush()?;
    let mut base = String::new();
    if std::io::stdin().read_line(&mut base)? == 0 {
        anyhow::bail!("no base URL read");
    }
    let base = providers::compat::normalize_base_url(&base)?;

    print!("API key (leave empty for none): ");
    std::io::stdout().flush()?;
    let mut key = String::new();
    std::io::stdin().read_line(&mut key)?;
    let key = key.trim();
    if key.chars().any(char::is_whitespace) {
        anyhow::bail!("the key contains whitespace");
    }

    let home = yoke_home()?;
    auth::set_endpoint(&home, "compat", &base, (!key.is_empty()).then_some(key))?;
    println!(
        "stored compat base URL {base} in {}",
        auth::auth_path(&home).display()
    );
    if key.is_empty() {
        println!(
            "no API key stored (or set {})",
            providers::compat::ENV_API_KEY
        );
    } else {
        println!(
            "stored the compat API key (or set {})",
            providers::compat::ENV_API_KEY
        );
    }
    Ok(())
}

fn codex_login(device: bool) -> anyhow::Result<()> {
    let home = yoke_home()?;
    let method = if device {
        providers::codex::Method::Device
    } else {
        providers::codex::Method::Browser
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(providers::codex::login(&home, method, |line| {
        println!("{line}");
    }))?;
    println!("signed in to codex");
    Ok(())
}

fn logout(provider: &str) -> anyhow::Result<()> {
    provider_info(provider)?;
    let home = yoke_home()?;
    if auth::remove(&home, provider)? {
        println!("logged out of {provider}");
    } else {
        println!("{provider} had no stored credentials");
    }
    Ok(())
}

fn plain_mode(args: &Args) -> anyhow::Result<()> {
    let home = yoke_home()?;
    let workspace = std::env::current_dir()?;
    let provider_id = args.provider.as_deref().unwrap_or("groq");
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| providers::default_model(provider_id).to_string());
    let dev = std::env::var("YOKE_DEV").ok();
    let provider = providers::make(provider_id, &model, dev.as_deref(), &home, |name| {
        std::env::var(name).ok()
    })?;
    let start = startup(&home, &workspace, args.resume)?;
    let options = r#loop::Options {
        yes: args.yes,
        conversation_id: start
            .session
            .path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string),
        ..r#loop::Options::new(model.clone(), start.instructions.clone())
    };

    println!(
        "yoke · {}/{model} · plain mode (/quit exits, Ctrl-C interrupts)",
        provider.id()
    );
    println!("session: {}", start.session.path().display());
    let mut plain = plain::Plain {
        provider,
        registry: tools::default_registry(),
        options,
        messages: start.messages,
        session: start.session,
    };
    plain.run()
}

/// Bare `yoke`: the TUI, with the same startup path as the plain REPL.
fn tui_mode(args: &Args) -> anyhow::Result<()> {
    let home = yoke_home()?;
    let workspace = std::env::current_dir()?;
    let slug = memory::slug(&workspace);
    let provider_id = args.provider.as_deref().unwrap_or("groq");
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| providers::default_model(provider_id).to_string());
    let dev = std::env::var("YOKE_DEV").ok();
    let provider = providers::make(provider_id, &model, dev.as_deref(), &home, |name| {
        std::env::var(name).ok()
    })?;
    let start = startup(&home, &workspace, args.resume)?;
    let options = r#loop::Options {
        yes: args.yes,
        conversation_id: start
            .session
            .path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string),
        ..r#loop::Options::new(model.clone(), start.instructions.clone())
    };
    let memory_path = memory::memory_path(&home, &slug);
    let setup = tui::Setup {
        provider,
        registry: tools::default_registry(),
        options,
        messages: start.messages,
        session: start.session,
        instructions: start.instructions,
        memory: start.memory,
        memory_path,
        home,
        slug,
        workspace,
        provider_id: provider_id.to_string(),
        model,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(tui::run(setup))
}

/// What the TUI or plain REPL needs at startup: the system prompt, the memory
/// text for context accounting, a new session file, and any resumed messages.
struct Startup {
    instructions: String,
    memory: String,
    session: Session,
    messages: Vec<Message>,
}

fn startup(home: &Path, workspace: &Path, resume: bool) -> anyhow::Result<Startup> {
    let slug = memory::slug(workspace);
    let memory_path = memory::memory_path(home, &slug);
    let memory = memory::read(&memory_path);
    let instructions = prompt::build(
        workspace,
        &clock::date(SystemTime::now()),
        &memory_path,
        &memory,
    );
    let messages = if resume {
        match session::latest(home, &slug) {
            Some(path) => session::load(&path)?,
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    Ok(Startup {
        instructions,
        memory,
        session: Session::create(home, &slug)?,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Result<Args, String> {
        parse_args(words.iter().map(|word| word.to_string()))
    }

    #[test]
    fn parses_commands() {
        assert!(matches!(parse(&[]).unwrap().command, Command::Run));
        assert!(matches!(
            parse(&["status"]).unwrap().command,
            Command::Status
        ));
        assert!(matches!(
            parse(&["login", "groq"]).unwrap().command,
            Command::Login(provider) if provider == "groq"
        ));
        assert!(matches!(
            parse(&["logout", "groq"]).unwrap().command,
            Command::Logout(provider) if provider == "groq"
        ));
    }

    #[test]
    fn parses_flags() {
        let args = parse(&[
            "--provider",
            "groq",
            "--model",
            "m",
            "--yes",
            "--plain",
            "--resume",
        ])
        .unwrap();
        assert_eq!(args.provider.as_deref(), Some("groq"));
        assert_eq!(args.model.as_deref(), Some("m"));
        assert!(args.yes && args.plain && args.resume);

        let args = parse(&["login", "codex", "--device"]).unwrap();
        assert!(args.device);
        assert!(matches!(args.command, Command::Login(provider) if provider == "codex"));
    }

    #[test]
    fn rejects_bad_usage() {
        assert!(parse(&["login"]).is_err());
        assert!(parse(&["login", "a", "b"]).is_err());
        assert!(parse(&["status", "extra"]).is_err());
        assert!(parse(&["--model"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }

    #[test]
    fn status_shows_the_compat_base_url_and_not_the_key() {
        use crate::tools::testutil::TempDir;

        let home = TempDir::new("status");
        let compat = PROVIDERS.iter().find(|info| info.id == "compat").unwrap();
        assert_eq!(provider_state(home.path(), compat, None, None, None), "-");
        assert_eq!(
            provider_state(
                home.path(),
                compat,
                Some("secret-key".into()),
                None,
                Some("http://localhost:8000/v1".into())
            ),
            "http://localhost:8000/v1"
        );

        let groq = PROVIDERS.iter().find(|info| info.id == "groq").unwrap();
        assert_eq!(
            provider_state(home.path(), groq, None, Some("key".into()), None),
            "api key (env)"
        );
    }
}
