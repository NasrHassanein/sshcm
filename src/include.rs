//! Following `Include` directives: a root config plus every file it pulls in.
//!
//! Semantics (verified against OpenSSH 10.2 with `ssh -G`):
//! - relative paths are relative to `~/.ssh`, *not* to the including file,
//!   even when the root config was given with `-F`;
//! - an `Include` line takes several space-separated paths, read in the
//!   order written; each may be a glob, and its matches are read sorted;
//! - paths that match nothing are silently ignored;
//! - `~`, `%d` (home), `%u` (local user), `%%`, and `${VAR}` are expanded;
//! - an `Include` inside a `Host`/`Match` block only takes effect when that
//!   block applies (handled by [`crate::resolve`]);
//! - including a file that is already being read is fatal in ssh ("Too many
//!   recursive configuration includes"), as is nesting deeper than 16;
//! - an included file that is group/world-writable makes ssh refuse the
//!   whole configuration ("Bad owner or permissions").
//!
//! Loading is best-effort: problems with included files become
//! [`Diagnostic`]s so the rest of the configuration stays usable.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use glob::Pattern;

use crate::diagnostic::Diagnostic;
use crate::model::value::split_args;
use crate::syntax::Document;

/// OpenSSH's `READCONF_MAX_DEPTH`.
pub const MAX_DEPTH: usize = 16;

/// Environment facts needed to expand `Include` paths.
#[derive(Debug, Clone)]
pub struct IncludeContext {
    /// The user's home directory (`~`, `%d`).
    pub home: PathBuf,
    /// Base for relative paths; normally `~/.ssh`.
    pub ssh_dir: PathBuf,
    /// The local user name (`%u`).
    pub local_user: String,
}

impl IncludeContext {
    /// The standard context for a home directory: relative includes resolve
    /// against `<home>/.ssh`.
    pub fn for_home(home: PathBuf, local_user: String) -> Self {
        Self {
            ssh_dir: home.join(".ssh"),
            home,
            local_user,
        }
    }
}

/// One config file in a [`ConfigSet`].
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// The path as configured (root) or as expanded from `Include`.
    pub path: PathBuf,
    /// The parsed contents.
    pub doc: Document,
    /// Canonical path, used to recognise the same file reached twice.
    real: PathBuf,
}

/// A root config and the files it includes. File 0 is the root.
#[derive(Debug, Clone)]
pub struct ConfigSet {
    files: Vec<ConfigFile>,
    /// `(file index, line index)` of an `Include` line → included files.
    /// Absent for `Include` lines that were not followed.
    includes: HashMap<(usize, usize), Vec<usize>>,
    diagnostics: Vec<Diagnostic>,
}

impl ConfigSet {
    /// A set holding just `doc`, without following its `Include` lines.
    pub fn single(path: impl Into<PathBuf>, doc: Document) -> Self {
        let path = path.into();
        Self {
            files: vec![ConfigFile {
                real: fs::canonicalize(&path).unwrap_or_else(|_| path.clone()),
                path,
                doc,
            }],
            includes: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Read `root` and everything it includes.
    ///
    /// # Errors
    /// Only if the root itself cannot be read; problems with included files
    /// are recorded as [`ConfigSet::diagnostics`].
    pub fn load(root: &Path, ctx: &IncludeContext) -> io::Result<Self> {
        let text = fs::read_to_string(root)?;
        Ok(Self::with_root(root, Document::parse(&text), ctx))
    }

    /// Follow the `Include` lines of an already-parsed root document.
    pub fn with_root(root: &Path, doc: Document, ctx: &IncludeContext) -> Self {
        let mut set = Self::single(root, doc);
        set.follow(0, ctx, &mut vec![0]);
        set
    }

    /// All files, root first, then in the order they were first included.
    pub fn files(&self) -> &[ConfigFile] {
        &self.files
    }

    /// The root file.
    pub fn root(&self) -> &ConfigFile {
        &self.files[0]
    }

    /// Files included by the `Include` at `line` (0-based) of `file`, or
    /// `None` if that line was not followed.
    pub fn included_by(&self, file: usize, line: usize) -> Option<&[usize]> {
        self.includes.get(&(file, line)).map(Vec::as_slice)
    }

    /// Problems found while following includes.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Every `(file, block)` in the order ssh reads them, regardless of
    /// whether the block would apply to any particular host. A file
    /// included from several places is listed once, at its first inclusion.
    pub fn blocks_in_read_order(&self) -> Vec<(usize, usize)> {
        fn visit(
            set: &ConfigSet,
            fid: usize,
            seen: &mut HashSet<usize>,
            out: &mut Vec<(usize, usize)>,
        ) {
            if !seen.insert(fid) {
                return;
            }
            let doc = &set.files[fid].doc;
            for (bi, block) in doc.blocks().iter().enumerate() {
                out.push((fid, bi));
                for line in block.lines() {
                    for &child in set.included_by(fid, line).unwrap_or_default() {
                        visit(set, child, seen, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(self, 0, &mut HashSet::new(), &mut out);
        out
    }

    fn diag(&mut self, file: usize, line: usize, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            file: self.files[file].path.clone(),
            line: line + 1,
            message: message.into(),
        });
    }

    /// Load the targets of every `Include` line in `fid`, depth first.
    /// `stack` holds the files currently being read (for cycle detection).
    fn follow(&mut self, fid: usize, ctx: &IncludeContext, stack: &mut Vec<usize>) {
        let include_lines: Vec<(usize, String)> = self.files[fid]
            .doc
            .lines()
            .iter()
            .enumerate()
            .filter(|(_, l)| l.is_keyword("Include"))
            .map(|(i, l)| (i, l.value().unwrap_or_default().to_owned()))
            .collect();

        for (line, value) in include_lines {
            let paths = match expand(&value, ctx) {
                Ok(p) => p,
                Err(msg) => {
                    self.diag(fid, line, msg);
                    continue;
                }
            };
            let mut children = Vec::new();
            for path in paths {
                if let Some(child) = self.open(fid, line, &path, ctx, stack) {
                    children.push(child);
                }
            }
            self.includes.insert((fid, line), children);
        }
    }

    /// Load one included file (or reuse it if already loaded).
    fn open(
        &mut self,
        fid: usize,
        line: usize,
        path: &Path,
        ctx: &IncludeContext,
        stack: &mut Vec<usize>,
    ) -> Option<usize> {
        let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let shown = path.display();

        if let Some(&on_stack) = stack.iter().find(|&&f| self.files[f].real == real) {
            let msg = format!(
                "Include cycle: {shown} is already being read (via {}); ssh fails with \"Too many recursive configuration includes\"",
                self.files[on_stack].path.display()
            );
            self.diag(fid, line, msg);
            return None;
        }
        if stack.len() > MAX_DEPTH {
            let msg = format!(
                "Include nested deeper than {MAX_DEPTH} levels at {shown}; ssh refuses this"
            );
            self.diag(fid, line, msg);
            return None;
        }
        if let Some(existing) = self.files.iter().position(|f| f.real == real) {
            return Some(existing);
        }
        if path.is_dir() {
            self.diag(
                fid,
                line,
                format!("Include matched directory {shown}; ssh cannot read it"),
            );
            return None;
        }
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.diag(fid, line, format!("cannot read included file {shown}: {e}"));
                return None;
            }
        };
        if let Some(problem) = permission_problem(path) {
            self.diag(fid, line, format!("{shown}: {problem}; ssh refuses the whole configuration (\"Bad owner or permissions\")"));
        }

        self.files.push(ConfigFile {
            path: path.to_owned(),
            doc: Document::parse(&text),
            real,
        });
        let child = self.files.len() - 1;
        stack.push(child);
        self.follow(child, ctx, stack);
        stack.pop();
        Some(child)
    }
}

fn permission_problem(path: &Path) -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path).ok()?.permissions().mode();
    (mode & 0o022 != 0).then_some("writable by group or others")
}

/// Expand an `Include` value into the existing paths it names, in the
/// order ssh reads them.
///
/// # Errors
/// A message for malformed values (bad quoting, unknown `%` token, unset
/// environment variable, invalid glob).
pub fn expand(value: &str, ctx: &IncludeContext) -> Result<Vec<PathBuf>, String> {
    let args = split_args(value).map_err(|e| format!("Include ignored: {e}"))?;
    if args.is_empty() {
        return Err("Include has no file argument".to_owned());
    }
    let mut out = Vec::new();
    for arg in args {
        let pattern = to_glob(&arg, ctx)?;
        let matches =
            glob::glob(&pattern).map_err(|e| format!("Include pattern `{arg}` is invalid: {e}"))?;
        // Unreadable directories are skipped, as glob(3) does without GLOB_ERR.
        out.extend(matches.filter_map(Result::ok));
    }
    Ok(out)
}

/// Turn one `Include` argument into an absolute glob pattern. Text that
/// comes from the environment (home, user, variables) is escaped so it is
/// matched literally.
fn to_glob(arg: &str, ctx: &IncludeContext) -> Result<String, String> {
    let esc = |p: &Path| Pattern::escape(&p.to_string_lossy());
    // The relative/absolute decision is made on the raw argument, as ssh does.
    let (mut out, rest) = if let Some(rest) = arg.strip_prefix("~/") {
        (format!("{}/", esc(&ctx.home)), rest)
    } else if arg == "~" {
        (esc(&ctx.home), "")
    } else if arg.starts_with('~') {
        return Err(format!("Include `{arg}`: `~user` paths are not supported"));
    } else if arg.starts_with('/') {
        (String::new(), arg)
    } else {
        (format!("{}/", esc(&ctx.ssh_dir)), arg)
    };

    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' => match chars.next() {
                Some('%') => out.push('%'),
                Some('d') => out.push_str(&esc(&ctx.home)),
                Some('u') => out.push_str(&Pattern::escape(&ctx.local_user)),
                Some(t) => return Err(format!("Include `{arg}`: unsupported token %{t}")),
                None => return Err(format!("Include `{arg}`: trailing %")),
            },
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                let name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let value = std::env::var(&name).map_err(|_| {
                    format!("Include `{arg}`: environment variable {name} is not set")
                })?;
                out.push_str(&Pattern::escape(&value));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> IncludeContext {
        IncludeContext {
            home: dir.to_owned(),
            ssh_dir: dir.join(".ssh"),
            local_user: "me".into(),
        }
    }

    #[test]
    fn glob_construction() {
        let c = IncludeContext {
            home: "/home/a[1]".into(),
            ssh_dir: "/home/a[1]/.ssh".into(),
            local_user: "u*".into(),
        };
        assert_eq!(
            to_glob("conf.d/*", &c).unwrap(),
            "/home/a[[]1[]]/.ssh/conf.d/*"
        );
        assert_eq!(to_glob("~/x", &c).unwrap(), "/home/a[[]1[]]/x");
        assert_eq!(to_glob("/etc/ssh/*.conf", &c).unwrap(), "/etc/ssh/*.conf");
        assert_eq!(to_glob("/x/%u/%%", &c).unwrap(), "/x/u[*]/%");
        assert!(to_glob("~bob/x", &c).is_err());
        assert!(to_glob("/x/%h", &c).is_err());
    }

    #[test]
    fn env_vars_expand() {
        let c = ctx(Path::new("/h"));
        // PATH is always set; reading it avoids mutating the environment.
        let path = std::env::var("PATH").unwrap();
        assert_eq!(
            to_glob("/${PATH}", &c).unwrap(),
            format!("/{}", Pattern::escape(&path))
        );
        assert!(
            to_glob("/${SSHCM_SURELY_UNSET_VAR}", &c)
                .unwrap_err()
                .contains("not set")
        );
    }

    #[test]
    fn multiple_args_keep_written_order_and_globs_sort() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(ssh.join("d")).unwrap();
        for f in ["b2", "b1", "a"] {
            fs::write(ssh.join("d").join(f), "").unwrap();
        }
        let got = expand("d/b* d/a d/missing", &ctx(dir.path())).unwrap();
        let names: Vec<_> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["b1", "b2", "a"]);
    }

    #[test]
    fn cycles_and_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("config"), "Include a\nInclude a\n").unwrap();
        fs::write(ssh.join("a"), "Include b\n").unwrap();
        fs::write(ssh.join("b"), "Include a\nHost b\n").unwrap();

        let set = ConfigSet::load(&ssh.join("config"), &ctx(dir.path())).unwrap();
        assert_eq!(set.files().len(), 3, "a loaded once despite two Includes");
        assert_eq!(set.included_by(0, 0), Some(&[1][..]));
        assert_eq!(set.included_by(0, 1), Some(&[1][..]));
        assert_eq!(set.included_by(2, 0), Some(&[][..]), "cycle edge dropped");
        assert_eq!(set.diagnostics().len(), 1);
        assert!(set.diagnostics()[0].message.contains("Include cycle"));
        assert_eq!(set.diagnostics()[0].line, 1);
    }

    #[test]
    fn depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("config"), "Include f0\n").unwrap();
        for i in 0..20 {
            fs::write(ssh.join(format!("f{i}")), format!("Include f{}\n", i + 1)).unwrap();
        }
        let set = ConfigSet::load(&ssh.join("config"), &ctx(dir.path())).unwrap();
        assert_eq!(set.files().len(), 1 + MAX_DEPTH);
        assert!(set.diagnostics()[0].message.contains("deeper than 16"));
    }

    #[test]
    fn writable_include_is_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("config"), "Include w\n").unwrap();
        fs::write(ssh.join("w"), "").unwrap();
        fs::set_permissions(ssh.join("w"), fs::Permissions::from_mode(0o666)).unwrap();
        let set = ConfigSet::load(&ssh.join("config"), &ctx(dir.path())).unwrap();
        assert!(
            set.diagnostics()[0]
                .message
                .contains("Bad owner or permissions")
        );
    }
}
