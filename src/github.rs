//! GitHub GraphQL client and the PR / check domain model.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

const API: &str = "https://api.github.com";
const MAX_PAGES: usize = 5;
/// How many recent successful runs of a workflow to sample for typical job durations.
const HISTORY_RUNS: usize = 5;
const HISTORY_TTL: Duration = Duration::from_secs(30 * 60);

const QUERY: &str = r#"
query($cursor: String) {
  viewer {
    login
    pullRequests(first: 50, states: OPEN, after: $cursor, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number
        title
        url
        isDraft
        repository { nameWithOwner isArchived }
        commits(last: 1) {
          nodes {
            commit {
              oid
              statusCheckRollup {
                contexts(first: 100) {
                  totalCount
                  checkRunCountsByState { state count }
                  statusContextCountsByState { state count }
                  nodes {
                    __typename
                    ... on CheckRun { name status conclusion startedAt completedAt }
                    ... on StatusContext { context state }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

// ---------------------------------------------------------------------------
// Domain model

/// Normalized state of a single check. Ordering is the display order in the details pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failed,
    Running,
    Pending,
    Passed,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Extra progress info, e.g. the current step of a running Actions job.
    pub detail: Option<String>,
    /// Typical duration of this job, from recent successful runs of the same workflow.
    pub expected: Option<chrono::Duration>,
}

impl Check {
    /// Run time so far (if running) or total run time (if finished).
    pub fn duration(&self) -> Option<chrono::Duration> {
        let start = self.started_at?;
        let end = self.completed_at.unwrap_or_else(Utc::now);
        Some(end - start)
    }

    /// For a running check with known history: (estimated fraction done, time remaining).
    /// Remaining goes negative once the job runs longer than usual.
    pub fn estimate(&self) -> Option<(f64, chrono::Duration)> {
        if self.state != CheckState::Running {
            return None;
        }
        let expected = self.expected.filter(|e| e.num_seconds() > 0)?;
        let elapsed = self.duration()?;
        Some((elapsed.num_seconds() as f64 / expected.num_seconds() as f64, expected - elapsed))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub passed: u32,
    pub failed: u32,
    pub running: u32,
    pub pending: u32,
    pub skipped: u32,
}

impl Counts {
    fn add(&mut self, state: CheckState, n: u32) {
        match state {
            CheckState::Passed => self.passed += n,
            CheckState::Failed => self.failed += n,
            CheckState::Running => self.running += n,
            CheckState::Pending => self.pending += n,
            CheckState::Skipped => self.skipped += n,
        }
    }

    pub fn total(&self) -> u32 {
        self.passed + self.failed + self.running + self.pending + self.skipped
    }

    pub fn finished(&self) -> u32 {
        self.passed + self.failed + self.skipped
    }

    pub fn overall(&self) -> Overall {
        if self.total() == 0 {
            Overall::NoChecks
        } else if self.failed > 0 {
            Overall::Failing
        } else if self.running + self.pending > 0 {
            Overall::Running
        } else {
            Overall::Passed
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overall {
    NoChecks,
    Running,
    Failing,
    Passed,
}

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub head_sha: String,
    pub counts: Counts,
    /// Individual checks, sorted failed → running → pending → passed → skipped.
    pub checks: Vec<Check>,
}

impl PullRequest {
    /// Time until the slowest running check is expected to finish (negative when overdue).
    pub fn eta(&self) -> Option<chrono::Duration> {
        self.checks.iter().filter_map(|c| c.estimate()).map(|(_, remaining)| remaining).max()
    }

    fn sort_checks(&mut self) {
        self.checks.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.name.cmp(&b.name)));
    }
}

#[derive(Debug)]
pub struct Snapshot {
    pub login: String,
    pub prs: Vec<PullRequest>,
    /// Non-fatal problems (e.g. orgs with SAML SSO the token isn't authorized for).
    pub warnings: Vec<String>,
    pub fetched_at: DateTime<Local>,
}

/// Maps CheckRun status/conclusion values, StatusContext states and (uppercased) Actions job values.
fn parse_state(s: &str) -> CheckState {
    match s {
        "SUCCESS" | "COMPLETED" => CheckState::Passed,
        "NEUTRAL" | "SKIPPED" => CheckState::Skipped,
        "IN_PROGRESS" => CheckState::Running,
        "QUEUED" | "PENDING" | "WAITING" | "REQUESTED" | "EXPECTED" => CheckState::Pending,
        // FAILURE, ERROR, CANCELLED, TIMED_OUT, ACTION_REQUIRED, STARTUP_FAILURE, STALE
        _ => CheckState::Failed,
    }
}

fn check_run_state(status: &str, conclusion: Option<&str>) -> CheckState {
    match status {
        "COMPLETED" => parse_state(conclusion.unwrap_or("SUCCESS")),
        other => parse_state(other),
    }
}

// ---------------------------------------------------------------------------
// Wire format: GraphQL

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<Data>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct Data {
    viewer: Viewer,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewer {
    login: String,
    pull_requests: PrConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrConnection {
    page_info: PageInfo,
    nodes: Vec<Option<PrNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrNode {
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    repository: Repository,
    commits: CommitConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name_with_owner: String,
    is_archived: bool,
}

#[derive(Deserialize)]
struct CommitConnection {
    nodes: Vec<Option<CommitNode>>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    oid: String,
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    contexts: Contexts,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Contexts {
    check_run_counts_by_state: Option<Vec<StateCount>>,
    status_context_counts_by_state: Option<Vec<StateCount>>,
    nodes: Vec<Option<ContextNode>>,
}

#[derive(Deserialize)]
struct StateCount {
    state: String,
    count: u32,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum ContextNode {
    CheckRun(CheckRunNode),
    StatusContext(StatusContextNode),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckRunNode {
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct StatusContextNode {
    context: String,
    state: String,
}

/// Converts a PR node. The flag is true when some check nodes came back null, which happens when
/// the token can't read check runs (fine-grained PATs can never be granted the Checks permission).
fn convert(node: PrNode) -> (PullRequest, bool) {
    let commit = node.commits.nodes.into_iter().flatten().next().map(|c| c.commit);
    let head_sha = commit.as_ref().map(|c| c.oid.clone()).unwrap_or_default();
    let rollup = commit.and_then(|c| c.status_check_rollup);

    let mut counts = Counts::default();
    let mut checks = Vec::new();
    let mut hidden = false;

    if let Some(rollup) = rollup {
        let ctx = rollup.contexts;
        hidden = ctx.nodes.iter().any(Option::is_none);
        checks = ctx
            .nodes
            .into_iter()
            .flatten()
            .map(|n| match n {
                ContextNode::CheckRun(r) => Check {
                    state: check_run_state(&r.status, r.conclusion.as_deref()),
                    name: r.name,
                    started_at: r.started_at,
                    completed_at: r.completed_at,
                    detail: None,
                    expected: None,
                },
                ContextNode::StatusContext(s) => Check {
                    state: parse_state(&s.state),
                    name: s.context,
                    started_at: None,
                    completed_at: None,
                    detail: None,
                    expected: None,
                },
            })
            .collect();

        // Counts-by-state cover every context, even beyond the first 100 nodes or forbidden ones.
        let by_state = ctx
            .check_run_counts_by_state
            .into_iter()
            .chain(ctx.status_context_counts_by_state)
            .flatten();
        for sc in by_state {
            counts.add(parse_state(&sc.state), sc.count);
        }
        if counts.total() == 0 {
            for c in &checks {
                counts.add(c.state, 1);
            }
        }
    }

    let mut pr = PullRequest {
        repo: node.repository.name_with_owner,
        number: node.number,
        title: node.title,
        url: node.url,
        is_draft: node.is_draft,
        head_sha,
        counts,
        checks,
    };
    pr.sort_checks();
    (pr, hidden)
}

// ---------------------------------------------------------------------------
// Wire format: REST (GitHub Actions), used when check runs are forbidden

#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    workflow_id: u64,
    status: Option<String>,
}

#[derive(Deserialize)]
struct JobsResponse {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
struct Job {
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    name: String,
    status: String,
}

impl From<Job> for Check {
    fn from(job: Job) -> Self {
        let state = check_run_state(&job.status.to_uppercase(), job.conclusion.map(|c| c.to_uppercase()).as_deref());
        let detail = match state {
            CheckState::Running if !job.steps.is_empty() => {
                let done = job.steps.iter().filter(|s| s.status == "completed").count();
                let current = job.steps.iter().find(|s| s.status == "in_progress");
                Some(match current {
                    Some(step) => format!("step {}/{}: {}", done + 1, job.steps.len(), step.name),
                    None => format!("step {}/{}", done, job.steps.len()),
                })
            }
            _ => None,
        };
        Check {
            name: job.name,
            state,
            started_at: if state == CheckState::Pending { None } else { job.started_at },
            completed_at: job.completed_at,
            detail,
            expected: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Client

pub struct Client {
    agent: ureq::Agent,
    token: String,
    /// Jobs of completed workflow runs, keyed by run id; they never change.
    run_cache: HashMap<u64, Vec<Check>>,
    /// Actions checks per commit whose CI has fully settled, so no REST calls are needed.
    settled_cache: HashMap<String, Vec<Check>>,
    /// Typical job durations per (repo, workflow id), refreshed after HISTORY_TTL.
    history_cache: HashMap<(String, u64), (Instant, HashMap<String, chrono::Duration>)>,
}

/// Copies Actions progress info onto the matching running check runs (same name).
fn enrich(checks: &mut [Check], jobs: Vec<Check>) {
    for job in jobs.into_iter().filter(|j| j.state == CheckState::Running) {
        if let Some(c) = checks.iter_mut().find(|c| c.state == CheckState::Running && c.name == job.name) {
            c.detail = job.detail;
            c.expected = job.expected;
        }
    }
}

impl Client {
    pub fn new(token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("github-pr-check/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent,
            token,
            run_cache: HashMap::new(),
            settled_cache: HashMap::new(),
            history_cache: HashMap::new(),
        }
    }

    pub fn fetch(&mut self) -> Result<Snapshot, String> {
        let mut login = String::new();
        let mut converted = Vec::new();
        let mut warnings = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let body = json!({ "query": QUERY, "variables": { "cursor": cursor } });
            let resp: GqlResponse = self.send(self.agent.post(&format!("{API}/graphql")), Some(body))?;
            let errors: Vec<String> = resp.errors.unwrap_or_default().into_iter().map(|e| e.message).collect();
            let Some(data) = resp.data else {
                return Err(if errors.is_empty() { "GitHub returned no data".into() } else { errors.join("; ") });
            };
            // Forbidden check runs are expected with fine-grained PATs and handled via the Actions API below.
            warnings.extend(errors.into_iter().filter(|e| !e.contains("not accessible by personal access token")));

            let viewer = data.viewer;
            login = viewer.login;
            let conn = viewer.pull_requests;
            // PRs in archived repos stay "open" forever but can't change, so they're just noise.
            converted.extend(conn.nodes.into_iter().flatten().filter(|n| !n.repository.is_archived).map(convert));
            if !conn.page_info.has_next_page {
                break;
            }
            cursor = conn.page_info.end_cursor;
        }

        let mut prs = Vec::with_capacity(converted.len());
        for (mut pr, hidden) in converted {
            let in_progress = pr.counts.running + pr.counts.pending > 0;
            if hidden {
                match self.actions_checks(&pr) {
                    Ok(jobs) => {
                        pr.checks.extend(jobs);
                        pr.sort_checks();
                    }
                    Err(e) => warnings.push(format!(
                        "{}#{}: check names hidden; the PAT can read neither check runs nor Actions jobs \
                         (grant 'Actions: Read-only' or use a classic PAT): {e}",
                        pr.repo, pr.number
                    )),
                }
            } else if in_progress {
                // Check runs are visible; Actions only adds step info and estimates, so failures are ignored.
                if let Ok(jobs) = self.actions_checks(&pr) {
                    enrich(&mut pr.checks, jobs);
                }
            }
            prs.push(pr);
        }

        warnings.dedup();
        Ok(Snapshot { login, prs, warnings, fetched_at: Local::now() })
    }

    /// Fetches GitHub Actions jobs for the PR's head commit as a stand-in for forbidden check runs.
    fn actions_checks(&mut self, pr: &PullRequest) -> Result<Vec<Check>, String> {
        if let Some(cached) = self.settled_cache.get(&pr.head_sha) {
            return Ok(cached.clone());
        }

        let (repo, sha) = (&pr.repo, &pr.head_sha);
        let runs: RunsResponse = self.get(&format!("/repos/{repo}/actions/runs?head_sha={sha}&per_page=100"))?;
        let mut checks = Vec::new();
        for run in runs.workflow_runs {
            let completed = run.status.as_deref() == Some("completed");
            let mut run_checks = self.run_jobs(repo, run.id, completed)?;
            if run_checks.iter().any(|c| c.state == CheckState::Running) {
                let typical = self.typical_durations(repo, run.workflow_id);
                for c in run_checks.iter_mut().filter(|c| c.state == CheckState::Running) {
                    c.expected = typical.get(&c.name).copied();
                }
            }
            checks.extend(run_checks);
        }

        let settled = pr.counts.running + pr.counts.pending == 0
            && checks.iter().all(|c| !matches!(c.state, CheckState::Running | CheckState::Pending));
        if settled {
            self.settled_cache.insert(sha.clone(), checks.clone());
        }
        Ok(checks)
    }

    /// Jobs of one workflow run; completed runs are cached since they never change.
    fn run_jobs(&mut self, repo: &str, run_id: u64, completed: bool) -> Result<Vec<Check>, String> {
        if let Some(cached) = self.run_cache.get(&run_id) {
            return Ok(cached.clone());
        }
        let jobs: JobsResponse = self.get(&format!("/repos/{repo}/actions/runs/{run_id}/jobs?per_page=100"))?;
        let checks: Vec<Check> = jobs.jobs.into_iter().map(Check::from).collect();
        if completed {
            self.run_cache.insert(run_id, checks.clone());
        }
        Ok(checks)
    }

    /// Median duration per job name over the workflow's recent successful runs.
    /// Failures are cached as "no history" too, so a missing permission doesn't cost calls every refresh.
    fn typical_durations(&mut self, repo: &str, workflow_id: u64) -> HashMap<String, chrono::Duration> {
        let key = (repo.to_string(), workflow_id);
        if let Some((fetched, typical)) = self.history_cache.get(&key) {
            if fetched.elapsed() < HISTORY_TTL {
                return typical.clone();
            }
        }

        let mut samples: HashMap<String, Vec<chrono::Duration>> = HashMap::new();
        let path = format!(
            "/repos/{repo}/actions/workflows/{workflow_id}/runs?status=success&exclude_pull_requests=true&per_page={HISTORY_RUNS}"
        );
        if let Ok(runs) = self.get::<RunsResponse>(&path) {
            for run in runs.workflow_runs {
                let Ok(jobs) = self.run_jobs(repo, run.id, true) else { continue };
                for job in jobs.into_iter().filter(|j| j.state == CheckState::Passed) {
                    if let Some(d) = job.duration() {
                        samples.entry(job.name).or_default().push(d);
                    }
                }
            }
        }

        let typical: HashMap<String, chrono::Duration> = samples
            .into_iter()
            .map(|(name, mut ds)| {
                ds.sort();
                (name, ds[ds.len() / 2])
            })
            .collect();
        self.history_cache.insert(key, (Instant::now(), typical.clone()));
        typical
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let req = self.agent.get(&format!("{API}{path}")).set("Accept", "application/vnd.github+json");
        self.send(req, None)
    }

    fn send<T: DeserializeOwned>(&self, req: ureq::Request, body: Option<serde_json::Value>) -> Result<T, String> {
        let req = req.set("Authorization", &format!("bearer {}", self.token));
        let result = match body {
            Some(body) => req.send_json(body),
            None => req.call(),
        };
        match result {
            Ok(resp) => resp.into_json().map_err(|e| format!("invalid response from GitHub: {e}")),
            Err(ureq::Error::Status(401, _)) => Err("401 Unauthorized: the PAT is invalid or expired".into()),
            Err(ureq::Error::Status(code, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                Err(format!("GitHub returned HTTP {code}: {}", text.chars().take(200).collect::<String>()))
            }
            Err(e) => Err(format!("network error: {e}")),
        }
    }
}
