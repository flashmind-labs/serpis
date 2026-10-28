//! Skills, as pi has them (the Agent Skills specification, agentskills.io): a directory with a
//! `SKILL.md` whose frontmatter names it and says when it applies, beside any scripts and
//! references it uses. The system prompt lists each skill's name, description and file; the
//! model reads the file when a task matches. `/skill:name args` sends a skill's instructions
//! with the message, for when it should apply for sure.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Its `SKILL.md` (or a standalone Markdown skill).
    pub file: PathBuf,
    /// Where it was found: `user`, `project` or `path` (a configured one), or `claude` (Claude
    /// Code's skill folders).
    pub source: String,
    /// Only for `/skill:name`: the model isn't offered it (`disable-model-invocation`).
    pub explicit_only: bool,
}

impl Skill {
    pub fn dir(&self) -> &Path {
        self.file.parent().unwrap_or(Path::new("."))
    }
}

/// Where to look, in order (the first skill of a name wins).
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// The agent directory (`~/.pi/agent`): its `skills`.
    pub agent_dir: PathBuf,
    /// The working directory: its `.pi/skills`, and `.agents/skills` up to the repository root.
    pub cwd: PathBuf,
    /// More files or directories (pi's `skills` setting, `agent-skill-paths`).
    pub paths: Vec<PathBuf>,
    /// Also Claude Code's: `~/.claude/skills` and the project's `.claude/skills`.
    pub claude: bool,
    /// The home directory (`~/.agents/skills`, `~/.claude/skills`).
    pub home: PathBuf,
}

/// Every skill the places hold, the first of each name kept.
pub fn discover(p: &Places) -> Vec<Skill> {
    let mut found = Vec::new();
    let home = &p.home;
    let mut dirs: Vec<(PathBuf, &str)> = vec![(p.agent_dir.join("skills"), "user"), (p.cwd.join(".pi").join("skills"), "project"), (home.join(".agents").join("skills"), "user")];
    // `.agents/skills` from the working directory up, stopping at the repository's root.
    for d in p.cwd.ancestors() {
        dirs.push((d.join(".agents").join("skills"), "project"));
        if d.join(".git").exists() {
            break;
        }
    }
    for path in &p.paths {
        dirs.push((path.clone(), "path"));
    }
    if p.claude {
        dirs.push((home.join(".claude").join("skills"), "claude"));
        dirs.push((p.cwd.join(".claude").join("skills"), "claude"));
    }
    let mut seen_files = HashSet::new();
    let mut names = HashSet::new();
    for (dir, source) in dirs {
        let mut here = Vec::new();
        if dir.is_file() {
            here.extend(load(&dir, source));
        } else {
            walk(&dir, source, true, &mut here);
        }
        for s in here {
            let real = std::fs::canonicalize(&s.file).unwrap_or_else(|_| s.file.clone());
            if seen_files.insert(real) && names.insert(s.name.clone()) {
                found.push(s);
            }
        }
    }
    found
}

/// A directory's skills: itself when it holds `SKILL.md`, else what its subdirectories hold
/// (and, at its top, Markdown files that describe themselves).
fn walk(dir: &Path, source: &str, top: bool, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.iter().any(|e| e.file_name() == "SKILL.md") {
        out.extend(load(&dir.join("SKILL.md"), source));
        return;
    }
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            walk(&path, source, false, out);
        } else if top && name.ends_with(".md") {
            out.extend(load(&path, source));
        }
    }
}

/// A skill from its file: None without a description (a standalone file without frontmatter
/// isn't one).
fn load(file: &Path, source: &str) -> Option<Skill> {
    let text = std::fs::read_to_string(file).ok()?;
    let (front, _) = frontmatter(&text)?;
    let field = |k: &str| front.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let description = field("description").filter(|d| !d.trim().is_empty())?;
    let dir_name = file.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = field("name").filter(|n| !n.is_empty()).unwrap_or(dir_name);
    Some(Skill {
        name,
        description: description.trim().to_string(),
        file: file.to_path_buf(),
        source: source.to_string(),
        explicit_only: field("disable-model-invocation").as_deref() == Some("true"),
    })
}

/// A Markdown file's YAML frontmatter, as its top-level `key: value` pairs (strings: quoted,
/// plain, folded `>` or literal `|`, continued on indented lines), and the text after it.
pub fn frontmatter(text: &str) -> Option<(Vec<(String, String)>, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    let end = rest.find("\n---").map(|i| (i, i + 4))?;
    let (yaml, body) = (&rest[..end.0], &rest[end.1..]);
    let body = body.split_once('\n').map(|(_, b)| b).unwrap_or("");
    let mut out: Vec<(String, String)> = Vec::new();
    let mut block: Option<(char, Vec<String>)> = None;
    for line in yaml.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented || line.trim().is_empty() {
            let t = line.trim();
            match (&mut block, out.last_mut()) {
                (Some((_, lines)), _) => lines.push(t.to_string()),
                (None, Some((_, v))) if !t.is_empty() && !t.starts_with('-') => {
                    if !v.is_empty() {
                        v.push(' ');
                    }
                    v.push_str(t);
                }
                _ => {}
            }
            continue;
        }
        if let Some((style, lines)) = block.take()
            && let Some((_, v)) = out.last_mut()
        {
            *v = join_block(style, &lines);
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = v.trim();
        let value = if v.starts_with('|') || v.starts_with('>') {
            block = Some((v.chars().next().unwrap_or('>'), Vec::new()));
            String::new()
        } else {
            unquote(v)
        };
        out.push((k.trim().to_string(), value));
    }
    if let Some((style, lines)) = block
        && let Some((_, v)) = out.last_mut()
    {
        *v = join_block(style, &lines);
    }
    Some((out, body))
}

fn join_block(style: char, lines: &[String]) -> String {
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let s = if style == '|' { lines.join("\n") } else { lines.join(" ") };
    s.trim().to_string()
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''))) {
        let inner = &v[1..v.len() - 1];
        return if v.starts_with('"') { inner.replace("\\\"", "\"").replace("\\n", "\n") } else { inner.replace("''", "'") };
    }
    v.to_string()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// What the system prompt says of the skills the model may load (pi's words): empty when
/// there are none.
pub fn for_prompt(skills: &[Skill], read_tool: &str) -> String {
    let offered: Vec<&Skill> = skills.iter().filter(|s| !s.explicit_only).collect();
    if offered.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        format!("Use the {read_tool} tool to load a skill's file when the task matches its description."),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for s in offered {
        lines.push("  <skill>".into());
        lines.push(format!("    <name>{}</name>", escape(&s.name)));
        lines.push(format!("    <description>{}</description>", escape(&s.description)));
        lines.push(format!("    <location>{}</location>", escape(&s.file.to_string_lossy())));
        lines.push("  </skill>".into());
    }
    lines.push("</available_skills>".into());
    lines.join("\n")
}

/// `/skill:name args`: the skill's instructions (as pi sends them), then the args. None when
/// the text isn't one, or names no skill.
pub fn expand(text: &str, skills: &[Skill]) -> Option<String> {
    let rest = text.strip_prefix("/skill:")?;
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (rest.trim(), ""),
    };
    let skill = skills.iter().find(|s| s.name == name)?;
    let content = std::fs::read_to_string(&skill.file).ok()?;
    let body = frontmatter(&content).map(|(_, b)| b).unwrap_or(&content).trim();
    let block = format!("<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{body}\n</skill>", skill.name, skill.file.display(), skill.dir().display());
    Some(if args.is_empty() { block } else { format!("{block}\n\n{args}") })
}

/// A message as sent, as typed: `/skill:name args` for one a skill was expanded into.
pub fn as_typed(text: &str) -> Option<String> {
    let rest = text.strip_prefix("<skill name=\"")?;
    let (name, rest) = rest.split_once('"')?;
    let after = rest.split_once("\n</skill>").map(|(_, a)| a.trim()).unwrap_or("");
    Some(if after.is_empty() { format!("/skill:{name}") } else { format!("/skill:{name} {after}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn frontmatter_reads_the_usual_forms() {
        let (f, body) = frontmatter("---\nname: pdf\ndescription: >\n  Extract text.\n  Use for PDFs.\nquoted: \"a \\\"b\\\"\"\nflag: true\n---\n# Body\n").unwrap();
        let get = |k: &str| f.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("name"), Some("pdf"));
        assert_eq!(get("description"), Some("Extract text. Use for PDFs."));
        assert_eq!(get("quoted"), Some("a \"b\""));
        assert_eq!(get("flag"), Some("true"));
        assert_eq!(body, "# Body\n");
        // A plain value continued on indented lines.
        let (f, _) = frontmatter("---\ndescription: one\n  two\n---\n").unwrap();
        assert_eq!(f[0].1, "one two");
        assert!(frontmatter("# no frontmatter").is_none());
    }

    #[test]
    fn skills_are_found_listed_and_expanded_as_pi_does() {
        let root = std::env::temp_dir().join(format!("serpis-skills-{}", std::process::id()));
        let agent = root.join("agent");
        let cwd = root.join("project");
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        write(&agent.join("skills/eng/review/SKILL.md"), "---\nname: review\ndescription: Review code. Use when asked to review.\n---\nRead the diff.\n");
        write(&agent.join("skills/eng/review/refs/x.md"), "not a skill");
        write(&agent.join("skills/quiet/SKILL.md"), "---\ndescription: Only by name.\ndisable-model-invocation: true\n---\nShh.\n");
        write(&agent.join("skills/notes.md"), "# just notes");
        // The project's own, and one by a name already taken (the first wins).
        write(&cwd.join(".pi/skills/deploy/SKILL.md"), "---\ndescription: Deploy <it> & check.\n---\nRun ./deploy.sh.\n");
        write(&cwd.join(".agents/skills/review/SKILL.md"), "---\nname: review\ndescription: Another.\n---\n");
        let skills = discover(&Places { agent_dir: agent.clone(), cwd: cwd.clone(), paths: vec![], claude: false, home: root.join("home") });
        let names: Vec<(&str, &str)> = skills.iter().map(|s| (s.name.as_str(), s.source.as_str())).collect();
        assert_eq!(names, [("review", "user"), ("quiet", "user"), ("deploy", "project")], "named by their folder when frontmatter doesn't");
        let prompt = for_prompt(&skills, "read");
        assert!(prompt.contains("<name>review</name>") && prompt.contains("Deploy &lt;it&gt; &amp; check."), "{prompt}");
        assert!(!prompt.contains("quiet"), "an explicit-only skill isn't offered");
        let sent = expand("/skill:deploy to staging", &skills).unwrap();
        assert!(sent.starts_with("<skill name=\"deploy\" location=\"") && sent.contains("\nReferences are relative to ") && sent.contains("\n\nRun ./deploy.sh.\n</skill>\n\nto staging"), "{sent}");
        assert_eq!(as_typed(&sent).as_deref(), Some("/skill:deploy to staging"));
        assert!(expand("/skill:quiet", &skills).is_some(), "by name it still works");
        assert_eq!(expand("/skill:nope x", &skills), None);
        assert_eq!(expand("hello", &skills), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
