//! `tape issue`: search, view, file, and comment on tape issues.

use anyhow::Result;
use clap::Subcommand;

#[derive(clap::Args)]
pub(crate) struct IssueArgs {
    #[command(subcommand)]
    pub(crate) command: IssueCommand,
}

#[derive(Subcommand)]
pub(crate) enum IssueCommand {
    /// Search Tape issues (`app:tape`); omit query to list recent.
    Search(IssueSearchArgs),
    /// Print one issue by number.
    View(IssueViewArgs),
    /// File a diagnostics-rich Tape issue.
    Create(IssueCreateArgs),
    /// Comment on an issue and ensure it is open.
    Comment(IssueCommentArgs),
}

#[derive(clap::Args)]
pub(crate) struct IssueSearchArgs {
    #[arg(value_name = "QUERY", num_args = 0..)]
    pub(crate) query: Vec<String>,
    #[arg(long, default_value = "open", value_parser = ["open", "closed", "all"])]
    pub(crate) state: String,
    #[arg(long, default_value_t = 20)]
    pub(crate) limit: u32,
}

#[derive(clap::Args)]
pub(crate) struct IssueViewArgs {
    pub(crate) number: u64,
}

#[derive(clap::Args)]
pub(crate) struct IssueCreateArgs {
    #[arg(short = 't', long)]
    pub(crate) title: Option<String>,
    #[arg(value_name = "MSG", num_args = 0..)]
    pub(crate) message: Vec<String>,
    #[arg(long)]
    pub(crate) url: Option<String>,
    #[arg(long)]
    pub(crate) repo: Option<String>,
    #[arg(long)]
    pub(crate) label: Vec<String>,
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[arg(short = 'y', long)]
    pub(crate) yes: bool,
}

#[derive(clap::Args)]
pub(crate) struct IssueCommentArgs {
    pub(crate) number: u64,
    #[arg(value_name = "MSG", num_args = 0..)]
    pub(crate) message: Vec<String>,
    #[arg(long)]
    pub(crate) repo: Option<String>,
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[arg(short = 'y', long)]
    pub(crate) yes: bool,
}

pub(crate) async fn issue(args: IssueArgs) -> Result<()> {
    match args.command {
        IssueCommand::Search(args) => {
            let query = (!args.query.is_empty()).then(|| args.query.join(" "));
            cli_std::issue::search(
                &crate::cli::TOOL,
                cli_std::issue::SearchOptions::default()
                    .with_query(query)
                    .with_state(args.state)
                    .with_limit(args.limit),
            )
            .await
        }
        IssueCommand::View(args) => cli_std::issue::view(&crate::cli::TOOL, args.number).await,
        IssueCommand::Create(args) => {
            let message = (!args.message.is_empty()).then(|| args.message.join(" "));
            let title = args.title.unwrap_or_else(|| {
                message
                    .as_deref()
                    .and_then(|msg| msg.lines().next())
                    .map(|head| format!("tape: {}", head.chars().take(72).collect::<String>()))
                    .unwrap_or_else(|| "tape: issue report".to_string())
            });
            cli_std::issue::create(
                &crate::cli::TOOL,
                cli_std::issue::CreateOptions::new(title)
                    .with_message(message)
                    .with_url(args.url)
                    .with_repo(args.repo)
                    .with_label(
                        std::iter::once("app:tape".to_string())
                            .chain(args.label)
                            .collect(),
                    )
                    .with_dry_run(args.dry_run)
                    .with_yes(args.yes),
            )
            .await
        }
        IssueCommand::Comment(args) => {
            let message = (!args.message.is_empty()).then(|| args.message.join(" "));
            let options = cli_std::issue::CommentOptions::try_new(args.number)?
                .with_message(message)
                .with_repo(args.repo)
                .with_dry_run(args.dry_run)
                .with_yes(args.yes);
            cli_std::issue::comment(&crate::cli::TOOL, options).await
        }
    }
}
