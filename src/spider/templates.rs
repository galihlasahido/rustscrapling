//! The `CrawlSpider` template. Rust has no class inheritance, so Python's `CrawlSpider`
//! becomes [`crawl_rules`], a helper a spider calls from its own `parse`.

use std::collections::HashSet;

use crate::response::{FollowOptions, Response};

use super::links::LinkExtractor;
use super::request::{Callback, Request};

/// One "extract these links and dispatch them" rule.
#[derive(Debug, Clone)]
pub struct CrawlRule {
    /// Which links to follow.
    pub link_extractor: LinkExtractor,
    /// Which callback handles them. Default [`Callback::Parse`].
    pub callback: Callback,
    /// Override the priority of the requests produced.
    pub priority: Option<i32>,
}

impl CrawlRule {
    /// A rule that follows the extractor's links into the spider's `parse`.
    pub fn new(link_extractor: LinkExtractor) -> CrawlRule {
        CrawlRule {
            link_extractor,
            callback: Callback::Parse,
            priority: None,
        }
    }

    /// Send the matched links to a named callback instead.
    pub fn callback(mut self, callback: Callback) -> Self {
        self.callback = callback;
        self
    }

    /// Give the produced requests a fixed priority.
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = Some(priority);
        self
    }
}

/// Apply every rule to a response and return the requests to follow, deduplicated in order.
///
/// Every request is built with [`Response::follow`], so it inherits the response's session id
/// and meta and carries a `referer` header pointing back at the page the link was found on,
/// exactly like the `response.follow(url, callback=rule.callback)` in Python's `CrawlSpider`.
/// A URL that cannot be turned into a request is skipped with a log line rather than dropping
/// the whole page's links.
pub fn crawl_rules(response: &Response, rules: &[CrawlRule]) -> Vec<Request> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Request> = Vec::new();

    for rule in rules {
        for url in rule.link_extractor.extract(response) {
            if !seen.insert(url.clone()) {
                continue;
            }

            let mut options = FollowOptions::default().callback(rule.callback.clone());
            if let Some(priority) = rule.priority {
                options = options.priority(priority);
            }

            match response.follow(&url, options) {
                Ok(request) => out.push(request),
                Err(error) => {
                    tracing::debug!(%url, %error, "skipping a link that could not be followed")
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><body>
        <a href="/products/1">one</a>
        <a href="/products/2">two</a>
        <a href="/about">about</a>
    </body></html>"#;

    fn page() -> Response {
        let mut response = Response::new("http://example.com/", PAGE.as_bytes().to_vec(), 200)
            .expect("a response");
        response.meta.insert(
            crate::response::META_SID.to_string(),
            serde_json::json!("default"),
        );
        response
            .meta
            .insert("depth".to_string(), serde_json::json!(1));
        response
    }

    #[test]
    fn a_rule_follows_its_links_into_a_named_callback() {
        let rules = vec![CrawlRule::new(
            LinkExtractor::new()
                .allow(&["/products/"])
                .expect("pattern"),
        )
        .callback(Callback::named("parse_product"))
        .priority(3)];

        let requests = crawl_rules(&page(), &rules);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].url, "http://example.com/products/1");
        assert_eq!(
            requests[0].callback,
            Callback::Named("parse_product".to_string())
        );
        assert_eq!(requests[0].priority, 3);
        assert_eq!(requests[0].sid, "default");
        assert_eq!(requests[0].meta.get("depth"), Some(&serde_json::json!(1)));
        assert_eq!(
            requests[0]
                .options
                .headers
                .get("referer")
                .map(String::as_str),
            Some("http://example.com/")
        );
    }

    #[test]
    fn rules_are_applied_in_order_and_deduplicated() {
        let rules = vec![
            CrawlRule::new(LinkExtractor::new().allow(&["/about"]).expect("pattern")),
            CrawlRule::new(LinkExtractor::new()),
        ];
        let requests = crawl_rules(&page(), &rules);
        let urls: Vec<String> = requests.iter().map(|request| request.url.clone()).collect();
        assert_eq!(
            urls,
            vec![
                "http://example.com/about".to_string(),
                "http://example.com/products/1".to_string(),
                "http://example.com/products/2".to_string(),
            ]
        );
    }

    #[test]
    fn no_rules_means_no_requests() {
        assert!(crawl_rules(&page(), &[]).is_empty());
    }
}
