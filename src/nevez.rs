//
// This file is part of elb-dev-tools-ng
//
// Copyright (C) 2020 Eric Le Bihan <eric.le.bihan.dev@free.fr>
//
// SPDX-License-Identifier: MIT OR Apache-2.0
//

use anyhow::Result;
use chrono::{DateTime, FixedOffset, Utc};
use elb_dev_tools_ng::run_command_or;
use regex::Regex;
use std::ffi::OsString;
use std::fs::{rename, File};
use std::io::{stdout, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "Nevez", about = "Generate a changelog")]
struct NevezOptions {
    #[structopt(short = "-s", long = "--since", help = "previous tag")]
    old_tag: Option<String>,

    #[structopt(
        short = "f",
        long = "file",
        help = "Path to changelog",
        default_value = "NEWS.md",
        parse(from_os_str)
    )]
    changelog: PathBuf,

    #[structopt(
        short = "i",
        long = "in-place",
        help = "Edit changelog in place"
    )]
    in_place: bool,

    #[structopt(help = "New tag")]
    new_tag: String,

    #[structopt(help = "Repository")]
    repository: Option<PathBuf>,
}

/// Represent the author of a commit
#[derive(Debug)]
struct Author {
    name: String,
    email: String,
}

/// Represent a commit
#[derive(Debug)]
struct Commit {
    id: String,
    author: Author,
    date: DateTime<FixedOffset>,
    message: String,
}

impl Commit {
    /// Return first line of commit message
    fn brief(&self) -> Option<&str> {
        self.message.lines().nth(0)
    }
}

/// Parse a `git log` commit
#[derive(Debug)]
struct CommitLogParser {
    pat_author: Regex,
    pat_date: Regex,
}

impl CommitLogParser {
    /// Create a new commit log parser
    fn new() -> Result<Self> {
        let pat_author = Regex::new(r"^Author:\s+(.+)<(.+)>$")?;
        let pat_date = Regex::new(r"^Date:\s+(.+)$")?;
        Ok(CommitLogParser {
            pat_author,
            pat_date,
        })
    }

    /// Parse commit log
    fn parse(&self, text: &str) -> Option<Commit> {
        let mut lines = text.lines();
        let id = lines.nth(0)?;
        let caps = lines.next().and_then(|l| self.pat_author.captures(l))?;
        let author = Author {
            name: caps[1].to_string(),
            email: caps[2].to_string(),
        };
        let caps = lines.next().and_then(|l| self.pat_date.captures(l))?;
        let date = DateTime::parse_from_rfc2822(&caps[1]).ok()?;
        Some(Commit {
            id: id.to_string(),
            author: author,
            date: date,
            message: lines
                .skip(1)
                .map(str::trim_start)
                .collect::<Vec<&str>>()
                .join("\n"),
        })
    }
}

/// Kind of commits
#[derive(Debug)]
enum CommitKind {
    Addition,
    Bump,
    Fix,
}

/// Classify commits
#[derive(Debug)]
struct CommitClassifier {
    add_patterns: Vec<Regex>,
    fix_patterns: Vec<Regex>,
    bump_patterns: Vec<Regex>,
}

/// Result of classification
#[derive(Debug)]
struct ClassifiedCommits<'a> {
    additions: Vec<&'a Commit>,
    changes: Vec<&'a Commit>,
    fixes: Vec<&'a Commit>,
}

impl CommitClassifier {
    /// Create a new classifier
    fn new() -> Result<Self> {
        let add_patterns = vec![
            Regex::new(r"^([Aa]dd(?:ed)?|[Nn]ew)\s+.+$")?,
            Regex::new(r"^.+:\s+([Aa]dd(?:ed)?|[Nn]ew)\s+.+$")?,
        ];
        let fix_patterns = vec![
            Regex::new(r"^[Ff]ix(?:ed)?\s+.+$")?,
            Regex::new(r"^.+\s+[Ff]ix(?:ed)?\s+.+$")?,
        ];
        let bump_patterns = vec![
            Regex::new(r"^[Kk]ick off\s+.+$")?,
            Regex::new(
                r"^(?:configure|meson|CMakeLists|version):\s+[Kk]ick off\s+.+$",
            )?,
            Regex::new(r"^[Bb]ump(?:ed)?\s+version.+$")?,
            Regex::new(
                r"^(?:configure|meson|CMakeLists|version):\s+[Bb]ump(?:ed)?\s+version\s.+$",
            )?,
            Regex::new(r"^(version|VERSION):\s+[Bb]ump(?:ed)?.+$")?,
        ];

        Ok(CommitClassifier {
            add_patterns,
            fix_patterns,
            bump_patterns,
        })
    }

    /// Check kind of commit by looking at its message
    fn check_kind(&self, kind: CommitKind, message: &str) -> bool {
        let patterns = match kind {
            CommitKind::Addition => &self.add_patterns,
            CommitKind::Bump => &self.bump_patterns,
            CommitKind::Fix => &self.fix_patterns,
        };
        patterns.iter().any({ |p| p.is_match(message) })
    }

    /// Perform classification
    fn classify<'a>(&self, commits: &'a [Commit]) -> ClassifiedCommits<'a> {
        let (additions, others): (Vec<&'a Commit>, Vec<&'a Commit>) =
            commits.iter().partition(|&c| {
                c.brief()
                    .map_or(false, |m| self.check_kind(CommitKind::Addition, m))
            });
        let (fixes, others): (Vec<&'a Commit>, Vec<&'a Commit>) =
            others.iter().partition(|&c| {
                c.brief()
                    .map_or(false, |m| self.check_kind(CommitKind::Fix, m))
            });
        let (_, changes): (Vec<&'a Commit>, Vec<&'a Commit>) =
            others.iter().partition(|&c| {
                c.brief()
                    .map_or(false, |m| self.check_kind(CommitKind::Bump, m))
            });
        ClassifiedCommits {
            additions,
            changes,
            fixes,
        }
    }
}

#[derive(Debug)]
struct CommitShortener {
    bug_patterns: Vec<Regex>,
}

impl CommitShortener {
    /// Create a new shortener
    fn new() -> Result<Self> {
        let bug_patterns = vec![
            Regex::new(r"^Bug\s[\d]+:.*")?,
            Regex::new(r"^JIRA:\s[\w]+")?,
            Regex::new(r"^CS[\d]+")?,
        ];
        Ok(CommitShortener { bug_patterns })
    }

    /// Shorten commit message
    fn shorten(&self, commit: &Commit) -> Option<String> {
        let bugs: Vec<&str> = commit
            .message
            .lines()
            .filter(|l| self.bug_patterns.iter().any(|p| p.is_match(l)))
            .collect();
        let mut text = commit.brief()?.to_string();
        if !bugs.is_empty() {
            let mut extra = String::from(" (");
            extra.push_str(&bugs.join(","));
            extra.push(')');
            text.push_str(&extra);
        }
        Some(text)
    }
}

/// Create a new Markdown section
fn format_md_section(level: usize, title: &str, items: &[String]) -> String {
    if items.is_empty() {
        return "".to_string();
    }
    let mut text = "#".repeat(level);
    text.push(' ');
    text.push_str(title);
    text.push_str("\n\n");
    for item in items.iter() {
        text.push_str("- ");
        text.push_str(item);
        text.push('\n');
    }
    text.push('\n');
    text
}

#[derive(Debug)]
struct Formatter {
    shortener: CommitShortener,
}

impl Formatter {
    /// Create a new formatter, with a shortener
    fn new(shortener: CommitShortener) -> Self {
        Formatter { shortener }
    }

    /// Format commits as changelog snippet
    fn format(&self, commits: &ClassifiedCommits, tag: &str) -> String {
        let additions = self.shorten(&commits.additions);
        let changes = self.shorten(&commits.changes);
        let fixes = self.shorten(&commits.fixes);
        let timestamp: DateTime<Utc> = Utc::now();
        let mut text =
            format!("## [{}] - {}\n", tag, timestamp.format("%Y-%m-%d"));
        text.push_str(&format_md_section(3, "Added", &additions));
        text.push_str(&format_md_section(3, "Changed", &changes));
        text.push_str(&format_md_section(3, "Fixed", &fixes));
        text
    }

    fn shorten(&self, commits: &[&Commit]) -> Vec<String> {
        let mut commits: Vec<String> = commits
            .iter()
            .filter_map(|&c| self.shortener.shorten(c))
            .collect();
        commits.sort();
        commits
    }
}

/// Collect commits since `tag` in repository at `path`
fn collect_commits<P: AsRef<Path>>(path: P, tag: &str) -> Result<Vec<Commit>> {
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir")
        .arg(path.as_ref())
        .arg("log")
        .arg("--date=rfc2822")
        .arg("--no-merges")
        .arg("--invert-grep")
        .arg("--grep")
        .arg("^Squash")
        .arg(format!("{}..HEAD", tag));

    let text = run_command_or(&mut cmd, "git-log failed")?;
    let parser = CommitLogParser::new()?;
    let pattern = Regex::new(r"(?x)commit ")?;
    let commits = pattern
        .split(&text)
        .skip(1)
        .filter_map(|commit| parser.parse(commit))
        .collect();
    Ok(commits)
}

/// Find the latest annotated tag
fn find_latest_tag<P: AsRef<Path>>(path: P) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("--git-dir")
        .arg(path.as_ref())
        .arg("describe")
        .arg("--abbrev=0")
        .arg("--tags");
    run_command_or(&mut cmd, "git-describe failed")
}

/// Generate a changelog
fn generate_changelog<P: AsRef<Path>>(
    repository: P,
    old_tag: &str,
    new_tag: &str,
) -> Result<String> {
    let commits = collect_commits(repository, old_tag)?;
    let classifier = CommitClassifier::new()?;
    let commits = classifier.classify(&commits);
    let shortener = CommitShortener::new()?;
    let formatter = Formatter::new(shortener);
    let text = formatter.format(&commits, new_tag);
    Ok(text)
}

/// Update a changelog
fn update_changelog<P: AsRef<Path>>(
    changelog: P,
    text: &str,
    in_place: bool,
) -> Result<()> {
    let mut inserted = false;
    let pat = Regex::new(r"^##\s+\[[\w\-.]+\]\s+-\s+[\d]{4}-[\d]{2}-[\d]{2}$")?;
    let input = File::open(&changelog)?;
    let reader = BufReader::new(input);
    let mut tmp = OsString::from(&changelog.as_ref());
    tmp.push(".tmp");
    let mut writer: Box<dyn Write> = match in_place {
        true => {
            let file = File::create(&tmp)?;
            Box::new(file)
        }
        false => Box::new(stdout()),
    };
    for line in reader.lines() {
        let line = line?;
        if pat.is_match(&line) {
            if !inserted {
                write!(writer, "{}", text)?;
                inserted = true;
            }
        }
        write!(writer, "{}\n", line)?;
    }
    if in_place {
        rename(&tmp, &changelog)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let opts = NevezOptions::from_args();
    let cwd = std::env::current_dir()?;
    let repo = opts.repository.unwrap_or(cwd);
    let gitdir = repo.join(".git");
    let last_tag = find_latest_tag(&gitdir)?;
    let old_tag = opts.old_tag.unwrap_or(last_tag);
    let text = generate_changelog(gitdir, &old_tag, &opts.new_tag)?;
    let mut changelog = repo.clone();
    changelog.push(opts.changelog);
    update_changelog(changelog, &text, opts.in_place)
}
