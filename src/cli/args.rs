//! Command-line argument definitions.
//!
//! Doc comments here are user-facing `--help` text, so they are written as
//! prose for the terminal, not as rustdoc Markdown.
#![allow(clippy::doc_markdown)]

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use sshcm::model::forward::ForwardKind;

#[derive(Debug, Parser)]
#[command(name = "sshcm", version, about, arg_required_else_help = true)]
pub struct Cli {
    /// Config file to operate on [default: ~/.ssh/config]
    #[arg(
        short = 'F',
        long,
        env = "SSHCM_CONFIG",
        global = true,
        value_name = "PATH"
    )]
    pub file: Option<PathBuf>,

    #[command(flatten)]
    pub write: WriteOpts,

    #[command(subcommand)]
    pub command: Command,
}

/// Options for commands that modify the config.
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
#[derive(Debug, Args, Clone, Default)]
pub struct WriteOpts {
    /// Show the diff but write nothing
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,
    /// Don't ask for confirmation
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    /// Don't save a backup before writing
    #[arg(long, global = true)]
    pub no_backup: bool,
    /// Don't check the result with `ssh -G` before writing
    #[arg(long, global = true)]
    pub no_validate: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List Host blocks
    #[command(visible_alias = "ls")]
    List {
        /// Only show blocks with a pattern matching this wildcard (e.g. 'web-*')
        #[arg(short, long, value_name = "GLOB")]
        pattern: Option<String>,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },

    /// Print the Host block(s) for an alias exactly as written
    Show {
        /// The alias, as it appears on a Host line
        alias: String,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },

    /// Show the effective settings ssh would use for a host
    Resolve {
        /// The host name, as you would type it after `ssh`
        host: String,
        /// Output JSON
        #[arg(long, conflicts_with = "via_ssh")]
        json: bool,
        /// Ask OpenSSH itself (`ssh -G`), including built-in defaults
        #[arg(long)]
        via_ssh: bool,
    },

    /// Add a new Host block (placed before `Host *` by default)
    Add {
        /// The new alias
        alias: String,
        /// HostName: the real host name or IP to connect to
        #[arg(long, value_name = "HOST")]
        hostname: Option<String>,
        /// Remote user name
        #[arg(short, long)]
        user: Option<String>,
        /// Port number
        #[arg(short, long)]
        port: Option<u16>,
        /// IdentityFile: private key path
        #[arg(short, long, value_name = "PATH")]
        identity: Option<String>,
        /// Any other option, e.g. -o ProxyJump=bastion (repeatable)
        #[arg(short = 'o', long = "option", value_name = "KEY=VALUE", value_parser = parse_kv)]
        options: Vec<(String, String)>,
        #[command(flatten)]
        forwards: ForwardSpecs,
        /// Insert before this alias's block
        #[arg(long, value_name = "ALIAS", conflicts_with = "after")]
        before: Option<String>,
        /// Insert after this alias's block
        #[arg(long, value_name = "ALIAS")]
        after: Option<String>,
    },

    /// Set options in a Host block, e.g. `set web Port=2222 User=deploy`
    Set {
        /// The alias whose block to edit
        alias: String,
        /// KEY=VALUE pairs; the value is written verbatim (quote paths with spaces)
        #[arg(required = true, value_name = "KEY=VALUE", value_parser = parse_kv)]
        settings: Vec<(String, String)>,
        /// Add another line for list options (IdentityFile, LocalForward, …)
        #[arg(long)]
        append: bool,
    },

    /// Remove options from a Host block
    Unset {
        /// The alias whose block to edit
        alias: String,
        /// Option names to remove (all occurrences)
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
    },

    /// Remove a host (and its comments); other aliases on the same Host line stay
    #[command(visible_alias = "remove")]
    Rm {
        /// The alias to remove
        alias: String,
    },

    /// Rename an alias
    #[command(visible_alias = "mv")]
    Rename {
        /// Current alias
        old: String,
        /// New alias
        new: String,
    },

    /// Duplicate a Host block under a new alias
    #[command(visible_alias = "copy")]
    Cp {
        /// Alias to copy
        src: String,
        /// New alias
        dst: String,
    },

    /// Manage port and socket forwarding (LocalForward, RemoteForward, DynamicForward)
    #[command(visible_alias = "fwd")]
    Forward {
        #[command(subcommand)]
        action: ForwardAction,
    },

    /// Check the config for errors ssh would reject and likely mistakes
    Lint {
        /// Also fail (exit 4) on warnings
        #[arg(long)]
        strict: bool,
        /// Output JSON
        #[arg(long)]
        json: bool,
        /// Skip checks that read other files (keys, permissions)
        #[arg(long)]
        no_file_checks: bool,
    },

    /// Normalise layout: indentation, keyword case, separators, blank lines
    Fmt {
        /// Don't write; exit 1 if any file would change
        #[arg(long)]
        check: bool,
        /// Spaces of indentation inside Host/Match blocks
        #[arg(long, value_name = "N", default_value_t = 4, conflicts_with = "tabs")]
        indent: usize,
        /// Indent with a tab instead of spaces
        #[arg(long)]
        tabs: bool,
        /// Also format files pulled in by Include
        #[arg(long)]
        all: bool,
    },

    /// Print all Host blocks as JSON (see `import`)
    Export,

    /// Add Host blocks from a JSON file produced by `export`
    Import {
        /// JSON file, or - for standard input. (Not named `file`: that id
        /// belongs to the global -F/--file option, and clap would merge them.)
        #[arg(value_name = "JSON")]
        source: PathBuf,
        /// Skip hosts that already exist instead of failing
        #[arg(long)]
        skip_existing: bool,
    },

    /// List or restore the backups taken before each change
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },

    /// Print a shell completion script
    Completions {
        /// Shell to generate for
        shell: clap_complete::Shell,
    },

    /// Print the man page in roff format
    #[command(hide = true)]
    Manpage,

    /// Print how the config file was parsed (developer aid)
    #[command(hide = true)]
    DumpSyntax,
}

/// `-L/-R/-D` in ssh's own command-line syntax; written to the config in
/// config syntax (`-L 8080:db:80` becomes `LocalForward 8080 db:80`).
#[derive(Debug, Args, Clone, Default)]
pub struct ForwardSpecs {
    /// Local forward: [bind:]port:host:hostport, or with Unix socket paths (repeatable)
    #[arg(short = 'L', long = "local", value_name = "SPEC")]
    pub local: Vec<String>,
    /// Remote forward: [bind:]port:host:hostport, or [bind:]port for a remote SOCKS proxy (repeatable)
    #[arg(short = 'R', long = "remote", value_name = "SPEC")]
    pub remote: Vec<String>,
    /// Dynamic (SOCKS) forward: [bind:]port (repeatable)
    #[arg(short = 'D', long = "dynamic", value_name = "SPEC")]
    pub dynamic: Vec<String>,
}

impl ForwardSpecs {
    /// `(kind, spec)` pairs in -L, -R, -D order.
    pub fn iter(&self) -> impl Iterator<Item = (ForwardKind, &str)> {
        self.local
            .iter()
            .map(|s| (ForwardKind::Local, s.as_str()))
            .chain(
                self.remote
                    .iter()
                    .map(|s| (ForwardKind::Remote, s.as_str())),
            )
            .chain(
                self.dynamic
                    .iter()
                    .map(|s| (ForwardKind::Dynamic, s.as_str())),
            )
    }

    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.remote.is_empty() && self.dynamic.is_empty()
    }
}

#[derive(Debug, Subcommand)]
pub enum ForwardAction {
    /// List forwards of every host, or of one alias
    #[command(visible_alias = "ls")]
    List {
        /// Only this alias
        alias: Option<String>,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },
    /// Add forwards, e.g. `forward add db -L 5432:localhost:5432 -D 1080`
    Add {
        /// The alias whose block to edit
        alias: String,
        #[command(flatten)]
        specs: ForwardSpecs,
    },
    /// Remove forwards by listen endpoint, e.g. `forward rm db -L 5432 -D 1080`
    #[command(visible_alias = "remove")]
    Rm {
        /// The alias whose block to edit
        alias: String,
        #[command(flatten)]
        specs: ForwardSpecs,
    },
}

#[derive(Debug, Subcommand)]
pub enum BackupAction {
    /// List backups, newest first
    #[command(visible_alias = "ls")]
    List,
    /// Restore a backup (the current file is backed up first)
    Restore {
        /// `latest`, a number from `backup list`, or a backup file name
        #[arg(default_value = "latest")]
        id: String,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got `{s}`"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err(format!("missing key in `{s}`"));
    }
    Ok((k.to_owned(), v.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn kv_parsing() {
        assert_eq!(parse_kv("Port=22"), Ok(("Port".into(), "22".into())));
        assert_eq!(parse_kv("SetEnv=A=b"), Ok(("SetEnv".into(), "A=b".into())));
        assert_eq!(parse_kv(" User = x "), Ok(("User".into(), "x".into())));
        assert!(parse_kv("Port").is_err());
        assert!(parse_kv("=x").is_err());
    }

    /// Regression: a subcommand argument named `file` shared clap's id with
    /// the global `-F/--file`, so `import x.json` silently used x.json as
    /// the config file and wrote hosts into it.
    #[test]
    fn subcommand_args_never_alias_the_config_path() {
        let cli = Cli::try_parse_from(["sshcm", "-F", "cfg", "import", "hosts.json"]).unwrap();
        assert_eq!(cli.file.as_deref(), Some(std::path::Path::new("cfg")));
        let Command::Import { source, .. } = cli.command else {
            panic!()
        };
        assert_eq!(source, std::path::Path::new("hosts.json"));

        let cli = Cli::try_parse_from(["sshcm", "import", "hosts.json"]).unwrap();
        assert_eq!(cli.file, None, "positional must not populate --file");

        // No subcommand argument may reuse the ids of global options.
        let root = Cli::command();
        let globals: Vec<_> = root
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .collect();
        for sub in root.get_subcommands() {
            for arg in sub.get_arguments().filter(|a| !a.is_global_set()) {
                assert!(
                    !globals.contains(&arg.get_id().to_string()),
                    "`{}` argument `{}` collides with a global option",
                    sub.get_name(),
                    arg.get_id()
                );
            }
        }
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
