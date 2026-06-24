use anyhow::Result;
use base64::Engine as _;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

pub struct GitHubIngestion {
    client: Client,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRepo {
    name: String,
    /// "owner/repo" — the authoritative path. The owner may be an org or another account, NOT the
    /// authenticated user (we list via /user/repos?visibility=all). Use this for clone URLs, never
    /// `<authenticated-login>/<name>`, which is wrong for org/collaborator repos.
    full_name: String,
    description: Option<String>,
    language: Option<String>,
    default_branch: String,
    #[serde(rename = "stargazers_count")]
    #[allow(dead_code)]
    // parsed from the API; reserved for repo-ranking in ingestion selection
    stars: u32,
    fork: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubContent {
    content: Option<String>,
    encoding: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GitHubPull {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubReview {
    user: GitHubUser,
    body: String,
    state: String,
}

#[derive(Debug, Clone)]
pub struct RepoSummary {
    pub name: String,
    /// "owner/repo" — authoritative clone path (owner may be an org/other account, not the user).
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub default_branch: String,
    pub readme: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrReview {
    pub repo: String,
    pub body: String,
    pub state: String,
}

/// Raw text extracted from a professional's GitHub profile.
/// This becomes the input for style_guide extraction.
#[derive(Debug, Default, Clone)]
pub struct GitHubProfile {
    pub username: String,
    pub repos: Vec<RepoSummary>,
    pub pr_reviews: Vec<PrReview>,
}

impl GitHubIngestion {
    pub fn new(token: Option<String>) -> Self {
        Self {
            client: Client::new(),
            token,
        }
    }

    /// Adds auth headers when a token is present.
    fn request(&self, url: &str) -> RequestBuilder {
        let req = self
            .client
            .get(url)
            .header("User-Agent", "flaborful-onboarding/1.0");
        match &self.token {
            Some(t) => req
                .header("Authorization", format!("Bearer {}", t))
                .header("Accept", "application/vnd.github+json"),
            None => req,
        }
    }

    /// Fetch the authenticated user's GitHub login (requires token).
    pub async fn fetch_github_login(&self) -> Result<String> {
        let user = self
            .request("https://api.github.com/user")
            .send()
            .await?
            .error_for_status()?
            .json::<GitHubUser>()
            .await?;
        Ok(user.login)
    }

    /// Fetch repos + README content. When `username` is None, resolves it via /user (requires token).
    pub async fn ingest(&self, username: Option<&str>) -> Result<GitHubProfile> {
        let resolved_username = match username {
            Some(u) => u.to_string(),
            None => self.fetch_github_login().await?,
        };

        tracing::info!("Fetching GitHub repos for user: {}", resolved_username);
        let repos = self.fetch_repos(&resolved_username).await?;
        tracing::info!("Fetched {} repos for {}", repos.len(), resolved_username);

        let mut summaries = Vec::new();
        for repo in repos.iter().filter(|r| !r.fork).take(10) {
            tracing::debug!("Fetching README for repo: {}", repo.name);
            let readme = self
                .fetch_readme(&resolved_username, &repo.name, &repo.default_branch)
                .await
                .ok()
                .flatten();
            tracing::debug!(
                "README for {}: {}",
                repo.name,
                if readme.is_some() {
                    "found"
                } else {
                    "not found"
                }
            );

            summaries.push(RepoSummary {
                name: repo.name.clone(),
                full_name: repo.full_name.clone(),
                description: repo.description.clone(),
                language: repo.language.clone(),
                default_branch: repo.default_branch.clone(),
                readme,
            });
        }

        // Fetch PR reviews from top 3 repos
        let mut all_reviews = Vec::new();
        for repo in summaries.iter().take(3) {
            let reviews = self
                .fetch_pr_reviews(&resolved_username, &repo.name)
                .await
                .unwrap_or_default();
            all_reviews.extend(reviews);
        }

        tracing::info!(
            "GitHub ingestion complete: {} repos, {} PR reviews for {}",
            summaries.len(),
            all_reviews.len(),
            resolved_username
        );
        Ok(GitHubProfile {
            username: resolved_username,
            repos: summaries,
            pr_reviews: all_reviews,
        })
    }

    async fn fetch_repos(&self, username: &str) -> Result<Vec<GitHubRepo>> {
        let url = if self.token.is_some() {
            "https://api.github.com/user/repos?visibility=all&sort=updated&per_page=30".to_string()
        } else {
            format!(
                "https://api.github.com/users/{}/repos?sort=updated&per_page=30",
                username
            )
        };
        let repos = self
            .request(&url)
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<GitHubRepo>>()
            .await?;
        Ok(repos)
    }

    /// Fetch PR reviews authored by this user across a repo. Returns up to 30 reviews.
    async fn fetch_pr_reviews(&self, username: &str, repo: &str) -> Result<Vec<PrReview>> {
        let pulls_url = format!(
            "https://api.github.com/repos/{}/{}/pulls?state=all&per_page=20",
            username, repo
        );
        let pulls: Vec<GitHubPull> = match self.request(&pulls_url).send().await {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(_) => return Ok(vec![]),
        };

        let mut reviews = Vec::new();
        for pull in pulls.iter().take(10) {
            let reviews_url = format!(
                "https://api.github.com/repos/{}/{}/pulls/{}/reviews",
                username, repo, pull.number
            );
            let raw: Vec<GitHubReview> = match self.request(&reviews_url).send().await {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(_) => continue,
            };
            for r in raw {
                if r.user.login == username && !r.body.trim().is_empty() {
                    reviews.push(PrReview {
                        repo: repo.to_string(),
                        body: r.body,
                        state: r.state,
                    });
                }
            }
        }
        Ok(reviews)
    }

    async fn fetch_readme(
        &self,
        username: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/README.md?ref={}",
            username, repo, branch
        );
        let content = self
            .request(&url)
            .send()
            .await?
            .error_for_status()?
            .json::<GitHubContent>()
            .await?;

        if let (Some(enc), Some(data)) = (content.encoding, content.content) {
            if enc == "base64" {
                let cleaned = data.replace('\n', "");
                let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes())?;
                return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
        Ok(None)
    }
}
