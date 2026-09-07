//! Fetching, parsing and caching `robots.txt`, a port of `scrapling/spiders/robotstxt.py`.

use std::collections::HashMap;
use std::sync::Arc;

use robotstxt::DefaultMatcher;
use tokio::sync::Mutex;

use super::request::Request;
use super::session::SessionManager;

/// The user agent the crawl declares itself as when reading `robots.txt`.
const USER_AGENT: &str = "*";

/// The most of a `robots.txt` that is read. Google stops at 500 KiB and so does this, so a
/// server that answers `/robots.txt` with a huge file cannot make the crawl allocate for it.
const MAX_ROBOTS_BYTES: usize = 512 * 1024;

/// Fetches, parses and caches robots.txt per domain.
#[derive(Debug)]
pub struct RobotsManager {
    sessions: Arc<SessionManager>,
    sid: String,
    cache: Mutex<HashMap<String, Arc<String>>>,
}

impl RobotsManager {
    /// A manager that fetches robots.txt with the given session manager and session id.
    pub fn new(sessions: Arc<SessionManager>, sid: String) -> RobotsManager {
        RobotsManager {
            sessions,
            sid,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The robots.txt body for a URL's domain, fetching it once and caching it afterwards.
    /// A domain whose robots.txt is missing or unreadable caches an empty body, which allows
    /// everything.
    async fn body_for(&self, url: &str) -> Arc<String> {
        let (scheme, authority) = match ::url::Url::parse(url) {
            Ok(parsed) => {
                let mut authority = parsed.host_str().unwrap_or("").to_string();
                if let Some(port) = parsed.port() {
                    authority.push(':');
                    authority.push_str(&port.to_string());
                }
                (parsed.scheme().to_string(), authority)
            }
            Err(_) => (String::from("https"), String::new()),
        };

        if let Some(body) = self.cache.lock().await.get(&authority) {
            return Arc::clone(body);
        }

        if authority.is_empty() {
            // Nothing to ask; an unparsable URL is left to the fetcher to reject.
            let body = Arc::new(String::new());
            self.cache.lock().await.insert(authority, Arc::clone(&body));
            return body;
        }

        let robots_url = format!("{scheme}://{authority}/robots.txt");
        let mut content = String::new();
        match self
            .sessions
            .fetch(&Request::new(robots_url).sid(self.sid.clone()))
            .await
        {
            Ok(response) => {
                if response.status == 200 {
                    let body = &response.body[..response.body.len().min(MAX_ROBOTS_BYTES)];
                    if body.len() < response.body.len() {
                        tracing::warn!(
                            domain = %authority,
                            limit = MAX_ROBOTS_BYTES,
                            "robots.txt is longer than the limit; only the first part is read"
                        );
                    }
                    content = String::from_utf8_lossy(body).into_owned();
                }
            }
            Err(error) => {
                tracing::warn!(domain = %authority, %error, "failed to fetch robots.txt");
            }
        }

        let body = Arc::new(content);
        self.cache.lock().await.insert(authority, Arc::clone(&body));
        body
    }

    /// Whether `*` may fetch this URL; true when robots.txt is missing or unreadable.
    pub async fn can_fetch(&self, url: &str) -> bool {
        let body = self.body_for(url).await;
        if body.is_empty() {
            return true;
        }
        let mut matcher = DefaultMatcher::default();
        matcher.one_agent_allowed_by_robots(body.as_str(), USER_AGENT, url)
    }

    /// The `Crawl-delay` for `*` on this URL's domain, when it declares one.
    pub async fn crawl_delay(&self, url: &str) -> Option<f64> {
        let body = self.body_for(url).await;
        crawl_delay_for(body.as_str(), USER_AGENT)
    }

    /// Warm the cache for these domains, concurrently.
    pub async fn prefetch(&self, urls: &[String]) {
        if urls.is_empty() {
            return;
        }
        tracing::debug!(domains = urls.len(), "pre-fetching robots.txt");
        let fetches = urls.iter().map(|url| self.body_for(url));
        let _ = futures::future::join_all(fetches).await;
    }
}

/// Read the `Crawl-delay` of the group that applies to `user_agent`, falling back to the
/// wildcard group. `robotstxt` only answers allow/deny questions, so this walks the file.
fn crawl_delay_for(body: &str, user_agent: &str) -> Option<f64> {
    let wanted = user_agent.to_lowercase();
    let mut in_group = false;
    // A run of `User-agent` lines opens one group; the first directive closes the run.
    let mut collecting_agents = false;
    let mut delay: Option<f64> = None;

    for line in body.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let field = field.trim().to_lowercase();
        let value = value.trim();

        if field == "user-agent" {
            if !collecting_agents {
                in_group = false;
                collecting_agents = true;
            }
            if value.to_lowercase() == wanted {
                in_group = true;
            }
            continue;
        }

        collecting_agents = false;
        if in_group && field == "crawl-delay" {
            if let Ok(parsed) = value.parse::<f64>() {
                if parsed.is_finite() && parsed >= 0.0 {
                    delay = Some(match delay {
                        Some(current) => current.max(parsed),
                        None => parsed,
                    });
                }
            }
        }
    }

    delay
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROBOTS: &str = "User-agent: BadBot\nDisallow: /\nCrawl-delay: 99\n\n\
                          User-agent: *\nDisallow: /private\nCrawl-delay: 2.5\n";

    #[test]
    fn reads_the_wildcard_crawl_delay() {
        assert_eq!(crawl_delay_for(ROBOTS, "*"), Some(2.5));
        assert_eq!(crawl_delay_for(ROBOTS, "badbot"), Some(99.0));
        assert_eq!(crawl_delay_for("User-agent: *\nDisallow:\n", "*"), None);
        assert_eq!(crawl_delay_for("", "*"), None);
    }

    #[test]
    fn ignores_comments_and_unreadable_values() {
        let body = "User-agent: *  # everyone\nCrawl-delay: soon\n";
        assert_eq!(crawl_delay_for(body, "*"), None);
    }

    #[test]
    fn the_matcher_honours_disallow_rules() {
        let mut matcher = DefaultMatcher::default();
        assert!(!matcher.one_agent_allowed_by_robots(ROBOTS, "*", "http://e.com/private/x"));
        let mut matcher = DefaultMatcher::default();
        assert!(matcher.one_agent_allowed_by_robots(ROBOTS, "*", "http://e.com/public/x"));
    }
}
